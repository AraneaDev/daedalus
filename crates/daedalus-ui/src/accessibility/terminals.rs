use std::collections::HashMap;

use super::{named, overlay};
use accesskit::{Action, ActionRequest, Node, NodeId, Role, TreeId};
use daedalus_proto::{PaneId, WorkspaceId};
use daedalus_ui::layout::HitTarget;
use daedalus_ui::state::{AppState, ConnectionState, WorkspaceView};

const TAB_LIST: NodeId = NodeId(18);
const WORKSPACE_LIST: NodeId = NodeId(23);

/// Platform requests can arrive after a pane closes. Allocate IDs once and never recycle them.
#[derive(Default)]
pub(crate) struct TerminalControls {
    serial: u64,
    run_tests: Option<(WorkspaceId, NodeId)>,
    workspaces: HashMap<WorkspaceId, NodeId>,
    panes: HashMap<PaneId, u64>,
    widgets: HashMap<(WorkspaceId, String), NodeId>,
}

impl TerminalControls {
    pub(super) fn append_workspaces(
        &mut self,
        state: &AppState,
        root: &mut Node,
        nodes: &mut Vec<(NodeId, Node)>,
    ) {
        self.workspaces
            .retain(|id, _| state.workspaces.iter().any(|workspace| workspace.id == *id));
        if state.workspaces.is_empty() {
            return;
        }
        let mut list = named(Role::ListBox, "Workspaces");
        for workspace in state.workspaces_in_sidebar_order() {
            let id = *self.workspaces.entry(workspace.id).or_insert_with(|| {
                self.serial += 1;
                NodeId((1 << 61) + self.serial * 8)
            });
            let mut duplicates = state
                .workspaces
                .iter()
                .filter(|other| other.id != workspace.id && other.name == workspace.name)
                .peekable();
            let label = if duplicates.peek().is_some() {
                let mut label = format!("{} · {}", workspace.name, workspace.root_dir.display());
                if duplicates.any(|other| other.root_dir == workspace.root_dir) {
                    label.push_str(&format!(" · {}", workspace.id.0.simple()));
                }
                label
            } else {
                workspace.name.clone()
            };
            let mut option = named(Role::ListBoxOption, label);
            option.set_selected(state.active_workspace_id == Some(workspace.id));
            option.add_action(Action::Click);
            list.push_child(id);
            nodes.push((id, option));
        }
        root.push_child(WORKSPACE_LIST);
        nodes.push((WORKSPACE_LIST, list));
    }

    pub(super) fn workspace_for_node(
        &self,
        state: &AppState,
        target: NodeId,
    ) -> Option<WorkspaceId> {
        state.workspaces.iter().find_map(|workspace| {
            (self.workspaces.get(&workspace.id) == Some(&target)).then_some(workspace.id)
        })
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
                .or_insert_with(|| {
                    self.serial += 1;
                    NodeId((1 << 61) + self.serial * 8)
                });
            let mut control = named(Role::Button, format!("Open {} widget", widget.name));
            control.add_action(Action::Click);
            root.push_child(node);
            nodes.push((node, control));
        }
    }
    fn pane_base(&mut self, pane: PaneId) -> u64 {
        *self.panes.entry(pane).or_insert_with(|| {
            self.serial += 1;
            (1 << 61) + self.serial * 8
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
            self.run_tests = Some((workspace, NodeId((1 << 61) + self.serial * 8)));
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
