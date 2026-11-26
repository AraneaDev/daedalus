use std::collections::HashMap;

use super::{named, overlay, workspace_controls::WorkspaceControls};
use accesskit::{Action, ActionRequest, Node, NodeId, Role, TreeId};
use daedalus_proto::{PaneId, WorkspaceId};
use daedalus_ui::assignments::CargoPreset;
use daedalus_ui::layout::HitTarget;
use daedalus_ui::state::{AppState, ConnectionState, WorkspaceView};

const TAB_LIST: NodeId = NodeId(18);

/// Platform requests can arrive after a pane closes. Allocate IDs once and never recycle them.
#[derive(Default)]
pub(crate) struct TerminalControls {
    serial: u64,
    run_tests: Option<(WorkspaceId, NodeId)>,
    cargo: Option<(WorkspaceId, NodeId)>,
    close_modal: Option<(u64, NodeId)>,
    sidebar: WorkspaceControls,
    panes: HashMap<PaneId, u64>,
    widgets: HashMap<(WorkspaceId, String), NodeId>,
}

pub(super) fn next_control_group(serial: &mut u64) -> NodeId {
    *serial += 1;
    NodeId((1 << 61) + *serial * 8)
}

impl TerminalControls {
    pub(super) fn append_sidebar(
        &mut self,
        state: &AppState,
        root: &mut Node,
        nodes: &mut Vec<(NodeId, Node)>,
    ) {
        self.sidebar.append(state, root, nodes, &mut self.serial);
    }

    pub(super) fn sidebar_action(
        &self,
        state: &AppState,
        target: NodeId,
    ) -> Option<daedalus_ui::input::UiAction> {
        self.sidebar.action(state, target)
    }

    pub(super) fn append_cargo(
        &mut self,
        state: &AppState,
        workspace: &mut Node,
        nodes: &mut Vec<(NodeId, Node)>,
    ) {
        let Some(id) = state.active_workspace_id.filter(|_| state.show_cargo_bar()) else {
            self.cargo = None;
            return;
        };
        if self.cargo.is_none_or(|(current, _)| current != id) {
            self.cargo = Some((id, next_control_group(&mut self.serial)));
        }
        if let Some((_, base)) = self.cargo {
            for (index, preset) in CargoPreset::ALL.into_iter().enumerate() {
                let node = NodeId(base.0 + index as u64);
                let mut button = named(Role::Button, preset.label());
                button.add_action(Action::Click);
                workspace.push_child(node);
                nodes.push((node, button));
            }
        }
    }

    pub(super) fn cargo_preset(&self, state: &AppState, target: NodeId) -> Option<CargoPreset> {
        let (id, base) = self.cargo?;
        if state.active_workspace_id != Some(id) || !state.show_cargo_bar() {
            return None;
        }
        CargoPreset::ALL
            .into_iter()
            .enumerate()
            .find_map(|(index, preset)| (target == NodeId(base.0 + index as u64)).then_some(preset))
    }

    pub(super) fn register_close_modal(&mut self, session: u64) -> NodeId {
        if let Some(base) = self.close_modal_base(session) {
            return base;
        }
        let base = next_control_group(&mut self.serial);
        self.close_modal = Some((session, base));
        base
    }

    pub(super) fn close_modal_base(&self, session: u64) -> Option<NodeId> {
        self.close_modal
            .filter(|(opened, _)| *opened == session)
            .map(|(_, base)| base)
    }

    pub(super) fn append_widgets(
        &mut self,
        state: &AppState,
        root: &mut Node,
        nodes: &mut Vec<(NodeId, Node)>,
    ) {
        let Some((workspace, stored)) = &state.workbench else {
            return;
        };
        if state.active_workspace_id != Some(*workspace) || stored.revision == 0 {
            return;
        }
        self.widgets.retain(|(id, widget), _| {
            *id == *workspace
                && stored
                    .document
                    .widgets
                    .iter()
                    .any(|candidate| &candidate.id == widget)
        });
        for widget in &stored.document.widgets {
            let node = *self
                .widgets
                .entry((*workspace, widget.id.clone()))
                .or_insert_with(|| next_control_group(&mut self.serial));
            let mut control = named(Role::Button, format!("Open {} widget", widget.name));
            control.add_action(Action::Click);
            root.push_child(node);
            nodes.push((node, control));
        }
    }
    fn pane_base(&mut self, pane: PaneId) -> u64 {
        *self
            .panes
            .entry(pane)
            .or_insert_with(|| next_control_group(&mut self.serial).0)
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
            self.run_tests = Some((workspace, next_control_group(&mut self.serial)));
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
        if state.zoomed
            && state.focused_role == daedalus_proto::PaneRole::Agent
            && state.widget_diagnostic().is_some()
            && let Some(runner) = state.pane_id_for_role(daedalus_proto::PaneRole::Runner)
        {
            let widget = NodeId(self.pane_base(runner) + 4);
            let mut button = named(
                Role::Button,
                state
                    .focused_widget_label(daedalus_proto::PaneRole::Runner)
                    .unwrap_or_default(),
            );
            button.add_action(Action::Click);
            workspace.push_child(widget);
            nodes.push((widget, button));
        }
        let mut tabs = named(Role::TabList, "Terminals");
        let mut close = None;
        let mut terminal = None;
        let focused = state.focused_pane_id();
        for (id, label) in state.terminal_tabs(ws) {
            let base = self.pane_base(id);
            let selected = Some(id) == focused;
            let mut tab = named(Role::Tab, &label);
            if let Some(pane) = state.panes.get(&id)
                && state.pane_id_for_role(pane.summary.role) == Some(id)
                && let Some(insight) = state.widget_label(pane.summary.role)
            {
                tab.set_description(insight);
            }
            if let Some(pane) = state.panes.get(&id)
                && let Some(indicator) = pane.tab_indicator(true)
            {
                let status = if indicator == '●' {
                    "Unread notification"
                } else {
                    "Working"
                };
                let description = tab
                    .description()
                    .map_or_else(|| status.to_owned(), |text| format!("{text}. {status}"));
                tab.set_description(description);
            }
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
        if request.action == Action::Click
            && let Some((workspace, stored)) = &state.workbench
            && Some(*workspace) == state.active_workspace_id
            && let Some(index) = stored.document.widgets.iter().position(|widget| {
                self.widgets.get(&(*workspace, widget.id.clone())) == Some(&request.target_node)
            })
        {
            return Some(HitTarget::CustomWidget(Some(index)));
        }
        if state
            .pane_id_for_role(daedalus_proto::PaneRole::Runner)
            .and_then(|id| self.panes.get(&id))
            .is_some_and(|base| request.target_node == NodeId(base + 4))
            && request.action == Action::Click
            && state.zoomed
            && state.focused_role == daedalus_proto::PaneRole::Agent
            && state.widget_diagnostic().is_some()
        {
            return Some(HitTarget::Widget(daedalus_proto::PaneRole::Runner));
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
