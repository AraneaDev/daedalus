use std::collections::HashMap;

use super::{named, overlay};
use accesskit::{Action, ActionRequest, Node, NodeId, Role, TreeId};
use daedalus_proto::{PaneId, WorkspaceId};
use daedalus_ui::layout::HitTarget;
use daedalus_ui::state::{AppState, ConnectionState, WorkspaceView};

const TAB_LIST: NodeId = NodeId(18);

/// Platform requests can arrive after a pane closes. Allocate IDs once and never recycle them.
#[derive(Default)]
pub(crate) struct TerminalControls {
    serial: u64,
    run_tests: Option<(WorkspaceId, NodeId)>,
    panes: HashMap<PaneId, u64>,
}

impl TerminalControls {
    fn pane_base(&mut self, pane: PaneId) -> u64 {
        *self.panes.entry(pane).or_insert_with(|| {
            self.serial += 1;
            (1 << 61) + self.serial * 4
        })
    }

    pub(super) fn append_run_tests(
        &mut self,
        state: &AppState,
        root: &mut Node,
        nodes: &mut Vec<(NodeId, Node)>,
    ) {
        let Some(workspace) = state.active_test_workspace() else {
            self.run_tests = None;
            return;
        };
        if self.run_tests.is_none_or(|(id, _)| id != workspace) {
            self.serial += 1;
            self.run_tests = Some((workspace, NodeId((1 << 61) + self.serial * 4)));
        }
        if let Some((_, node_id)) = self.run_tests {
            let mut button = named(Role::Button, "Run tests");
            button.add_action(Action::Click);
            root.push_child(node_id);
            nodes.push((node_id, button));
        }
    }

    pub(super) fn is_run_tests(&self, state: &AppState, node: NodeId) -> bool {
        self.run_tests
            .is_some_and(|(id, target)| target == node && state.active_test_workspace() == Some(id))
    }

    pub(super) fn append_failed_runs(
        &mut self,
        state: &AppState,
        root: &mut Node,
        nodes: &mut Vec<(NodeId, Node)>,
    ) {
        self.panes.retain(|id, _| state.panes.contains_key(id));
        for ws in &state.workspaces {
            if let Some(pane) = state.failed_runner(ws) {
                let id = NodeId(self.pane_base(pane) + 3);
                let mut button = named(Role::Button, format!("Open failed run in {}", ws.name));
                button.add_action(Action::Click);
                root.push_child(id);
                nodes.push((id, button));
            }
        }
    }

    pub(super) fn failed_run_workspace(
        &self,
        state: &AppState,
        node: NodeId,
    ) -> Option<WorkspaceId> {
        state.workspaces.iter().find_map(|ws| {
            let pane = state.failed_runner(ws)?;
            let base = self.panes.get(&pane)?;
            (node == NodeId(base + 3)).then_some(ws.id)
        })
    }

    pub(super) fn append(
        &mut self,
        state: &AppState,
        ws: &WorkspaceView,
        workspace: &mut Node,
        nodes: &mut Vec<(NodeId, Node)>,
    ) -> Option<(NodeId, String)> {
        if ws.panes.is_empty() {
            return None;
        }
        let mut tabs = named(Role::TabList, "Terminals");
        let mut close = None;
        let mut terminal = None;
        let focused = state.focused_pane_id();
        for (id, label) in state.terminal_tabs(ws) {
            let base = self.pane_base(id);
            let selected = Some(id) == focused;
            let mut tab = named(Role::Tab, &label);
            tab.set_selected(selected);
            tab.add_action(Action::Click);
            tab.add_action(Action::Focus);
            tabs.push_child(NodeId(base));
            nodes.push((NodeId(base), tab));
            if selected {
                let mut button = named(Role::Button, format!("Close {label} terminal"));
                if state.connection == ConnectionState::Connected {
                    button.add_action(Action::Click);
                } else {
                    button.set_disabled();
                }
                close = Some((NodeId(base + 1), button));
                terminal = Some((NodeId(base + 2), label));
            }
        }
        workspace.push_child(TAB_LIST);
        nodes.push((TAB_LIST, tabs));
        if let Some((id, button)) = close {
            workspace.push_child(id);
            nodes.push((id, button));
        }
        terminal
    }

    pub(crate) fn action(&self, state: &AppState, request: &ActionRequest) -> Option<HitTarget> {
        if request.target_tree != TreeId::ROOT || overlay(state).is_some() {
            return None;
        }
        let ws = state.active_workspace()?;
        ws.panes.iter().copied().find_map(|pane| {
            let base = *self.panes.get(&pane)?;
            if request.target_node == NodeId(base)
                && matches!(request.action, Action::Click | Action::Focus)
            {
                Some(HitTarget::SelectSession(pane))
            } else if request.target_node == NodeId(base + 1)
                && request.action == Action::Click
                && state.connection == ConnectionState::Connected
                && state.focused_pane_id() == Some(pane)
            {
                Some(HitTarget::CloseSession(pane))
            } else {
                None
            }
        })
    }
}
