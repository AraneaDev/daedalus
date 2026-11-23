use std::collections::HashMap;

use super::{named, overlay};
use accesskit::{Action, ActionRequest, Node, NodeId, Role, TreeId};
use daedalus_proto::PaneId;
use daedalus_ui::layout::HitTarget;
use daedalus_ui::state::{AppState, ConnectionState, WorkspaceView};

const TAB_LIST: NodeId = NodeId(18);

/// Platform requests can arrive after a pane closes. Allocate IDs once and never recycle them.
#[derive(Default)]
pub(crate) struct TerminalControls {
    serial: u64,
    panes: HashMap<PaneId, u64>,
}

impl TerminalControls {
    pub(super) fn append(
        &mut self,
        state: &AppState,
        ws: &WorkspaceView,
        workspace: &mut Node,
        nodes: &mut Vec<(NodeId, Node)>,
    ) -> Option<(NodeId, String)> {
        self.panes.retain(|id, _| state.panes.contains_key(id));
        if ws.panes.is_empty() {
            return None;
        }
        let mut tabs = named(Role::TabList, "Terminals");
        let mut close = None;
        let mut terminal = None;
        let focused = state.focused_pane_id();
        for (id, label) in state.terminal_tabs(ws) {
            let base = *self.panes.entry(id).or_insert_with(|| {
                self.serial += 1;
                (1 << 61) + self.serial * 3
            });
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
