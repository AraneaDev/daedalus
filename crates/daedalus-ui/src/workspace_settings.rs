mod assignments;

use super::DaedalusApp;
use daedalus_proto::{PaneRole, WorkspaceId};
use daedalus_ui::{
    assignments::Assignment,
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
                self.state.settings.setup_baseline = self.project_setups.baseline(id);
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
        let Some(workspace) = self
            .state
            .settings
            .widget_workspace
            .or(self.state.active_workspace_id)
        else {
            self.state.settings.open = false;
            return;
        };
        let launch = launch_id.and_then(|id| entries.iter().find(|entry| entry.id == id).cloned());
        match self.project_setups.save(workspace, entries) {
            Ok(Some(assignments)) => {
                let request_id =
                    self.state
                        .settings
                        .start_save(workspace, launch, assignments.clone());
                if !self.send_command(UiCommand::SaveWorkspaceSettings {
                    workspace_id: workspace,
                    assignments,
                    expected: self.state.settings.setup_baseline.clone(),
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
        let Some(launch) = self
            .state
            .settings
            .finish_save(workspace, request_id, result.is_ok())
        else {
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
}
