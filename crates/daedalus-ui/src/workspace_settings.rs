use super::{DaedalusApp, FALLBACK_PANE_COLS, FALLBACK_PANE_ROWS};
use daedalus_proto::{PaneId, PaneRole, WorkspaceId, WorkspaceInfo};
use daedalus_ui::{
    assignments::{self, Assignment},
    bridge::{UiCommand, WorkbenchOperation},
    project_setups::SetupMigration,
    settings::SettingsAction,
    state::{AgentPreset, WorkspaceView},
};
use winit::keyboard::Key;

fn initial_assignments(ws: &WorkspaceView, default_agent: AgentPreset) -> Vec<Assignment> {
    let manifest = ws.manifest.as_ref();
    let agent = manifest
        .and_then(|m| m.agent_preset.as_deref())
        .and_then(AgentPreset::from_cli_name)
        .unwrap_or(default_agent);
    let agent_command = manifest
        .and_then(|m| m.agent_command.clone())
        .or_else(|| agent.command());
    [
        (PaneRole::Agent, agent.short_name(), agent_command),
        (
            PaneRole::Service,
            "Service",
            manifest.and_then(|m| m.service_command.clone()),
        ),
        (
            PaneRole::Runner,
            "Runner",
            manifest.and_then(|m| m.runner_command.clone()),
        ),
    ]
    .into_iter()
    .filter_map(|(role, name, argv)| {
        Some(Assignment {
            id: format!("default-{role}"),
            name: name.into(),
            role,
            argv: argv?,
        })
    })
    .collect()
}

impl DaedalusApp {
    pub(super) fn workspace_form_key(&mut self, key: &Key) {
        if self.form_paste_key(key) {
            if let Some(text) = self.form_clipboard() {
                self.state.workspace_form.insert_text(&text);
            }
            self.request_redraw();
            return;
        }
        let action = self.state.workspace_form.key(key, self.modifiers);
        self.handle_workspace_form(action);
    }

    pub(super) fn handle_workspace_form(
        &mut self,
        action: daedalus_ui::workspace_form::WorkspaceFormAction,
    ) {
        use daedalus_ui::workspace_form::WorkspaceFormAction as Action;
        match action {
            Action::None => {}
            Action::Cancel => self.state.workspace_form.open = false,
            Action::Save { name, folder } => self.inspect_setup_folder(name, folder),
            Action::Reopen(project) => self.reopen_project(project),
        }
        self.request_redraw();
    }
    pub(super) fn open_workspace_settings(&mut self) {
        if self.state.settings.is_saving() {
            return;
        }
        let Some(ws) = self.state.active_workspace() else {
            self.show_toast("Create a workspace to configure its terminals", false);
            return;
        };
        let id = ws.id;
        let defaults = initial_assignments(ws, self.config.default_agent);
        if let Some(migration) = self
            .project_setups
            .retry_migration(&ws.root_dir, &self.state.workspaces)
        {
            self.send_setup_migration(migration);
        }
        match self.project_setups.load_or(id, defaults) {
            Ok(entries) => {
                self.state.close_search();
                self.state.action_palette.close();
                self.state.picker.close();
                self.state.settings.begin(entries);
                self.state.settings.widget_workspace = Some(id);
                self.prepare_workbench_settings(id);
                self.state.settings.set_automatic_widgets(
                    self.state.has_widget_source(PaneRole::Service),
                    self.state.has_widget_source(PaneRole::Runner),
                );
                self.state.settings.widgets = self
                    .state
                    .workspace_widgets
                    .get(&id)
                    .cloned()
                    .unwrap_or_default();
            }
            Err(err) => self.show_toast(err, true),
        }
        self.request_redraw();
    }

    pub(super) fn send_setup_migration(&mut self, migration: SetupMigration) {
        let SetupMigration {
            workspace_id,
            assignments,
            request_id,
        } = migration;
        if !self.send_command(UiCommand::SaveProjectSetup {
            workspace_id,
            assignments,
            request_id,
        }) {
            self.project_setups
                .finish_migration(workspace_id, request_id, false);
        }
    }

    pub(super) fn handle_settings_action(&mut self, action: SettingsAction) {
        if self.state.settings.is_saving() {
            return;
        }
        let (entries, launch_id) = match action {
            SettingsAction::None => {
                self.request_redraw();
                return;
            }
            SettingsAction::Cancel => {
                self.state.settings.open = false;
                self.request_redraw();
                return;
            }
            SettingsAction::Save(entries) => (entries, None),
            SettingsAction::Launch(entries, id) => (entries, Some(id)),
            SettingsAction::SaveWidgets(widgets) => {
                self.save_pane_widgets(widgets);
                self.request_redraw();
                return;
            }
            SettingsAction::Workbench { operation, request } => {
                if let Some(id) = self.state.settings.widget_workspace {
                    self.request_workbench_as(id, operation, *request);
                }
                self.request_redraw();
                return;
            }
        };
        let Some(workspace) = self.state.active_workspace_id else {
            self.state.settings.open = false;
            return;
        };
        let launch = launch_id.and_then(|id| entries.iter().find(|entry| entry.id == id).cloned());
        match self.project_setups.save(workspace, entries) {
            Ok(Some(assignments)) => {
                let request_id = self.state.settings.start_save(workspace, launch);
                if !self.send_command(UiCommand::SaveWorkspaceSettings {
                    workspace_id: workspace,
                    assignments,
                    request_id,
                }) {
                    self.state
                        .settings
                        .fail_pending_save("Reconnect to save these settings.".into());
                }
            }
            Ok(None) => self.complete_workspace_settings_save(workspace, launch),
            Err(error) => self.state.settings.error = Some(error),
        }
        self.request_redraw();
    }

    fn save_pane_widgets(&mut self, widgets: daedalus_ui::workspace_widgets::WorkspaceWidgets) {
        let Some(id) = self
            .state
            .settings
            .widget_workspace
            .filter(|id| self.state.workspaces.iter().any(|ws| ws.id == *id))
        else {
            self.state.settings.error = Some("This workspace is no longer open.".into());
            return;
        };

        if self.workbench_capable || self.state.settings.custom_supported {
            if self.state.connection != daedalus_ui::state::ConnectionState::Connected {
                self.state.settings.error = Some("Reconnect to save the widget settings.".into());
                return;
            }
            let Some((_, snapshot)) = self
                .state
                .workbench
                .as_ref()
                .filter(|(owner, _)| *owner == id)
            else {
                self.state.settings.error = Some("Workspace settings are still loading.".into());
                return;
            };
            let mut document = snapshot.document.clone();
            document.pane_widgets = Some(widgets.into());
            let revision = snapshot.revision;
            let token = self.state.settings.start_pane_widget_save(id);
            if !self.request_workbench_as(
                id,
                WorkbenchOperation::PaneWidgets(token),
                daedalus_proto::ClientMessage::UpdateWorkbench {
                    workspace_id: id,
                    revision,
                    document,
                    layout_only: false,
                    edit: Some(daedalus_proto::workbench::WorkbenchEdit::PaneWidgets),
                },
            ) {
                self.state.settings.finish_pane_widget_save(id, token);
            }
            return;
        }
        match daedalus_ui::workspace_widgets::save(&self.widget_directory, id, &widgets) {
            Ok(()) => {
                self.state.workspace_widgets.insert(id, widgets.clone());
                self.state.settings.widgets = widgets;
                self.state.settings.widgets_saved = true;
                self.schedule_resize_flush();
            }
            Err(error) => self.state.settings.error = Some(error),
        }
    }

    pub(super) fn pane_widget_save_disconnected(&mut self) {
        self.state.settings.fail_pane_widget_save();
    }

    pub(super) fn workspace_settings_saved(
        &mut self,
        workspace: WorkspaceId,
        request_id: u64,
        result: Result<(), String>,
    ) {
        let Some(launch) = self.state.settings.finish_save(workspace, request_id) else {
            return;
        };
        match result {
            Ok(()) => self.complete_workspace_settings_save(workspace, launch),
            Err(message) => self.state.settings.error = Some(message),
        }
        self.request_redraw();
    }

    pub(super) fn workspace_settings_disconnected(&mut self) {
        self.state.settings.fail_pending_save(
            "Connection lost before the daemon confirmed the save. Reconnect and check the saved settings before retrying.".into(),
        );
    }

    fn complete_workspace_settings_save(
        &mut self,
        workspace: WorkspaceId,
        launch: Option<Assignment>,
    ) {
        self.refresh_workspace_setup(workspace);
        if let Some(assignment) = launch
            && !self.launch_assignment(workspace, assignment, true)
        {
            self.state.settings.error =
                Some("Settings saved. Reconnect to launch this terminal.".into());
            return;
        }
        self.state.settings.open = false;
    }

    pub(super) fn refresh_test_runner(&mut self, workspace: WorkspaceId) {
        self.state.test_workspaces.remove(&workspace);
        match self.project_setups.load(workspace) {
            Ok(entries) if assignments::setup_runner(&entries).is_some() => {
                self.state.test_workspaces.insert(workspace);
            }
            Ok(_) => {}
            Err(error) => self.show_toast(error, true),
        }
    }

    pub(super) fn run_saved_tests(&mut self) {
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

    pub(super) fn run_cargo_preset(&mut self, preset: assignments::CargoPreset) {
        if !self.state.show_cargo_bar() {
            return;
        }
        if let Some(workspace) = self.state.active_workspace_id
            && !self.launch_assignment(workspace, preset.assignment(), false)
        {
            self.show_toast("Reconnect to run Cargo", true);
        }
    }

    fn launch_assignment(
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

    pub(super) fn settings_key(&mut self, key: &Key) {
        if self.form_paste_key(key) {
            if let Some(text) = self.form_clipboard() {
                self.state.settings.insert_text(&text);
            }
            self.request_redraw();
            return;
        }
        let action = self.state.settings.key(key, self.modifiers);
        self.handle_settings_action(action);
    }

    pub(super) fn form_paste_key(&self, key: &Key) -> bool {
        (self.modifiers.control_key() || self.modifiers.super_key())
            && matches!(key,Key::Character(c) if c.eq_ignore_ascii_case("v"))
    }

    pub(super) fn form_clipboard(&mut self) -> Option<String> {
        match self
            .ensure_clipboard()
            .and_then(|cb| cb.get_text().map_err(|err| err.to_string()))
        {
            Ok(text) => Some(text),
            Err(err) => {
                self.show_toast(err, true);
                None
            }
        }
    }

    pub(super) fn restore_session_names(&mut self, workspaces: &[WorkspaceInfo]) {
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

    pub(super) fn project_setup_changed(
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

    pub(super) fn refresh_workspace_setup(&mut self, workspace: WorkspaceId) {
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

    pub(super) fn assignment_started(
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
