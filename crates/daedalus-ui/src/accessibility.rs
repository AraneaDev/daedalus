mod settings;
mod terminals;
pub(super) use settings::settings_action;
pub(super) use terminals::TerminalControls;

use accesskit::{
    Action, ActionData, ActionRequest, Invalid, Live, Node, NodeId, Role, TextPosition,
    TextSelection, TreeId, TreeInfo, TreeUpdate,
};
use daedalus_proto::{AgentState, PaneRole, PaneStatus};
use daedalus_ui::input::{UiAction, filter_modal_text};
use daedalus_ui::layout::Rect;
use daedalus_ui::renderer::{worktree_button_rects, worktree_input_rect, worktree_modal_rect};
use daedalus_ui::state::{AppState, ConnectionState};
use daedalus_ui::workspace_form::{self, Control, WorkspaceForm, WorkspaceFormAction};
use unicode_segmentation::UnicodeSegmentation;

const WINDOW: NodeId = NodeId(1);
const STATUS: NodeId = NodeId(2);
const WORKSPACE: NodeId = NodeId(3);
const PICKER: NodeId = NodeId(7);
const ACTIONS: NodeId = NodeId(8);
const WORKTREE: NodeId = NodeId(9);
const CLOSE: NodeId = NodeId(10);
const SEARCH: NodeId = NodeId(11);

const SETTINGS: NodeId = NodeId(14);
const OPEN_SETTINGS: NodeId = NodeId(17);
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

fn field_offset(control: Control) -> u64 {
    match control {
        Control::Name => 0,
        Control::Folder => 1,
        Control::Cancel => 2,
        Control::Save => 3,
    }
}

pub(super) fn workspace_action(
    state: &mut AppState,
    request: &ActionRequest,
) -> Option<WorkspaceFormAction> {
    if !state.workspace_form.open
        || state.settings.open
        || state.failure_recipients.is_some()
        || request.target_tree != TreeId::ROOT
    {
        return None;
    }
    let form = &mut state.workspace_form;
    let control = [
        Control::Name,
        Control::Folder,
        Control::Cancel,
        Control::Save,
    ]
    .into_iter()
    .find(|c| form.enabled(*c) && form_node(form, field_offset(*c)) == request.target_node)?;
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

pub(super) fn action(state: &AppState, request: &ActionRequest) -> Option<UiAction> {
    if request.target_tree == TreeId::ROOT
        && request.target_node == OPEN_SETTINGS
        && request.action == Action::Click
        && state.active_workspace().is_some()
        && overlay(state).is_none()
    {
        return Some(UiAction::WorkspaceSettings);
    }
    if !state.worktree_modal.open
        || state.workspace_form.open
        || state.settings.open
        || state.failure_recipients.is_some()
        || state.picker.open
        || state.action_palette.open
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
    let (id, label, description) = if let Some(choices) = &state.failure_recipients {
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
    } else if state.settings.open {
        (SETTINGS, "Workspace settings", state.settings.heading())
    } else if state.workspace_form.open {
        (
            NEW_WORKSPACE,
            "New workspace",
            if state.workspace_form.submitting() {
                "Creating workspace. Escape dismisses this dialog.".into()
            } else {
                "Tab changes focus. Enter creates the workspace. Escape cancels.".into()
            },
        )
    } else if state.picker.open {
        (
            PICKER,
            "Select agent preset",
            format!(
                "Selected {}. Use Up and Down to choose, Enter to confirm, or Escape to cancel.",
                state.picker.selected_preset().label()
            ),
        )
    } else if state.action_palette.open {
        let selection = state.active_actions().get(state.action_palette.selected_index).map_or_else(
            || "No actions configured for this workspace.".to_string(),
            |action| format!("Selected {}. Use Up and Down to choose, Enter to run, or Escape to cancel.", action.label),
        );
        (ACTIONS, "Action palette", selection)
    } else if state.worktree_modal.open {
        (
            WORKTREE,
            "New Git worktree",
            "Enter to create. Escape to cancel.".to_owned(),
        )
    } else if state.close_modal.open {
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
        detail.push_str(" Escape to close.");
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
    let invalid_field = form.error.as_ref().and_then(|_| {
        [Control::Name, Control::Folder]
            .into_iter()
            .find(|control| {
                form.value(*control)
                    .is_some_and(|text| text.trim().is_empty())
            })
    });
    if let Some(message) = &form.error {
        let mut alert = named(Role::Alert, message);
        alert.set_live(Live::Assertive);
        dialog.push_child(error_id);
        nodes.push((error_id, alert));
    }
    for (control, rect) in workspace_form::controls(modal) {
        let offset = field_offset(control);
        let id = form_node(form, offset);
        let label = match control {
            Control::Name => "Name",
            Control::Folder => "Folder",
            Control::Cancel if form.submitting() => "Dismiss",
            Control::Cancel => "Cancel",
            Control::Save => "Create workspace",
        };
        let mut node = if let Some(value) = form.value(control) {
            let run_id = form_node(form, offset + 4);
            let mut node = editable_field(label, value, run_id, form.selection(control), nodes);
            if invalid_field == Some(control) {
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
        dialog.push_child(id);
        nodes.push((id, node));
    }
    form_node(form, field_offset(form.focus))
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
    let focus = if let Some((id, mut node)) = overlay(state) {
        let mut focus = id;
        if id == SETTINGS {
            focus = settings::fields(
                &state.settings,
                &mut node,
                &mut nodes,
                daedalus_ui::settings::modal_rect(width, height, scale),
            );
        } else if id == NEW_WORKSPACE {
            focus = workspace_fields(
                &state.workspace_form,
                &mut node,
                &mut nodes,
                workspace_form::modal_rect(width, height, scale),
            );
        } else if id == WORKTREE {
            let (branch, cancel, create) = worktree_controls(state);
            let modal = worktree_modal_rect(width, height, scale);
            node.set_bounds(bounds(modal));
            let mut field = named(Role::TextInput, "Branch name");
            field.set_bounds(bounds(worktree_input_rect(modal, scale)));
            let run = text_run(&state.worktree_modal.branch_input);
            let caret = TextPosition {
                node: BRANCH_TEXT,
                character_index: run.character_lengths().len(),
            };
            field.push_child(BRANCH_TEXT);
            field.set_text_selection(TextSelection {
                anchor: caret,
                focus: caret,
            });
            nodes.push((BRANCH_TEXT, run));
            field.set_required();
            field.add_action(Action::SetValue);
            if state.worktree_modal.missing_branch() {
                field.set_invalid(Invalid::True);
                field.set_error_message(VALIDATION);
            }
            node.push_child(branch);
            nodes.push((branch, field));
            focus = branch;
            let (cancel_rect, create_rect) = worktree_button_rects(modal, scale);
            for (button_id, name, enabled, rect) in [
                (cancel, "Cancel", true, cancel_rect),
                (create, "Create", state.can_create_worktree(), create_rect),
            ] {
                let mut button = named(Role::Button, name);
                button.set_bounds(bounds(rect));
                if enabled {
                    button.add_action(Action::Click);
                } else {
                    button.set_disabled();
                }
                node.push_child(button_id);
                nodes.push((button_id, button));
            }
            if state.worktree_modal.missing_branch() {
                let mut alert = named(Role::Alert, "Enter a branch name");
                alert.set_live(Live::Assertive);
                node.push_child(VALIDATION);
                nodes.push((VALIDATION, alert));
            }
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
        description.extend(state.recap.lines(ws.id));
        workspace.set_description(description.join(". "));
        let mut open_settings = named(Role::Button, "Workspace settings");
        open_settings.add_action(Action::Click);
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
        let modal = workspace_form::modal_rect(320., 480., 1.);
        let update = super::tree(&state, 320., 480., 1.);
        assert_eq!(update.focus, form_node(&state.workspace_form, 0));
        for (control, rect) in workspace_form::controls(modal) {
            let id = form_node(&state.workspace_form, field_offset(control));
            let node = &update.nodes.iter().find(|(n, _)| *n == id).unwrap().1;
            assert_eq!(node.bounds(), Some(bounds(rect)));
            assert!(node.supports_action(Action::Focus));
            if state.workspace_form.value(control).is_some() {
                assert!(node.is_required() && node.supports_action(Action::SetValue));
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
        state.workspace_form.begin(std::path::Path::new("/project"));
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
        let name = request(
            form_node(&state.workspace_form, 0),
            Action::SetValue,
            Some(ActionData::Value("日本語 work".into())),
        );
        assert_eq!(
            workspace_action(&mut state, &name),
            Some(WorkspaceFormAction::None)
        );
        assert_eq!(
            workspace_action(&mut state, &save),
            Some(WorkspaceFormAction::Save {
                name: "日本語 work".into(),
                folder: "/project".into()
            })
        );
        state.workspace_form.begin(std::path::Path::new("/second"));
        assert_eq!(workspace_action(&mut state, &save), None);
        assert_eq!(workspace_action(&mut state, &name), None);
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
            agent_state: Some(AgentState::WaitingForApproval),
        };
        state.set_workspaces(vec![WorkspaceInfo {
            project: Default::default(),
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
        state.register_pane(other, None);
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
