mod setup_review;
pub(super) use setup_review::setup_action;
mod settings;
mod terminals;
mod worktree;
pub(super) use settings::settings_action;
pub(super) use terminals::TerminalControls;

use accesskit::{
    Action, ActionData, ActionRequest, Invalid, Live, Node, NodeId, Role, TextPosition,
    TextSelection, TreeId, TreeInfo, TreeUpdate,
};
use daedalus_proto::{AgentState, PaneRole, PaneStatus};
use daedalus_ui::input::{UiAction, filter_modal_text};
use daedalus_ui::layout::Rect;
#[cfg(test)]
use daedalus_ui::layout::overlays::{worktree_input_rect, worktree_modal_rect};
use daedalus_ui::state::{ActiveOverlay, AppState, ConnectionState};
use daedalus_ui::workspace_form::{Control, WorkspaceForm, WorkspaceFormAction};
use unicode_segmentation::UnicodeSegmentation;

const WINDOW: NodeId = NodeId(1);
const STATUS: NodeId = NodeId(2);
const WORKSPACE: NodeId = NodeId(3);
const OPEN_NEW_WORKSPACE: NodeId = NodeId(4);
const PICKER: NodeId = NodeId(7);
const ACTIONS: NodeId = NodeId(8);
const WORKTREE: NodeId = NodeId(9);
const CLOSE: NodeId = NodeId(10);
const SEARCH: NodeId = NodeId(11);

const SETUP_REVIEW: NodeId = NodeId(19);
const SETTINGS: NodeId = NodeId(14);
const OPEN_SETTINGS: NodeId = NodeId(17);
const TOGGLE_LAYOUT: NodeId = NodeId(20);
const CARGO_BUTTONS: [(NodeId, daedalus_ui::assignments::CargoPreset); 2] = [
    (NodeId(21), daedalus_ui::assignments::CargoPreset::Test),
    (NodeId(22), daedalus_ui::assignments::CargoPreset::Check),
];
const NEW_WORKSPACE: NodeId = NodeId(15);
const VALIDATION: NodeId = NodeId(12);
const BRANCH_TEXT: NodeId = NodeId(13);

fn worktree_controls(state: &AppState) -> (NodeId, NodeId, NodeId) {
    // Queued platform actions from a prior dialog must not affect its successor.
    let first = 100 + state.worktree_modal.session() * 3;
    (NodeId(first), NodeId(first + 1), NodeId(first + 2))
}

// The high bit keeps form control IDs separate from the worktree dialog's IDs.
fn form_node(form: &WorkspaceForm, offset: u64) -> NodeId {
    NodeId((1 << 63) | form.session().wrapping_mul(8) | offset)
}

fn control_node(form: &WorkspaceForm, control: Control) -> NodeId {
    match control {
        Control::Name => form_node(form, 0),
        Control::Folder => form_node(form, 1),
        Control::Cancel => form_node(form, 2),
        Control::Save => form_node(form, 3),
        Control::Recent(i) => recent_node(form, i as u64),
    }
}

// Bit 62 next to the form's bit keeps recent rows apart from the form's other nodes.
fn recent_node(form: &WorkspaceForm, offset: u64) -> NodeId {
    NodeId((0b11 << 62) | ((form.session() & ((1 << 56) - 1)) << 6) | offset)
}

fn recent_list_node(form: &WorkspaceForm) -> NodeId {
    recent_node(form, 63)
}

pub(super) fn workspace_action(
    state: &mut AppState,
    request: &ActionRequest,
) -> Option<WorkspaceFormAction> {
    if state.active_overlay() != Some(ActiveOverlay::WorkspaceForm)
        || request.target_tree != TreeId::ROOT
    {
        return None;
    }
    let form = &mut state.workspace_form;
    let (control, _) = form
        .controls(Rect::new(0., 0., 1., 1.))
        .into_iter()
        .find(|(c, _)| form.enabled(*c) && control_node(form, *c) == request.target_node)?;
    match request.action {
        Action::Focus => {
            if control != form.focus {
                form.set_focus(control);
            }
        }
        Action::Click if form.value(control).is_none() => return Some(form.activate(control)),
        Action::SetValue if form.value(control).is_some() => {
            let Some(ActionData::Value(text)) = &request.data else {
                return None;
            };
            form.set_value(control, text);
        }
        _ => return None,
    }
    Some(WorkspaceFormAction::None)
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
            OPEN_SETTINGS if state.active_workspace().is_some() => {
                return Some(UiAction::WorkspaceSettings);
            }
            OPEN_NEW_WORKSPACE if state.workspace_list_loaded => {
                return Some(UiAction::NewWorkspace);
            }
            node if state.workspace_list_loaded => {
                if state.show_cargo_bar()
                    && let Some((_, preset)) = CARGO_BUTTONS.iter().find(|(id, _)| *id == node)
                {
                    return Some(UiAction::RunCargo(*preset));
                }
                if controls.is_run_tests(state, node) {
                    return Some(UiAction::RunTests);
                }
                if let Some(id) = controls.failed_run_workspace(state, node) {
                    return Some(UiAction::OpenFailedRun(id));
                }
                if let Some(id) = controls.workspace_for_node(state, node) {
                    return Some(UiAction::SelectWorkspace(id));
                }
            }
            _ => {}
        }
    }
    if state.active_overlay() != Some(ActiveOverlay::Worktree)
        || request.target_tree != TreeId::ROOT
    {
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
                "Selected {}. Use Up and Down to choose, Enter to confirm, or Escape to cancel.",
                state.picker.selected_preset().label()
            ),
        )
    } else if overlay == Some(ActiveOverlay::ActionPalette) {
        let selection = state.active_actions().get(state.action_palette.selected_index).map_or_else(
            || "No actions configured for this workspace.".to_string(),
            |action| format!("Selected {}. Use Up and Down to choose, Enter to run, or Escape to cancel.", action.label),
        );
        (ACTIONS, "Action palette", selection)
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
        if modal.has_compose {
            detail.push(format!("Stop Docker Compose: {}", modal.compose_down));
        }
        if modal.is_worktree {
            detail.push(format!("Remove worktree: {}", modal.remove_worktree));
        }
        if modal.is_dirty && modal.remove_worktree {
            detail.push(format!("Force removal: {}", modal.force_remove_worktree));
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

fn workspace_fields(
    form: &WorkspaceForm,
    dialog: &mut Node,
    nodes: &mut Vec<(NodeId, Node)>,
    modal: Rect,
) -> NodeId {
    dialog.set_bounds(bounds(modal));
    let error_id = form_node(form, 6);
    if let Some(message) = &form.error {
        let mut alert = named(Role::Alert, message);
        alert.set_live(Live::Assertive);
        dialog.push_child(error_id);
        nodes.push((error_id, alert));
    }
    let list_id = recent_list_node(form);
    let mut list = form.recent_heading(modal).map(|heading| {
        dialog.push_child(list_id);
        let mut list = named(Role::List, "Recent projects");
        let rows = form.controls(modal);
        let last = rows
            .iter()
            .filter(|(c, _)| matches!(c, Control::Recent(_)))
            .map(|(_, rect)| rect.bottom())
            .fold(heading.bottom(), f32::max);
        list.set_bounds(bounds(Rect::new(
            heading.x,
            heading.y,
            heading.width,
            last - heading.y,
        )));
        list
    });
    for (control, rect) in form.controls(modal) {
        let id = control_node(form, control);
        let label = match control {
            Control::Name if form.name_is_fixed() => "Workspace name",
            Control::Name => "Name (optional)",
            Control::Folder => "Folder",
            Control::Cancel if form.submitting() => "Dismiss",
            Control::Cancel => "Cancel",
            Control::Save => "Review environment",
            Control::Recent(_) => "",
        };
        let mut node = if let Control::Recent(i) = control {
            let project = &form.recent()[i];
            let mut node = named(
                Role::ListItem,
                format!("{}, {}", project.name, project.root_dir.display()),
            );
            node.add_action(Action::Click);
            node
        } else if let Some(value) = form.value(control) {
            let offset = if control == Control::Name { 0 } else { 1 };
            let run_id = form_node(form, offset + 4);
            let mut node = editable_field(label, value, run_id, form.selection(control), nodes);
            if control == Control::Name {
                node.clear_required();
            }
            if control == Control::Folder
                && form.error.as_deref().is_some_and(|message| {
                    message == "Enter the workspace folder."
                        || message.contains("root_dir does not exist")
                })
            {
                node.set_invalid(Invalid::True);
                node.set_error_message(error_id);
            }
            node
        } else {
            let mut node = named(Role::Button, label);
            node.add_action(Action::Click);
            node
        };
        node.set_bounds(bounds(rect));
        if form.enabled(control) {
            node.add_action(Action::Focus);
        } else {
            node.set_disabled();
            node.remove_action(Action::Click);
            node.remove_action(Action::SetValue);
        }
        match (&mut list, control) {
            (Some(list), Control::Recent(_)) => list.push_child(id),
            _ => dialog.push_child(id),
        }
        nodes.push((id, node));
    }
    if let Some(list) = list {
        nodes.push((list_id, list));
    }
    control_node(form, form.focus)
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

pub(super) fn workbench_tree(
    state: &AppState,
    width: f32,
    height: f32,
    scale: f32,
    controls: &mut TerminalControls,
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
    if let Some((message, _, _)) = &state.toast {
        status_text.push_str(". ");
        status_text.push_str(message);
    }
    let mut status = named(Role::Status, status_text);
    status.set_live(Live::Polite);
    root.push_child(STATUS);
    let mut nodes = vec![(STATUS, status)];
    let current_overlay = overlay(state);
    if state.workspace_list_loaded && current_overlay.is_none() {
        controls.append_workspaces(state, &mut root, &mut nodes);
        controls.append_run_tests(state, &mut root, &mut nodes);
        controls.append_widgets(state, &mut root, &mut nodes);
        controls.append_failed_runs(state, &mut root, &mut nodes);
        let mut button = named(Role::Button, "New workspace");
        button.add_action(Action::Click);
        root.push_child(OPEN_NEW_WORKSPACE);
        nodes.push((OPEN_NEW_WORKSPACE, button));
    }
    let focus = if let Some((id, mut node)) = current_overlay {
        let mut focus = id;
        if id == SETUP_REVIEW {
            focus = setup_review::fields(
                &state.setup_review,
                &mut node,
                &mut nodes,
                daedalus_ui::setup_review::modal_rect(width, height, scale),
            );
        } else if id == SETTINGS {
            focus = settings::fields(
                &state.settings,
                &mut node,
                &mut nodes,
                state.settings.modal_rect(width, height, scale),
            );
        } else if id == NEW_WORKSPACE {
            focus = workspace_fields(
                &state.workspace_form,
                &mut node,
                &mut nodes,
                state.workspace_form.modal_rect(width, height, scale),
            );
        } else if id == WORKTREE {
            focus = worktree::append_fields(state, &mut node, &mut nodes, width, height, scale);
        }
        root.push_child(id);
        nodes.push((id, node));
        focus
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
        if state.show_cargo_bar() {
            for (id, preset) in CARGO_BUTTONS {
                let mut button = named(Role::Button, preset.label());
                button.add_action(Action::Click);
                workspace.push_child(id);
                nodes.push((id, button));
            }
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
    use super::*;
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
    fn modal_reports_current_selection_and_restores_focus() {
        let mut state = AppState::new();
        state.picker.toggle();
        let first = tree(&state);
        state.picker.move_down();
        let second = tree(&state);
        assert_eq!(first.focus, PICKER);
        assert_eq!(second.focus, PICKER);
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
            state.search.as_mut().unwrap().toggle_case();
        }
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
        assert_eq!(
            workspace_action(&mut state, &request(items[1], Action::Click)),
            Some(WorkspaceFormAction::Reopen(projects[1].clone()))
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

    #[test]
    fn cargo_accessibility_actions_require_a_cargo_workspace_and_no_modal() {
        let mut state = agent_workspace();
        let controls = TerminalControls::default();
        let click = |node| ActionRequest {
            action: Action::Click,
            target_node: node,
            target_tree: TreeId::ROOT,
            data: None,
        };
        assert_eq!(
            super::action(&state, &click(TOGGLE_LAYOUT), &controls),
            Some(UiAction::ToggleZoom)
        );
        for (id, _) in CARGO_BUTTONS {
            assert_eq!(super::action(&state, &click(id), &controls), None);
        }
        state
            .active_workspace_mut()
            .unwrap()
            .project
            .tools
            .push("Cargo".into());
        let current = tree(&state);
        for (id, preset) in CARGO_BUTTONS {
            assert!(
                current
                    .nodes
                    .iter()
                    .any(|(node_id, node)| *node_id == id && node.label() == Some(preset.label()))
            );
            assert_eq!(
                super::action(&state, &click(id), &controls),
                Some(UiAction::RunCargo(preset))
            );
        }
        state.worktree_modal.open();
        for (id, _) in CARGO_BUTTONS {
            assert_eq!(super::action(&state, &click(id), &controls), None);
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
