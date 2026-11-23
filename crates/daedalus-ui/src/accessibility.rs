use accesskit::{
    Action, ActionData, ActionRequest, Invalid, Live, Node, NodeId, Role, TextPosition,
    TextSelection, TreeId, TreeInfo, TreeUpdate,
};
use daedalus_proto::{AgentState, PaneRole, PaneStatus};
use daedalus_ui::input::{UiAction, filter_modal_text};
use daedalus_ui::layout::Rect;
use daedalus_ui::renderer::{worktree_button_rects, worktree_input_rect, worktree_modal_rect};
use daedalus_ui::state::{AppState, ConnectionState};
use unicode_segmentation::UnicodeSegmentation;

const WINDOW: NodeId = NodeId(1);
const STATUS: NodeId = NodeId(2);
const WORKSPACE: NodeId = NodeId(3);
const AGENT: NodeId = NodeId(4);
const SERVICE: NodeId = NodeId(5);
const RUNNER: NodeId = NodeId(6);
const PICKER: NodeId = NodeId(7);
const ACTIONS: NodeId = NodeId(8);
const WORKTREE: NodeId = NodeId(9);
const CLOSE: NodeId = NodeId(10);
const SEARCH: NodeId = NodeId(11);

const VALIDATION: NodeId = NodeId(12);
const BRANCH_TEXT: NodeId = NodeId(13);

fn worktree_controls(state: &AppState) -> (NodeId, NodeId, NodeId) {
    // Queued platform actions from a prior dialog must not affect its successor.
    let first = 100 + state.worktree_modal.session() * 3;
    (NodeId(first), NodeId(first + 1), NodeId(first + 2))
}

pub(super) fn action(state: &AppState, request: &ActionRequest) -> Option<UiAction> {
    if !state.worktree_modal.open
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
        (
            NodeId(14),
            "Workspace settings",
            state.settings.accessibility_description(),
        )
    } else if state.workspace_form.open {
        (
            NodeId(15),
            "New workspace",
            state.workspace_form.accessibility_description(),
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
fn branch_text_run(text: &str) -> Node {
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

pub(super) fn tree(state: &AppState, width: f32, height: f32, scale: f32) -> TreeUpdate {
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
        if id == WORKTREE {
            let (branch, cancel, create) = worktree_controls(state);
            let modal = worktree_modal_rect(width, height, scale);
            node.set_bounds(bounds(modal));
            let mut field = named(Role::TextInput, "Branch name");
            field.set_bounds(bounds(worktree_input_rect(modal, scale)));
            let run = branch_text_run(&state.worktree_modal.branch_input);
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
        root.push_child(WORKSPACE);
        let mut focus = WORKSPACE;
        if let Some(pane) = state.pane_for_role(state.focused_role) {
            let (id, label) = match state.focused_role {
                PaneRole::Agent => (AGENT, "Agent pane"),
                PaneRole::Service => (SERVICE, "Service pane"),
                PaneRole::Runner => (RUNNER, "Runner pane"),
            };
            let status = match pane.summary.status {
                PaneStatus::Starting => "Starting".to_owned(),
                PaneStatus::Idle => "Idle".to_owned(),
                PaneStatus::Busy => "Busy".to_owned(),
                PaneStatus::Exited { code: Some(code) } => format!("Exited with code {code}"),
                PaneStatus::Exited { code: None } => "Exited".to_owned(),
            };
            let mut terminal = named(Role::Terminal, format!("{label} in {}", ws.name));
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
        assert_eq!(
            branch_text_run("e\u{301}/🌱").character_lengths(),
            &[3, 1, 4]
        );
        let text = format!("e{}", "\u{301}".repeat(150));
        let run = branch_text_run(&text);
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
    fn dialog_focus_returns_to_the_active_workspaces_focused_pane() {
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
        let original = tree(&state);
        assert_eq!(original.focus, AGENT);
        let terminal = &original.nodes.iter().find(|n| n.0 == AGENT).unwrap().1;
        assert_eq!(terminal.label(), Some("Agent pane in Project"));
        assert_eq!(
            terminal.description(),
            Some("Busy. Agent waiting for approval")
        );
        state.worktree_modal.open();
        let modal = tree(&state);
        assert_eq!(modal.focus, worktree_controls(&state).0);
        assert!(modal.nodes.iter().all(|n| n.0 != AGENT));
        state.worktree_modal.close();
        assert_eq!(tree(&state), original);
    }
}
