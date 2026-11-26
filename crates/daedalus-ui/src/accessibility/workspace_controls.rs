use super::{named, terminals::next_control_group};
use accesskit::{Action, Node, NodeId, Role};
use daedalus_proto::WorkspaceId;
use daedalus_ui::input::UiAction;
use daedalus_ui::state::{AppState, WorkspaceView};
use std::collections::HashMap;

pub(super) const OPEN_NEW_WORKSPACE: NodeId = NodeId(4);
const OPEN_NEW_WORKTREE: NodeId = NodeId(24);
const WORKSPACE_LIST: NodeId = NodeId(23);

#[derive(Default)]
pub(super) struct WorkspaceControls {
    rows: HashMap<WorkspaceId, NodeId>,
}

impl WorkspaceControls {
    pub(super) fn append(
        &mut self,
        state: &AppState,
        root: &mut Node,
        nodes: &mut Vec<(NodeId, Node)>,
        serial: &mut u64,
    ) {
        self.rows
            .retain(|id, _| state.workspaces.iter().any(|workspace| workspace.id == *id));
        if !state.workspaces.is_empty() {
            let mut list = named(Role::ListBox, "Workspaces");
            root.push_child(WORKSPACE_LIST);
            for workspace in state.workspaces_in_sidebar_order() {
                let id = *self
                    .rows
                    .entry(workspace.id)
                    .or_insert_with(|| next_control_group(serial));
                let label = workspace_label(state, workspace);
                let mut option = named(Role::ListBoxOption, &label);
                option.set_selected(state.active_workspace_id == Some(workspace.id));
                option.add_action(Action::Click);
                list.push_child(id);
                nodes.push((id, option));
                append_button(root, nodes, close_node(id), format!("Close {label}"));
            }
            nodes.push((WORKSPACE_LIST, list));
        }
        append_button(root, nodes, OPEN_NEW_WORKSPACE, "New workspace");
        append_button(root, nodes, OPEN_NEW_WORKTREE, "New Git worktree");
    }

    pub(super) fn action(&self, state: &AppState, target: NodeId) -> Option<UiAction> {
        match target {
            OPEN_NEW_WORKSPACE => Some(UiAction::NewWorkspace),
            OPEN_NEW_WORKTREE => Some(UiAction::ToggleWorktreeModal),
            _ => state.workspaces.iter().find_map(|workspace| {
                let id = *self.rows.get(&workspace.id)?;
                if target == id {
                    Some(UiAction::SelectWorkspace(workspace.id))
                } else if target == close_node(id) {
                    Some(UiAction::CloseWorkspaceFromSidebar(workspace.id))
                } else {
                    None
                }
            }),
        }
    }
}

fn close_node(base: NodeId) -> NodeId {
    NodeId(base.0 + 1)
}

fn append_button(
    root: &mut Node,
    nodes: &mut Vec<(NodeId, Node)>,
    id: NodeId,
    label: impl Into<String>,
) {
    let mut button = named(Role::Button, label);
    button.add_action(Action::Click);
    root.push_child(id);
    nodes.push((id, button));
}

fn workspace_label(state: &AppState, workspace: &WorkspaceView) -> String {
    let mut duplicates = state
        .workspaces
        .iter()
        .filter(|other| other.id != workspace.id && other.name == workspace.name)
        .peekable();
    if duplicates.peek().is_none() {
        return workspace.name.clone();
    }
    let mut label = format!("{} · {}", workspace.name, workspace.root_dir.display());
    if duplicates.any(|other| other.root_dir == workspace.root_dir) {
        label.push_str(&format!(" · {}", workspace.id.0.simple()));
    }
    label
}
