mod workspace_form;
pub(super) use workspace_form::workspace_action;
mod setup_review;
pub(super) use setup_review::setup_action;
mod choice_dialogs;
mod close_workspace;
mod overlays;
mod search;
mod settings;
mod terminals;
mod workspace_controls;
mod worktree;
pub(super) use settings::settings_action;
pub(super) use terminals::TerminalControls;

use accesskit::{
    Action, ActionData, ActionRequest, Live, Node, NodeId, Role, TextPosition, TextSelection,
    TreeId, TreeInfo, TreeUpdate,
};
use daedalus_proto::{AgentState, PaneRole, PaneStatus};
use daedalus_ui::input::{UiAction, filter_modal_text};
use daedalus_ui::layout::Rect;
#[cfg(test)]
use daedalus_ui::layout::overlays::{worktree_input_rect, worktree_modal_rect};
use daedalus_ui::state::{ActiveOverlay, AppState, ConnectionState};
use unicode_segmentation::UnicodeSegmentation;

const WINDOW: NodeId = NodeId(1);
const STATUS: NodeId = NodeId(2);
const WORKSPACE: NodeId = NodeId(3);
use workspace_controls::OPEN_NEW_WORKSPACE;
const PICKER: NodeId = NodeId(7);
const ACTIONS: NodeId = NodeId(8);
const WORKTREE: NodeId = NodeId(9);
const CLOSE: NodeId = NodeId(10);
const SEARCH: NodeId = NodeId(11);

const SETUP_REVIEW: NodeId = NodeId(19);
const SETTINGS: NodeId = NodeId(14);
const OPEN_SETTINGS: NodeId = NodeId(17);
const TOGGLE_LAYOUT: NodeId = NodeId(20);
const OPEN_SESSIONS: NodeId = NodeId(21);
const NEW_WORKSPACE: NodeId = NodeId(15);
const VALIDATION: NodeId = NodeId(12);
const BRANCH_TEXT: NodeId = NodeId(13);

fn worktree_controls(state: &AppState) -> (NodeId, NodeId, NodeId) {
    // Queued platform actions from a prior dialog must not affect its successor.
    let first = 100 + state.worktree_modal.session() * 3;
    (NodeId(first), NodeId(first + 1), NodeId(first + 2))
}

pub(super) fn action(
    state: &AppState,
    request: &ActionRequest,
    controls: &TerminalControls,
) -> Option<UiAction> {
    if request.target_tree == TreeId::ROOT
        && request.action == Action::Click
        && state.active_overlay().is_none()
    {
        match request.target_node {
            TOGGLE_LAYOUT if state.active_workspace().is_some() => {
                return Some(UiAction::ToggleZoom);
            }
            OPEN_SESSIONS
                if state.connection == ConnectionState::Connected
                    && state.active_workspace().is_some() =>
            {
                return Some(UiAction::OpenSessions);
            }
            OPEN_SETTINGS if state.active_workspace().is_some() => {
                return Some(UiAction::WorkspaceSettings);
            }
            node if state.workspace_list_loaded => {
                if let Some(preset) = controls.cargo_preset(state, node) {
                    return Some(UiAction::RunCargo(preset));
                }
                if controls.is_run_tests(state, node) {
                    return Some(UiAction::RunTests);
                }
                if let Some(id) = controls.failed_run_workspace(state, node) {
                    return Some(UiAction::OpenFailedRun(id));
                }
                if let Some(action) = controls.sidebar_action(state, node) {
                    return Some(action);
                }
            }
            _ => {}
        }
    }
    if request.target_tree != TreeId::ROOT {
        return None;
    }
    let overlay = state.active_overlay();
    if overlay == Some(ActiveOverlay::Search) {
        return search::action(state, request, controls);
    }
    if overlay == Some(ActiveOverlay::CloseWorkspace) {
        return close_workspace::action(state, request, controls);
    }
    if overlay != Some(ActiveOverlay::Worktree) {
        return None;
    }
    let (branch, cancel, create) = worktree_controls(state);
    if request.target_node == branch && request.action == Action::SetValue {
        let Some(ActionData::Value(value)) = &request.data else {
            return None;
        };
        let text = if value.is_empty() {
            String::new()
        } else {
            filter_modal_text(value, false)?
        };
        return Some(UiAction::WorktreeModalSetText(text));
    }
    if request.action != Action::Click {
        return None;
    }
    if request.target_node == cancel {
        Some(UiAction::WorktreeModalCancel)
    } else if request.target_node == create && state.can_create_worktree() {
        Some(UiAction::WorktreeModalConfirm)
    } else {
        None
    }
}

fn named(role: Role, name: impl Into<String>) -> Node {
    let mut node = Node::new(role);
    node.set_label(name.into());
    node
}

fn overlay(state: &AppState) -> Option<(NodeId, Node)> {
    let overlay = state.active_overlay();
    let (id, label, description) = if overlay == Some(ActiveOverlay::FailureRecipients)
        && let Some(choices) = &state.failure_recipients
    {
        (
            NodeId(16),
            "Send failure to agent",
            format!(
                "Selected {}, {} of {}. Use Up and Down to choose a recipient, Enter to send, or Escape to cancel.",
                state.failure_recipient_label(choices.agents[choices.selected]),
                choices.selected + 1,
                choices.agents.len()
            ),
        )
    } else if overlay == Some(ActiveOverlay::SetupReview) {
        (
            SETUP_REVIEW,
            "Review environment",
            format!(
                "{} on {}. {}",
                state.setup_review.name,
                state.setup_review.host(),
                state.setup_review.detail()
            ),
        )
    } else if overlay == Some(ActiveOverlay::Settings) {
        (SETTINGS, "Workspace settings", state.settings.heading())
    } else if overlay == Some(ActiveOverlay::WorkspaceForm) {
        (
            NEW_WORKSPACE,
            "Set up workspace · Choose project",
            if state.workspace_form.submitting() {
                "Checking project on the daemon host. Escape dismisses this dialog.".into()
            } else if state.workspace_form.name_is_fixed() {
                "This folder uses the existing workspace name. Enter reviews the environment. Escape cancels.".into()
            } else {
                "Enter a project folder. The workspace name defaults to the folder name. Enter reviews the environment. Escape cancels.".into()
            },
        )
    } else if overlay == Some(ActiveOverlay::AgentPicker) {
        (
            PICKER,
            "Select agent preset",
            format!(
                "Selected {}. Use Up and Down to choose, Enter to switch, Shift+Enter to add, or Escape to cancel.",
                state.picker.selected_preset().label()
            ),
        )
    } else if overlay == Some(ActiveOverlay::ActionPalette) {
        let selection = state.active_actions().get(state.action_palette.selected_index).map_or_else(
            || "No actions configured for this workspace.".to_string(),
            |action| format!("Selected {}. Use Up and Down to choose, Enter to run, or Escape to cancel.", action.label),
        );
        if state.action_palette.session_actions.is_some() {
            (
                ACTIONS,
                "Workspace sessions",
                "Choose a session to launch or stop.".into(),
            )
        } else {
            (ACTIONS, "Action palette", selection)
        }
    } else if overlay == Some(ActiveOverlay::Worktree) {
        (
            WORKTREE,
            "New Git worktree",
            "Enter to create. Escape to cancel.".to_owned(),
        )
    } else if overlay == Some(ActiveOverlay::CloseWorkspace) {
        let modal = &state.close_modal;
        let mut detail = vec![modal.workspace_name.clone()];
        if modal.is_dirty {
            detail.push("Worktree has uncommitted changes".to_owned());
        }
        detail.push(if modal.submitting {
            "Closing workspace.".to_owned()
        } else {
            "Enter to confirm. Escape to cancel.".to_owned()
        });
        (CLOSE, "Close workspace", detail.join(". "))
    } else if let Some(search) = &state.search {
        let mut detail = if search.query.is_empty() {
            "Type a search query.".to_string()
        } else {
            format!("Query: {}.", search.query)
        };
        if let Some(result) = search.result_label() {
            detail.push_str(&format!(" {result}."));
        }
        let case = if search.case_sensitive { "on" } else { "off" };
        detail.push_str(&format!(" Match case {case}. F4 toggles match case. Enter for next match. Shift+Enter for previous match. Escape to close."));
        let mut node = named(Role::Search, "Search pane");
        node.set_description(detail);
        return Some((SEARCH, node));
    } else {
        return None;
    };
    let mut node = named(Role::Dialog, label);
    node.set_modal();
    node.set_description(description);
    Some((id, node))
}

fn bounds(rect: Rect) -> accesskit::Rect {
    accesskit::Rect {
        x0: rect.x.into(),
        y0: rect.y.into(),
        x1: rect.right().into(),
        y1: rect.bottom().into(),
    }
}

// The Linux adapter exposes EditableText when the input contains a TextRun.
fn text_run(text: &str) -> Node {
    let mut lengths = Vec::new();
    for grapheme in text.graphemes(true) {
        if let Ok(length) = u8::try_from(grapheme.len()) {
            lengths.push(length);
        } else {
            // Individual AccessKit lengths are bytes. Preserve all of an unusually
            // long combining sequence by splitting it at valid scalar boundaries.
            lengths.extend(grapheme.chars().map(|ch| ch.len_utf8() as u8));
        }
    }
    let mut run = Node::new(Role::TextRun);
    run.set_value(text);
    run.set_character_lengths(lengths);
    run
}

fn editable_field(
    label: &str,
    value: &str,
    run_id: NodeId,
    selection: std::ops::Range<usize>,
    nodes: &mut Vec<(NodeId, Node)>,
) -> Node {
    let run = text_run(value);
    let position = |byte| {
        let mut end = 0;
        let character_index = run
            .character_lengths()
            .iter()
            .take_while(|len| {
                end += usize::from(**len);
                end <= byte
            })
            .count();
        TextPosition {
            node: run_id,
            character_index,
        }
    };
    let mut node = named(Role::TextInput, label);
    node.set_required();
    node.set_text_selection(TextSelection {
        anchor: position(selection.start),
        focus: position(selection.end),
    });
    node.add_action(Action::SetValue);
    node.push_child(run_id);
    nodes.push((run_id, run));
    node
}

#[cfg(test)]
fn tree(state: &AppState, width: f32, height: f32, scale: f32) -> TreeUpdate {
    workbench_tree(
        state,
        width,
        height,
        scale,
        &mut TerminalControls::default(),
    )
}

#[cfg(test)]
pub(super) fn workbench_tree(
    state: &AppState,
    width: f32,
    height: f32,
    scale: f32,
    controls: &mut TerminalControls,
) -> TreeUpdate {
    workbench_tree_with_search_bar(state, width, height, scale, controls, None)
}

pub(super) fn workbench_tree_with_search_bar(
    state: &AppState,
    width: f32,
    height: f32,
    scale: f32,
    controls: &mut TerminalControls,
    search_bar: Option<Rect>,
) -> TreeUpdate {
    let mut root = named(Role::Window, "Daedalus Workbench");
    root.set_bounds(bounds(Rect::new(0.0, 0.0, width, height)));
    let connection = match state.connection {
        ConnectionState::Connecting => "Connecting to daemon",
        ConnectionState::Reconnecting => "Disconnected, reconnecting to daemon",
        ConnectionState::Connected => "Connected",
    };
    let mut status_text = connection.to_string();
    if let Some(host) = &state.remote_host {
        status_text.push_str(&format!(" ({host})"));
    }
    if let Some(message) = state.persistence_warning() {
        status_text.push_str(". ");
        status_text.push_str(message);
    }
    if let Some((message, _, _)) = &state.toast {
        status_text.push_str(". ");
        status_text.push_str(message);
    }
    if state.active_overlay().is_none()
        && let Some((title, hint)) = state.terminal_guidance(state.focused_role)
    {
        status_text.push_str(". ");
        status_text.push_str(title);
        if let Some(hint) = hint {
            status_text.push_str(". ");
            status_text.push_str(hint);
        }
    }
    let mut status = named(Role::Status, status_text);
    status.set_live(Live::Polite);
    root.push_child(STATUS);
    let mut nodes = vec![(STATUS, status)];
    let current_overlay = overlay(state);
    if state.workspace_list_loaded && current_overlay.is_none() {
        controls.append_sidebar(state, &mut root, &mut nodes);
        controls.append_run_tests(state, &mut root, &mut nodes);
        controls.append_widgets(state, &mut root, &mut nodes);
        controls.append_failed_runs(state, &mut root, &mut nodes);
    }
    let focus = if let Some((id, node)) = current_overlay {
        root.push_child(id);
        overlays::append(
            state,
            (id, node),
            &mut nodes,
            (width, height, scale),
            controls,
            search_bar,
        )
    } else if let Some(ws) = state.active_workspace() {
        let mut workspace = named(Role::Group, format!("Workspace {}", ws.name));
        let mut description = Vec::new();
        if let Some(repo) = &ws.project.repository {
            description.push(format!("Repository {}", repo.display()));
        }
        for row in daedalus_ui::sidebar_content::workspace_details(ws) {
            use daedalus_ui::sidebar_content::SidebarRow;
            match row {
                SidebarRow::Heading(text) | SidebarRow::Detail(text, None) => {
                    description.push(text)
                }
                SidebarRow::Detail(text, Some(level)) => {
                    description.push(format!("{text}, {level:?}"))
                }
                SidebarRow::Workspace(_) => {}
            }
        }
        description.extend(state.recap_rows(ws).into_iter().map(|row| row.label));
        workspace.set_description(description.join(". "));
        let mut open_settings = named(Role::Button, "Workspace settings");
        open_settings.add_action(Action::Click);
        let mut toggle_layout = named(
            Role::Button,
            if state.zoomed {
                "Split view"
            } else {
                "Focus view"
            },
        );
        toggle_layout.add_action(Action::Click);
        workspace.push_child(TOGGLE_LAYOUT);
        nodes.push((TOGGLE_LAYOUT, toggle_layout));
        controls.append_cargo(state, &mut workspace, &mut nodes);
        if state.connection == ConnectionState::Connected {
            let mut launch = named(Role::Button, "Launch or stop a session");
            launch.add_action(Action::Click);
            workspace.push_child(OPEN_SESSIONS);
            nodes.push((OPEN_SESSIONS, launch));
        }
        workspace.push_child(OPEN_SETTINGS);
        nodes.push((OPEN_SETTINGS, open_settings));
        root.push_child(WORKSPACE);
        let mut focus = OPEN_SETTINGS;
        if let Some((id, tab_label)) = controls.append(state, ws, &mut workspace, &mut nodes)
            && let Some(pane) = state.pane_for_role(state.focused_role)
        {
            let label = match state.focused_role {
                PaneRole::Agent => "Agent pane",
                PaneRole::Service => "Service pane",
                PaneRole::Runner => "Runner pane",
            };
            let status = match pane.summary.status {
                PaneStatus::Starting => "Starting".to_owned(),
                PaneStatus::Idle => "Idle".to_owned(),
                PaneStatus::Busy => "Busy".to_owned(),
                PaneStatus::Exited { code: Some(code) } => format!("Exited with code {code}"),
                PaneStatus::Exited { code: None } => "Exited".to_owned(),
            };
            let label = if state.session_names.contains_key(&pane.summary.id)
                || state
                    .workspace_panes(ws)
                    .filter(|p| p.role == pane.summary.role)
                    .count()
                    > 1
            {
                format!("{tab_label}, {label} in {}", ws.name)
            } else {
                format!("{label} in {}", ws.name)
            };
            let mut terminal = named(Role::Terminal, label);
            let description = match pane.summary.agent_state {
                Some(AgentState::WaitingForApproval) => {
                    format!("{status}. Agent waiting for approval")
                }
                Some(agent) => format!("{status}. Agent {agent}"),
                None => status,
            };
            terminal.set_description(description);
            workspace.push_child(id);
            nodes.push((id, terminal));
            focus = id;
        }
        nodes.push((WORKSPACE, workspace));
        focus
    } else if state.workspace_list_loaded {
        OPEN_NEW_WORKSPACE
    } else {
        WINDOW
    };
    nodes.push((WINDOW, root));
    TreeUpdate {
        nodes,
        tree: Some(TreeInfo::new(WINDOW)),
        tree_id: TreeId::ROOT,
        focus,
    }
}

#[cfg(test)]
mod tests {
    use super::workspace_form::{control_node, form_node};
    use super::*;
    use accesskit::Invalid;
    use daedalus_ui::assignments::CargoPreset;
    use daedalus_ui::workspace_form::{Control, WorkspaceFormAction};
    fn action(state: &AppState, request: &ActionRequest) -> Option<UiAction> {
        super::action(state, request, &TerminalControls::default())
    }

    fn tree(state: &AppState) -> TreeUpdate {
        super::tree(state, 1200.0, 800.0, 1.0)
    }

    #[test]
    fn initial_tree_names_window_and_connection() {
        let t = tree(&AppState::new());
        assert_eq!(t.focus, WINDOW);
        let status = &t.nodes.iter().find(|n| n.0 == STATUS).unwrap().1;
        assert_eq!(status.label(), Some("Connecting to daemon"));
        assert_eq!(status.live(), Some(Live::Polite));
    }

    #[test]
    fn rejected_snapshot_warning_is_announced_in_the_live_status() {
        let mut state = AppState::new();
        state.connection = ConnectionState::Connected;
        state.persistence_health =
            Some(daedalus_proto::PersistenceHealth::TemporarySnapshotRejected);
        let update = tree(&state);
        let status = &update.nodes.iter().find(|node| node.0 == STATUS).unwrap().1;
        assert!(
            status
                .label()
                .unwrap()
                .contains("Workspace changes are temporary")
        );
        assert_eq!(status.live(), Some(Live::Polite));
    }

    #[test]
    fn empty_workspace_guidance_is_announced_until_the_creation_form_opens() {
        let mut state = AppState::new();
        state.connection = ConnectionState::Connected;
        state.set_workspaces(Vec::new());
        let status_label = |state: &AppState| {
            let update = tree(state);
            let status = update.nodes.iter().find(|(id, _)| *id == STATUS).unwrap();
            status.1.label().unwrap().to_owned()
        };
        assert_eq!(
            status_label(&state),
            "Connected. No workspaces open. Choose Workspaces → + Workspace."
        );
        state.workspace_form.begin(std::path::Path::new("/tmp"));
        assert_eq!(status_label(&state), "Connected");
    }
    #[test]
    fn empty_workbench_offers_workspace_creation_and_hides_it_while_editing() {
        let mut state = AppState::new();
        state.set_workspaces(Vec::new());
        let update = tree(&state);
        assert_eq!(update.focus, OPEN_NEW_WORKSPACE);
        let button = &update.nodes.iter().find(|n| n.0 == update.focus).unwrap().1;
        assert_eq!(button.label(), Some("New workspace"));
        assert!(button.supports_action(Action::Click));
        let request = ActionRequest {
            target_tree: TreeId::ROOT,
            target_node: OPEN_NEW_WORKSPACE,
            action: Action::Click,
            data: None,
        };
        assert_eq!(action(&state, &request), Some(UiAction::NewWorkspace));
        state.workspace_form.begin(std::path::Path::new("."));
        assert!(tree(&state).nodes.iter().all(|n| n.0 != OPEN_NEW_WORKSPACE));
        assert_eq!(action(&state, &request), None);
    }

    #[test]
    fn sidebar_opens_worktree_and_workspace_specific_close_dialogs() {
        let mut state = agent_workspace();
        let id = state.active_workspace_id.unwrap();
        let mut controls = TerminalControls::default();
        let tree = workbench_tree(&state, 1200., 800., 1., &mut controls);
        let click = |label: &str| {
            let (node, control) = tree
                .nodes
                .iter()
                .find(|(_, node)| node.label() == Some(label))
                .unwrap();
            assert!(control.supports_action(Action::Click));
            ActionRequest {
                target_tree: TreeId::ROOT,
                target_node: *node,
                action: Action::Click,
                data: None,
            }
        };
        let worktree = click("New Git worktree");
        let close = click("Close Project");
        assert_eq!(
            super::action(&state, &worktree, &controls),
            Some(UiAction::ToggleWorktreeModal)
        );
        assert_eq!(
            super::action(&state, &close, &controls),
            Some(UiAction::CloseWorkspaceFromSidebar(id))
        );
        state.open_worktree_modal();
        assert_eq!(super::action(&state, &close, &controls), None);
        state.worktree_modal.close();
        state.set_workspaces(Vec::new());
        assert_eq!(super::action(&state, &close, &controls), None);
    }

    #[test]
    fn modal_reports_current_selection_and_restores_focus() {
        let mut state = AppState::new();
        state.picker.toggle();
        let first = tree(&state);
        state.picker.move_down();
        let second = tree(&state);
        // Native focus follows the selected preset row.
        for tree in [&first, &second] {
            let (_, focused) = tree.nodes.iter().find(|(id, _)| *id == tree.focus).unwrap();
            assert_eq!(focused.role(), Role::Button);
            assert!(focused.supports_action(Action::Click));
        }
        assert_ne!(first.focus, second.focus);
        assert_ne!(first, second);
        let dialog = &second.nodes.iter().find(|n| n.0 == PICKER).unwrap().1;
        assert!(dialog.is_modal());
        assert!(
            dialog
                .description()
                .unwrap()
                .contains(state.picker.selected_preset().label())
        );
        state.picker.close();
        assert_eq!(tree(&state).focus, WINDOW);
    }

    #[test]
    fn native_preset_actions_target_rows_and_reject_a_previous_dialog() {
        use daedalus_ui::state::AgentPreset;

        let mut state = agent_workspace();
        let mut controls = TerminalControls::default();
        state.open_picker_with(AgentPreset::Shell);
        let tree = workbench_tree(&state, 1000., 680., 1., &mut controls);
        let row = tree
            .nodes
            .iter()
            .find(|(_, node)| {
                node.role() == Role::Button && node.label() == Some(AgentPreset::Codex.label())
            })
            .unwrap()
            .0;
        let request = |action| ActionRequest {
            action,
            target_tree: TreeId::ROOT,
            target_node: row,
            data: None,
        };
        assert_eq!(
            controls.choice_action(&mut state, &request(Action::Focus)),
            Some(UiAction::Ignore)
        );
        assert_eq!(state.picker.selected_preset(), AgentPreset::Codex);
        assert_eq!(
            workbench_tree(&state, 1000., 680., 1., &mut controls).focus,
            row
        );
        assert_eq!(
            controls.choice_action(&mut state, &request(Action::Click)),
            Some(UiAction::PickerConfirm)
        );
        state.picker.close();
        state.open_picker_with(AgentPreset::Shell);
        assert_eq!(
            controls.choice_action(&mut state, &request(Action::Click)),
            None
        );
        assert_eq!(state.picker.selected_preset(), AgentPreset::Shell);
    }

    #[test]
    fn native_action_palette_can_reveal_a_row_and_reject_changed_actions() {
        use daedalus_proto::{CustomAction, WorkspaceManifest};

        let mut state = agent_workspace();
        state.active_workspace_mut().unwrap().manifest = Some(WorkspaceManifest {
            actions: (0..8)
                .map(|index| CustomAction {
                    id: format!("step-{index}"),
                    label: format!("Step {index}"),
                    target: PaneRole::Runner,
                    command: format!("step-{index}"),
                    shortcut: None,
                })
                .collect(),
            ..Default::default()
        });
        state.open_action_palette();
        let mut controls = TerminalControls::default();
        let tree = workbench_tree(&state, 420., 168., 1., &mut controls);
        let (row, node) = tree
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("Step 7"))
            .unwrap();
        assert!(node.bounds().is_none());
        let request = |action| ActionRequest {
            action,
            target_tree: TreeId::ROOT,
            target_node: *row,
            data: None,
        };
        assert_eq!(
            controls.choice_action(&mut state, &request(Action::Focus)),
            Some(UiAction::Ignore)
        );
        let revealed = workbench_tree(&state, 420., 168., 1., &mut controls);
        assert_eq!(revealed.focus, *row);
        assert!(
            revealed
                .nodes
                .iter()
                .find(|(id, _)| id == row)
                .unwrap()
                .1
                .bounds()
                .is_some()
        );
        assert_eq!(
            controls.choice_action(&mut state, &request(Action::Click)),
            Some(UiAction::ActionPaletteConfirm)
        );
        state
            .active_workspace_mut()
            .unwrap()
            .manifest
            .as_mut()
            .unwrap()
            .actions[7]
            .command = "updated".into();
        assert_eq!(
            controls.choice_action(&mut state, &request(Action::Click)),
            None
        );
    }
    #[test]
    fn search_description_exposes_case_and_navigation_keys() {
        let mut state = AppState::new();
        state.toggle_search(daedalus_proto::PaneId::new());
        for expected in ["Match case off", "Match case on"] {
            let update = tree(&state);
            assert_eq!(update.focus, SEARCH);
            let search = &update
                .nodes
                .iter()
                .find(|entry| entry.0 == SEARCH)
                .unwrap()
                .1;
            let detail = search.description().unwrap();
            assert!(detail.contains(expected));
            assert!(detail.contains("F4"));
            assert!(detail.contains("Shift+Enter"));
            assert!(search.children().iter().any(|id| {
                update
                    .nodes
                    .iter()
                    .any(|(node_id, node)| node_id == id && node.label() == Some("Match case"))
            }));
            state.search.as_mut().unwrap().toggle_case();
        }
    }

    #[test]
    fn search_controls_follow_results_and_reject_clicks_from_prior_session() {
        let mut state = agent_workspace();
        let pane = state.focused_pane_id().unwrap();
        state.toggle_search(pane);
        let bar = Rect::new(330., 39., 340., 58.);
        let mut controls = TerminalControls::default();
        let render = |state: &AppState, controls: &mut TerminalControls| {
            workbench_tree_with_search_bar(state, 1000., 600., 1., controls, Some(bar))
        };
        let click = |target_node| ActionRequest {
            action: Action::Click,
            target_node,
            target_tree: TreeId::ROOT,
            data: None,
        };
        let first = render(&state, &mut controls);
        let group = &first
            .nodes
            .iter()
            .find(|entry| entry.0 == SEARCH)
            .unwrap()
            .1;
        let [prev, next, case] = group.children() else {
            panic!("expected two navigation buttons and a match-case toggle");
        };
        let expected_rects = daedalus_ui::layout::overlays::search_controls(bar);
        for (id, rect) in group.children().iter().zip(expected_rects) {
            let node = &first.nodes.iter().find(|entry| entry.0 == *id).unwrap().1;
            assert_eq!(node.bounds(), Some(bounds(rect)));
            assert_eq!(node.is_disabled(), id != case);
        }
        assert_eq!(super::action(&state, &click(*prev), &controls), None);
        assert_eq!(super::action(&state, &click(*next), &controls), None);
        assert_eq!(
            super::action(&state, &click(*case), &controls),
            Some(UiAction::SearchToggleCase)
        );

        let search = state.search.as_mut().unwrap();
        search.toggle_case();
        search.push_text("find");
        search.set_results(
            0,
            vec![daedalus_proto::SearchMatch {
                abs_row: 0,
                start_col: 0,
                end_col: 4,
            }],
        );
        let populated = render(&state, &mut controls);
        let toggle = &populated
            .nodes
            .iter()
            .find(|entry| entry.0 == *case)
            .unwrap()
            .1;
        assert_eq!(toggle.role(), Role::CheckBox);
        assert_eq!(toggle.toggled(), Some(accesskit::Toggled::True));
        assert_eq!(
            super::action(&state, &click(*prev), &controls),
            Some(UiAction::SearchPrev)
        );
        assert_eq!(
            super::action(&state, &click(*next), &controls),
            Some(UiAction::SearchNext)
        );

        state.close_search();
        state.toggle_search(pane);
        assert_eq!(super::action(&state, &click(*case), &controls), None);
        render(&state, &mut controls);
        assert_eq!(super::action(&state, &click(*case), &controls), None);
    }

    #[test]
    fn worktree_controls_track_availability_validation_and_dialog_session() {
        let mut state = AppState::new();
        state.worktree_modal.open();
        state.connection = ConnectionState::Connected;
        let (_, cancel, create) = worktree_controls(&state);
        let click = |target_node| ActionRequest {
            action: Action::Click,
            target_node,
            target_tree: TreeId::ROOT,
            data: None,
        };
        let disabled = tree(&state);
        let button = &disabled.nodes.iter().find(|n| n.0 == create).unwrap().1;
        assert!(button.is_disabled());
        assert!(!button.supports_action(Action::Click));
        assert_eq!(action(&state, &click(create)), None);
        assert_eq!(
            action(&state, &click(cancel)),
            Some(UiAction::WorktreeModalCancel)
        );
        assert!(state.worktree_modal.branch_for_submit().is_none());
        let invalid = tree(&state);
        let alert = &invalid.nodes.iter().find(|n| n.0 == VALIDATION).unwrap().1;
        assert_eq!(alert.label(), Some("Enter a branch name"));
        assert_eq!(alert.live(), Some(Live::Assertive));
        state.worktree_modal.push_text("feature");
        let enabled = tree(&state);
        assert!(enabled.nodes.iter().all(|n| n.0 != VALIDATION));
        let button = &enabled.nodes.iter().find(|n| n.0 == create).unwrap().1;
        assert!(button.supports_action(Action::Click));
        assert!(!button.is_disabled());
        assert_eq!(
            action(&state, &click(create)),
            Some(UiAction::WorktreeModalConfirm)
        );
        state.connection = ConnectionState::Reconnecting;
        assert_eq!(action(&state, &click(create)), None);
        state.worktree_modal.close();
        assert_eq!(action(&state, &click(cancel)), None);
        state.worktree_modal.open();
        assert_eq!(action(&state, &click(cancel)), None);
    }
    #[test]
    fn branch_field_tracks_focus_value_bounds_and_validation() {
        let mut state = AppState::new();
        state.worktree_modal.open();
        state.worktree_modal.push_text("feat/café");
        let branch = worktree_controls(&state).0;
        let update = super::tree(&state, 320.0, 480.0, 1.5);
        assert_eq!(update.focus, branch);
        let field = &update.nodes.iter().find(|n| n.0 == branch).unwrap().1;
        assert_eq!(field.role(), Role::TextInput);
        assert_eq!(field.label(), Some("Branch name"));
        assert_eq!(field.children(), &[BRANCH_TEXT]);
        let run = &update.nodes.iter().find(|n| n.0 == BRANCH_TEXT).unwrap().1;
        assert_eq!(run.value(), Some("feat/café"));
        assert_eq!(field.text_selection().unwrap().focus.character_index, 9);
        assert!(field.is_required() && field.supports_action(Action::SetValue));
        assert_eq!(
            field.bounds(),
            Some(bounds(worktree_input_rect(
                worktree_modal_rect(320.0, 480.0, 1.5),
                1.5,
            )))
        );
        state.worktree_modal.set_text(String::new());
        assert!(state.worktree_modal.branch_for_submit().is_none());
        let invalid = tree(&state);
        let field = &invalid.nodes.iter().find(|n| n.0 == branch).unwrap().1;
        assert_eq!(field.invalid(), Some(Invalid::True));
        assert_eq!(field.error_message(), Some(VALIDATION));
        state.worktree_modal.set_text("new".to_owned());
        let valid = tree(&state);
        let field = &valid.nodes.iter().find(|n| n.0 == branch).unwrap().1;
        let run = &valid.nodes.iter().find(|n| n.0 == BRANCH_TEXT).unwrap().1;
        assert_eq!(run.value(), Some("new"));
        assert_eq!(field.invalid(), None);
        assert_eq!(field.error_message(), None);
    }

    #[test]
    fn branch_text_ranges_cover_combining_sequences_and_emoji() {
        assert_eq!(text_run("e\u{301}/🌱").character_lengths(), &[3, 1, 4]);
        let text = format!("e{}", "\u{301}".repeat(150));
        let run = text_run(&text);
        let mut offset = 0;
        for length in run.character_lengths() {
            offset += usize::from(*length);
            assert!(text.is_char_boundary(offset));
        }
        assert_eq!(offset, text.len());
    }

    #[test]
    fn branch_replacement_uses_keyboard_filter_and_rejects_stale_requests() {
        let mut state = AppState::new();
        state.worktree_modal.open();
        let branch = worktree_controls(&state).0;
        let replace = |value: &str| ActionRequest {
            action: Action::SetValue,
            target_tree: TreeId::ROOT,
            target_node: branch,
            data: Some(ActionData::Value(value.into())),
        };
        assert_eq!(
            action(&state, &replace("feat/ 日本語\n")),
            Some(UiAction::WorktreeModalSetText("feat/日本語".into()))
        );
        assert_eq!(
            action(&state, &replace("")),
            Some(UiAction::WorktreeModalSetText(String::new()))
        );
        assert_eq!(action(&state, &replace(" \n")), None);
        assert_eq!(
            action(
                &state,
                &ActionRequest {
                    data: None,
                    ..replace("bad")
                }
            ),
            None
        );
        state.worktree_modal.close();
        assert_eq!(action(&state, &replace("late")), None);
        state.worktree_modal.open();
        assert_eq!(action(&state, &replace("late")), None);
    }

    #[test]
    fn workspace_form_exposes_editable_controls_matching_keyboard_focus_and_bounds() {
        let mut state = AppState::new();
        state.workspace_form.begin(std::path::Path::new("/日本語"));
        let modal = state.workspace_form.modal_rect(320., 480., 1.);
        let update = super::tree(&state, 320., 480., 1.);
        assert_eq!(update.focus, form_node(&state.workspace_form, 0));
        for (control, rect) in state.workspace_form.controls(modal) {
            let id = control_node(&state.workspace_form, control);
            let node = &update.nodes.iter().find(|(n, _)| *n == id).unwrap().1;
            assert_eq!(node.bounds(), Some(bounds(rect)));
            assert!(node.supports_action(Action::Focus));
            if state.workspace_form.value(control).is_some() {
                assert_eq!(node.is_required(), control == Control::Folder);
                assert!(node.supports_action(Action::SetValue));
            } else {
                assert!(node.supports_action(Action::Click));
            }
        }
        let folder = form_node(&state.workspace_form, 1);
        let request = ActionRequest {
            action: Action::Focus,
            target_node: folder,
            target_tree: TreeId::ROOT,
            data: None,
        };
        assert_eq!(
            workspace_action(&mut state, &request),
            Some(WorkspaceFormAction::None)
        );
        let update = tree(&state);
        assert_eq!(update.focus, folder);
        let node = &update.nodes.iter().find(|n| n.0 == folder).unwrap().1;
        let selection = node.text_selection().unwrap();
        assert_eq!(selection.anchor.character_index, 0);
        assert_eq!(selection.focus.character_index, 4);
        assert_eq!(
            update
                .nodes
                .iter()
                .find(|n| n.0 == selection.focus.node)
                .unwrap()
                .1
                .value(),
            Some("/日本語")
        );
    }

    #[test]
    fn workspace_form_validation_submission_and_stale_actions_match_visible_dialog() {
        let mut state = AppState::new();
        state.workspace_form.begin(std::path::Path::new(""));
        let request = |target_node, action, data| ActionRequest {
            target_tree: TreeId::ROOT,
            target_node,
            action,
            data,
        };
        let save = request(form_node(&state.workspace_form, 3), Action::Click, None);
        assert_eq!(
            workspace_action(&mut state, &save),
            Some(WorkspaceFormAction::None)
        );
        assert!(
            tree(&state)
                .nodes
                .iter()
                .any(|(_, n)| n.role() == Role::Alert && n.live() == Some(Live::Assertive))
        );
        let update = tree(&state);
        let field = &update.nodes.iter().find(|n| n.0 == update.focus).unwrap().1;
        assert_eq!(field.invalid(), Some(Invalid::True));
        assert_eq!(
            field.error_message(),
            Some(form_node(&state.workspace_form, 6))
        );
        let folder = request(
            form_node(&state.workspace_form, 1),
            Action::SetValue,
            Some(ActionData::Value("/repo/日本語 work".into())),
        );
        assert_eq!(
            workspace_action(&mut state, &folder),
            Some(WorkspaceFormAction::None)
        );
        assert_eq!(
            workspace_action(&mut state, &save),
            Some(WorkspaceFormAction::Save {
                name: "日本語 work".into(),
                folder: "/repo/日本語 work".into()
            })
        );
        state.workspace_form.begin(std::path::Path::new("/second"));
        assert_eq!(workspace_action(&mut state, &save), None);
        assert_eq!(workspace_action(&mut state, &folder), None);
    }

    #[test]
    fn recent_projects_are_a_list_whose_items_reopen_on_click() {
        let mut state = AppState::new();
        state
            .workspace_form
            .begin(std::path::Path::new("/home/dev"));
        let project = |name: &str, root: &str| daedalus_proto::RecentProject {
            name: name.into(),
            root_dir: root.into(),
            last_used: 0,
            has_setup: true,
        };
        let projects = vec![
            project("daedalus", "/home/tim/daedalus"),
            project("notes", "/home/tim/notes"),
        ];
        state.workspace_form.set_recent(projects.clone());
        let update = super::tree(&state, 800., 600., 1.);
        let node = |id: NodeId| &update.nodes.iter().find(|(n, _)| *n == id).unwrap().1;
        let (list_id, list) = update
            .nodes
            .iter()
            .find(|(_, n)| n.role() == Role::List)
            .expect("recent projects list");
        assert_eq!(list.label(), Some("Recent projects"));
        assert!(
            update
                .nodes
                .iter()
                .any(|(_, n)| n.children().contains(list_id))
        );
        let items = list.children().to_vec();
        assert_eq!(items.len(), 2);
        assert_eq!(node(items[0]).role(), Role::ListItem);
        assert_eq!(node(items[0]).label(), Some("daedalus, /home/tim/daedalus"));
        assert_eq!(node(items[1]).label(), Some("notes, /home/tim/notes"));
        let modal = state.workspace_form.modal_rect(800., 600., 1.);
        assert_eq!(
            node(items[1]).bounds(),
            Some(bounds(state.workspace_form.controls(modal)[1].1))
        );
        assert!(node(items[1]).supports_action(Action::Click));
        let request = |target_node, action| ActionRequest {
            target_tree: TreeId::ROOT,
            target_node,
            action,
            data: None,
        };
        assert_eq!(
            workspace_action(&mut state, &request(items[1], Action::Focus)),
            Some(WorkspaceFormAction::None)
        );
        assert_eq!(tree(&state).focus, items[1]);
        state
            .workspace_form
            .set_recent(vec![projects[1].clone(), projects[0].clone()]);
        assert_eq!(tree(&state).focus, items[1]);
        assert_eq!(
            workspace_action(&mut state, &request(items[1], Action::Click)),
            Some(WorkspaceFormAction::Reopen(projects[1].clone()))
        );
        state.workspace_form.set_recent(vec![projects[0].clone()]);
        assert_eq!(
            workspace_action(&mut state, &request(items[1], Action::Click)),
            None
        );
        state.workspace_form.set_recent(projects.clone());
        assert_eq!(
            workspace_action(&mut state, &request(items[1], Action::Click)),
            None
        );
        state.workspace_form.start_submission();
        let update = tree(&state);
        let busy = &update.nodes.iter().find(|(n, _)| *n == items[0]).unwrap().1;
        assert!(busy.is_disabled() && !busy.supports_action(Action::Click));
        assert_eq!(
            workspace_action(&mut state, &request(items[0], Action::Click)),
            None
        );
        state.workspace_form.set_recent(Vec::new());
        assert!(
            tree(&state)
                .nodes
                .iter()
                .all(|(_, n)| n.role() != Role::List)
        );
    }

    fn agent_workspace() -> AppState {
        use daedalus_proto::{PaneId, PaneSummary, WorkspaceId, WorkspaceInfo};
        let mut state = AppState::new();
        let ws = WorkspaceId::new();
        let pane = PaneSummary {
            id: PaneId::new(),
            workspace_id: ws,
            role: PaneRole::Agent,
            status: PaneStatus::Busy,
            cols: 80,
            rows: 24,
            cwd: None,
            last_exit_code: None,
            has_failure_trace: false,
            launch_preset: None,
            default_shell_fallback: None,
            agent_state: Some(AgentState::WaitingForApproval),
        };
        state.set_workspaces(vec![WorkspaceInfo {
            project: Default::default(),
            setup: None,
            default_panes_pending: None,
            id: ws,
            name: "Project".into(),
            root_dir: "/project".into(),
            slot_index: 0,
            injected_env: Default::default(),
            panes: vec![pane],
            port_offset: 0,
            compose_project_name: None,
            git_branch: None,
            git_status: None,
            bound_ports: vec![],
            containers: vec![],
            external_statuses: vec![],
            manifest: None,
        }]);
        state.active_workspace_id = Some(ws);
        let pane_id = state.focused_pane_id().unwrap();
        state.session_names.insert(pane_id, "Reviewer".into());
        state
    }

    #[test]
    fn run_tests_button_targets_only_its_current_connected_workspace() {
        let mut state = AppState::new();
        state.connection = ConnectionState::Connected;
        state.workspace_list_loaded = true;
        let workspace = daedalus_proto::WorkspaceId::new();
        state.active_workspace_id = Some(workspace);
        state.test_workspaces.insert(workspace);
        let mut controls = TerminalControls::default();
        let tree = workbench_tree(&state, 800., 600., 1., &mut controls);
        let target = tree
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("Run tests"))
            .unwrap()
            .0;
        let request = ActionRequest {
            target_tree: TreeId::ROOT,
            target_node: target,
            action: Action::Click,
            data: None,
        };
        assert_eq!(
            super::action(&state, &request, &controls),
            Some(UiAction::RunTests)
        );
        state.connection = ConnectionState::Reconnecting;
        assert_eq!(super::action(&state, &request, &controls), None);
        state.connection = ConnectionState::Connected;
        state.active_workspace_id = None;
        assert_eq!(super::action(&state, &request, &controls), None);
    }

    #[test]
    fn failed_run_button_targets_its_runner_and_ignores_stale_requests() {
        let mut state = agent_workspace();
        let mut failed_ws = state.workspaces[0].clone();
        failed_ws.id = daedalus_proto::WorkspaceId::new();
        failed_ws.name = "Feature worktree".into();
        failed_ws.panes.clear();
        let ws_id = failed_ws.id;
        state.workspaces.push(failed_ws);
        let mut runner = state.panes[&state.focused_pane_id().unwrap()]
            .summary
            .clone();
        runner.id = daedalus_proto::PaneId::new();
        runner.workspace_id = ws_id;
        runner.role = PaneRole::Runner;
        runner.last_exit_code = Some(7);
        runner.has_failure_trace = true;
        runner.agent_state = None;
        runner.status = PaneStatus::Exited { code: Some(7) };
        let failed = runner.id;
        state.register_pane(runner, false);
        let mut controls = TerminalControls::default();
        let update = workbench_tree(&state, 900., 700., 1., &mut controls);
        let (id, button) = update
            .nodes
            .iter()
            .find(|(_, n)| n.label() == Some("Open failed run in Feature worktree"))
            .unwrap();
        assert!(button.supports_action(Action::Click));
        let request = ActionRequest {
            action: Action::Click,
            target_node: *id,
            target_tree: TreeId::ROOT,
            data: None,
        };
        assert_eq!(
            super::action(&state, &request, &controls),
            Some(UiAction::OpenFailedRun(ws_id))
        );
        state.picker.toggle();
        assert!(
            workbench_tree(&state, 900., 700., 1., &mut controls)
                .nodes
                .iter()
                .all(|(node, _)| node != id)
        );
        assert_eq!(super::action(&state, &request, &controls), None);
        state.picker.toggle();
        let runner = &mut state.panes.get_mut(&failed).unwrap().summary;
        runner.status = PaneStatus::Idle;
        runner.last_exit_code = Some(0);
        assert_eq!(super::action(&state, &request, &controls), None);
        assert!(
            workbench_tree(&state, 900., 700., 1., &mut controls)
                .nodes
                .iter()
                .all(|(node, _)| node != id)
        );
        state.remove_session(failed);
        assert_eq!(super::action(&state, &request, &controls), None);
    }

    fn cargo_clicks(tree: &TreeUpdate) -> [(CargoPreset, ActionRequest); CargoPreset::ALL.len()] {
        CargoPreset::ALL.map(|preset| {
            let (id, _) = tree
                .nodes
                .iter()
                .find(|(_, node)| node.label() == Some(preset.label()))
                .unwrap();
            (
                preset,
                ActionRequest {
                    action: Action::Click,
                    target_node: *id,
                    target_tree: TreeId::ROOT,
                    data: None,
                },
            )
        })
    }

    #[test]
    fn cargo_clicks_are_scoped_to_the_workspace_that_exposed_them() {
        let mut state = agent_workspace();
        let mut second = state.workspaces[0].clone();
        second.id = daedalus_proto::WorkspaceId::new();
        second.name = "Other cargo project".into();
        second.panes.clear();
        second.project.tools.push("Cargo".into());
        let second_id = second.id;
        state.workspaces.push(second);
        state
            .active_workspace_mut()
            .unwrap()
            .project
            .tools
            .push("Cargo".into());

        let mut controls = TerminalControls::default();
        let first = cargo_clicks(&workbench_tree(&state, 1200., 800., 1., &mut controls));
        for (preset, click) in &first {
            assert_eq!(
                super::action(&state, click, &controls),
                Some(UiAction::RunCargo(*preset))
            );
        }

        state.select_workspace(second_id);
        for (_, stale) in &first {
            assert_eq!(super::action(&state, stale, &controls), None);
        }
        let second = cargo_clicks(&workbench_tree(&state, 1200., 800., 1., &mut controls));
        for ((_, stale), (preset, current)) in first.iter().zip(second) {
            assert_eq!(super::action(&state, stale, &controls), None);
            assert_eq!(
                super::action(&state, &current, &controls),
                Some(UiAction::RunCargo(preset))
            );
        }
    }

    #[test]
    fn close_workspace_exposes_clickable_options_and_dialog_buttons() {
        let mut state = agent_workspace();
        let workspace = state.active_workspace_mut().unwrap();
        workspace.compose_project_name = Some("test-project".into());
        let workspace_id = workspace.id;
        state.connection = ConnectionState::Connected;
        assert!(state.open_close_modal_for_workspace(workspace_id));
        state.close_modal.is_worktree = true;
        state.close_modal.is_dirty = true;
        state.close_modal.toggle_worktree();

        let mut controls = TerminalControls::default();
        let update = workbench_tree(&state, 400., 300., 1.25, &mut controls);
        let dialog = &update.nodes.iter().find(|(id, _)| *id == CLOSE).unwrap().1;
        assert_eq!(dialog.children().len(), 5);
        let mut requests = Vec::new();
        for (label, expected, toggled) in [
            (
                "Stop Docker Compose",
                UiAction::CloseModalToggleCompose,
                Some(accesskit::Toggled::True),
            ),
            (
                "Remove Git worktree",
                UiAction::CloseModalToggleWorktree,
                Some(accesskit::Toggled::True),
            ),
            (
                "Force removal",
                UiAction::CloseModalToggleForce,
                Some(accesskit::Toggled::False),
            ),
            ("Cancel", UiAction::CloseModalCancel, None),
            ("Confirm close", UiAction::CloseModalConfirm, None),
        ] {
            let (id, node) = update
                .nodes
                .iter()
                .find(|(_, node)| node.label() == Some(label))
                .unwrap();
            assert!(dialog.children().contains(id));
            assert!(node.bounds().is_some());
            assert_eq!(node.toggled(), toggled);
            assert!(node.supports_action(Action::Click));
            let request = ActionRequest {
                action: Action::Click,
                target_node: *id,
                target_tree: TreeId::ROOT,
                data: None,
            };
            assert_eq!(super::action(&state, &request, &controls), Some(expected));
            requests.push(request);
        }
        assert_eq!(update.focus, requests[3].target_node);

        state.connection = ConnectionState::Reconnecting;
        assert_eq!(super::action(&state, &requests[4], &controls), None);
        assert_eq!(
            super::action(&state, &requests[3], &controls),
            Some(UiAction::CloseModalCancel)
        );
        state.connection = ConnectionState::Connected;
        state.close_modal.toggle_worktree();
        assert_eq!(super::action(&state, &requests[2], &controls), None);
        state.close_modal.submitting = true;
        let pending = workbench_tree(&state, 400., 300., 1.25, &mut controls);
        assert_eq!(pending.focus, CLOSE);
        for request in &requests {
            assert_eq!(super::action(&state, request, &controls), None);
            if let Some((_, node)) = pending
                .nodes
                .iter()
                .find(|(id, _)| *id == request.target_node)
            {
                assert!(node.is_disabled());
                assert!(!node.supports_action(Action::Click));
            }
        }

        state.close_modal.close();
        assert!(state.open_close_modal_for_workspace(workspace_id));
        assert_eq!(super::action(&state, &requests[4], &controls), None);
        let _ = workbench_tree(&state, 400., 300., 1.25, &mut controls);
        for request in &requests {
            assert_eq!(super::action(&state, request, &controls), None);
        }
    }

    #[test]
    fn cargo_accessibility_actions_require_a_cargo_workspace_and_no_modal() {
        let mut state = agent_workspace();
        let mut controls = TerminalControls::default();
        let toggle = ActionRequest {
            action: Action::Click,
            target_node: TOGGLE_LAYOUT,
            target_tree: TreeId::ROOT,
            data: None,
        };
        assert_eq!(
            super::action(&state, &toggle, &controls),
            Some(UiAction::ToggleZoom)
        );
        let initial = workbench_tree(&state, 1200., 800., 1., &mut controls);
        for preset in CargoPreset::ALL {
            assert!(
                initial
                    .nodes
                    .iter()
                    .all(|(_, node)| node.label() != Some(preset.label()))
            );
        }

        state
            .active_workspace_mut()
            .unwrap()
            .project
            .tools
            .push("Cargo".into());
        let current = workbench_tree(&state, 1200., 800., 1., &mut controls);
        let clicks = cargo_clicks(&current);
        state.worktree_modal.open();
        for (_, click) in &clicks {
            assert_eq!(super::action(&state, click, &controls), None);
        }

        state.worktree_modal.close();
        state.active_workspace_mut().unwrap().project.tools.clear();
        for (_, click) in &clicks {
            assert_eq!(super::action(&state, click, &controls), None);
        }
    }

    #[test]
    fn dialog_focus_returns_to_the_active_workspaces_focused_pane() {
        let mut state = agent_workspace();
        let original = tree(&state);
        // Per-session focus also distinguishes terminals sharing a role.
        let terminal = &original
            .nodes
            .iter()
            .find(|n| n.0 == original.focus)
            .unwrap()
            .1;
        assert_eq!(terminal.role(), Role::Terminal);
        assert_eq!(terminal.label(), Some("Reviewer, Agent pane in Project"));
        assert_eq!(
            terminal.description(),
            Some("Busy. Agent waiting for approval")
        );
        state.worktree_modal.open();
        let modal = tree(&state);
        assert_eq!(modal.focus, worktree_controls(&state).0);
        assert!(modal.nodes.iter().all(|n| n.0 != original.focus));
        state.worktree_modal.close();
        assert_eq!(tree(&state), original);
    }
    #[test]
    fn terminal_actions_preserve_identity_and_follow_same_role_focus_after_close() {
        let mut state = agent_workspace();
        state.connection = ConnectionState::Connected;
        let first = state.focused_pane_id().unwrap();
        let mut other = state.panes[&first].summary.clone();
        other.id = daedalus_proto::PaneId::new();
        let second = other.id;
        let ws = other.workspace_id;
        state.register_pane(other, false);
        state.session_names.insert(second, "Builder".into());
        state.select_session(ws, PaneRole::Agent, first);
        let mut controls = TerminalControls::default();
        let render = |state: &AppState, controls: &mut TerminalControls| {
            workbench_tree(state, 320., 480., 1., controls)
        };
        let target = |t: &TreeUpdate, label: &str| {
            t.nodes
                .iter()
                .find(|(_, node)| node.label() == Some(label))
                .unwrap()
                .0
        };
        let click = |node| ActionRequest {
            action: Action::Click,
            target_node: node,
            target_tree: TreeId::ROOT,
            data: None,
        };
        let initial = render(&state, &mut controls);
        let selected = target(&initial, "Reviewer");
        assert_eq!(
            initial
                .nodes
                .iter()
                .find(|n| n.0 == selected)
                .unwrap()
                .1
                .is_selected(),
            Some(true)
        );
        let select_second = click(target(&initial, "Builder"));
        let close_first = click(target(&initial, "Close Reviewer terminal"));
        assert_eq!(
            controls.action(&state, &select_second),
            Some(daedalus_ui::layout::HitTarget::SelectSession(second))
        );
        state.select_session(ws, PaneRole::Agent, second);
        let switched = render(&state, &mut controls);
        assert_ne!(initial.focus, switched.focus);
        assert_eq!(
            switched.focus,
            target(&switched, "Builder, Agent pane in Project")
        );
        assert_eq!(controls.action(&state, &close_first), None);
        let close_second = click(target(&switched, "Close Builder terminal"));
        assert_eq!(
            controls.action(&state, &close_second),
            Some(daedalus_ui::layout::HitTarget::CloseSession(second))
        );
        state.remove_session(second);
        let remaining = render(&state, &mut controls);
        assert_eq!(remaining.focus, initial.focus);
        assert_eq!(controls.action(&state, &close_second), None);
        assert_eq!(controls.action(&state, &select_second), None);
        state.remove_session(first);
        assert_eq!(render(&state, &mut controls).focus, OPEN_SETTINGS);
    }

    #[test]
    fn terminal_close_requires_an_active_connected_workspace_without_an_overlay() {
        let mut state = agent_workspace();
        let mut controls = TerminalControls::default();
        let t = workbench_tree(&state, 1280., 800., 1., &mut controls);
        let (id, node) = t
            .nodes
            .iter()
            .find(|(_, n)| n.label() == Some("Close Reviewer terminal"))
            .unwrap();
        assert!(node.is_disabled());
        let request = ActionRequest {
            action: Action::Click,
            target_tree: TreeId::ROOT,
            target_node: *id,
            data: None,
        };
        assert_eq!(controls.action(&state, &request), None);
        state.connection = ConnectionState::Connected;
        state.picker.toggle();
        assert_eq!(controls.action(&state, &request), None);
        state.picker.close();
        state.active_workspace_id = None;
        assert_eq!(controls.action(&state, &request), None);
    }
}
