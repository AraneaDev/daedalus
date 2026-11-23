use accesskit::{Action, ActionRequest, Live, Node, NodeId, Role, TreeId, TreeInfo, TreeUpdate};
use daedalus_proto::{AgentState, PaneRole, PaneStatus};
use daedalus_ui::input::UiAction;
use daedalus_ui::layout::Rect;
use daedalus_ui::renderer::{worktree_button_rects, worktree_modal_rect};
use daedalus_ui::state::{AppState, ConnectionState};

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

fn worktree_buttons(state: &AppState) -> (NodeId, NodeId) {
    // Queued platform actions from a prior dialog must not affect its successor.
    let first = 100 + state.worktree_modal.session() * 2;
    (NodeId(first), NodeId(first + 1))
}

pub(super) fn action(state: &AppState, request: &ActionRequest) -> Option<UiAction> {
    if !state.worktree_modal.open
        || state.picker.open
        || state.action_palette.open
        || request.target_tree != TreeId::ROOT
        || request.action != Action::Click
    {
        return None;
    }
    let (cancel, create) = worktree_buttons(state);
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
    let (id, label, description) = if state.picker.open {
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
        let branch = &state.worktree_modal.branch_input;
        (
            WORKTREE,
            "New Git worktree",
            format!(
                "Branch name: {}. Enter to create. Escape to cancel.",
                if branch.is_empty() { "empty" } else { branch }
            ),
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
        if id == WORKTREE {
            let (cancel, create) = worktree_buttons(state);
            let modal = worktree_modal_rect(width, height, scale);
            node.set_bounds(bounds(modal));
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
        id
    } else if let Some(ws) = state.active_workspace() {
        let mut workspace = named(Role::Group, format!("Workspace {}", ws.name));
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
        let (cancel, create) = worktree_buttons(&state);
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
        assert_eq!(modal.focus, WORKTREE);
        assert!(modal.nodes.iter().all(|n| n.0 != AGENT));
        state.worktree_modal.close();
        assert_eq!(tree(&state), original);
    }
}
