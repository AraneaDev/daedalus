use super::super::{DaedalusApp, FALLBACK_PANE_COLS, FALLBACK_PANE_ROWS};
use daedalus_proto::{PaneId, PaneRole, WorkspaceId, WorkspaceInfo};
use daedalus_ui::{
    assignments::{self, Assignment},
    bridge::UiCommand,
};

impl DaedalusApp {
    pub(crate) fn refresh_test_runner(&mut self, workspace: WorkspaceId) {
        self.state.test_workspaces.remove(&workspace);
        match self.project_setups.load(workspace) {
            Ok(entries) if assignments::setup_runner(&entries).is_some() => {
                self.state.test_workspaces.insert(workspace);
            }
            Ok(_) => {}
            Err(error) => self.show_toast(error, true),
        }
    }

    pub(crate) fn run_saved_tests(&mut self) {
        let Some(workspace_id) = self.state.active_test_workspace() else {
            self.show_toast("No test command available for this workspace", false);
            return;
        };
        let entries = match self.project_setups.load(workspace_id) {
            Ok(entries) => entries,
            Err(err) => {
                self.state.test_workspaces.remove(&workspace_id);
                self.show_toast(err, true);
                return;
            }
        };
        let Some(assignment) = assignments::setup_runner(&entries).cloned() else {
            self.state.test_workspaces.remove(&workspace_id);
            self.show_toast("Test command is no longer configured", false);
            return;
        };
        if !self.launch_assignment(workspace_id, assignment, true) {
            self.show_toast("Reconnect to run tests", true);
        }
    }

    pub(crate) fn run_cargo_preset(&mut self, preset: assignments::CargoPreset) {
        if !self.state.show_cargo_bar() {
            return;
        }
        if let Some(workspace) = self.state.active_workspace_id
            && !self.launch_assignment(workspace, preset.assignment(), false)
        {
            self.show_toast("Reconnect to run Cargo", true);
        }
    }

    pub(in crate::workspace_settings) fn launch_assignment(
        &self,
        workspace_id: WorkspaceId,
        assignment: Assignment,
        from_saved_setup: bool,
    ) -> bool {
        let (cols, rows) = self
            .compute_current_layout()
            .and_then(|layout| layout.panes.first().map(|pane| (pane.cols, pane.rows)))
            .unwrap_or((FALLBACK_PANE_COLS, FALLBACK_PANE_ROWS));
        self.send_command(UiCommand::LaunchAssignment {
            workspace_id,
            saved_runner: from_saved_setup && assignment.role == PaneRole::Runner,
            assignment,
            cols,
            rows,
        })
    }

    pub(crate) fn restore_session_names(&mut self, workspaces: &[WorkspaceInfo]) {
        for ws in workspaces {
            self.restore_workspace_names(ws.id, ws.panes.iter().map(|pane| pane.id));
        }
    }

    fn restore_workspace_names(
        &mut self,
        workspace: WorkspaceId,
        panes: impl Iterator<Item = PaneId>,
    ) {
        self.state.test_workspaces.remove(&workspace);
        let project_setups = &self.project_setups;
        match assignments::load_bindings(workspace)
            .and_then(|bindings| Ok((bindings, project_setups.load(workspace)?)))
        {
            Ok((bindings, entries)) => {
                if assignments::setup_runner(&entries).is_some() {
                    self.state.test_workspaces.insert(workspace);
                }
                for id in panes {
                    if let Some(entry) = bindings
                        .get(&id)
                        .and_then(|binding| entries.iter().find(|entry| entry.id == *binding))
                    {
                        self.state.session_names.insert(id, entry.name.clone());
                    } else {
                        self.state.session_names.remove(&id);
                    }
                }
            }
            Err(err) => self.show_toast(err, true),
        }
    }

    pub(crate) fn project_setup_changed(
        &mut self,
        root_dir: &std::path::Path,
        assignments: Vec<Assignment>,
    ) {
        self.project_setups
            .changed(root_dir, assignments, &self.state.workspaces);
        let affected: Vec<_> = self
            .state
            .workspaces
            .iter()
            .filter(|ws| ws.root_dir == root_dir)
            .map(|ws| (ws.id, ws.panes.clone()))
            .collect();
        for (id, panes) in affected {
            self.restore_workspace_names(id, panes.into_iter());
        }
        self.request_redraw();
    }

    pub(crate) fn refresh_workspace_setup(&mut self, workspace: WorkspaceId) {
        let panes = self
            .state
            .workspaces
            .iter()
            .find(|ws| ws.id == workspace)
            .map(|ws| ws.panes.clone());
        if let Some(panes) = panes {
            self.restore_workspace_names(workspace, panes.into_iter());
        }
    }

    pub(crate) fn assignment_started(
        &mut self,
        workspace: WorkspaceId,
        pane: PaneId,
        assignment: Assignment,
    ) {
        self.state.session_names.insert(pane, assignment.name);
        let cargo_preset = assignments::CargoPreset::ALL
            .into_iter()
            .find(|preset| preset.id() == assignment.id && assignment.role == PaneRole::Runner);
        if let Some(preset) = cargo_preset {
            if let Some(view) = self.state.panes.get_mut(&pane) {
                view.cargo_preset = Some(preset);
            }
            self.state
                .select_role_pane(workspace, assignment.role, pane);
        } else {
            self.state.select_session(workspace, assignment.role, pane);
            if let Err(err) = assignments::bind(workspace, pane, &assignment.id) {
                self.show_toast(err, true);
            }
        }
        self.schedule_resize_flush();
    }
}
