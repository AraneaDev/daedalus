use super::{DaedalusApp, FALLBACK_PANE_COLS, FALLBACK_PANE_ROWS};
use daedalus_proto::{PaneId, PaneRole, WorkspaceId, WorkspaceInfo};
use daedalus_ui::{
    assignments::{self, Assignment},
    bridge::UiCommand,
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
            Action::Save { name, folder } => {
                if self.send_command(UiCommand::CreateManualWorkspace {
                    session: self.state.workspace_form.session(),
                    name,
                    root_dir: folder,
                }) {
                    self.state.workspace_form.start_submission();
                } else {
                    self.state.workspace_form.error =
                        Some("Reconnect to create this workspace.".into());
                }
            }
        }
        self.request_redraw();
    }
    pub(super) fn open_workspace_settings(&mut self) {
        let Some(ws) = self.state.active_workspace() else {
            self.show_toast("Create a workspace to configure its terminals", false);
            return;
        };
        match assignments::load_or(ws.id, initial_assignments(ws, self.config.default_agent)) {
            Ok(entries) => {
                self.state.close_search();
                self.state.action_palette.close();
                self.state.picker.close();
                self.state.settings.begin(entries);
            }
            Err(err) => self.show_toast(err, true),
        }
        self.request_redraw();
    }

    pub(super) fn handle_settings_action(&mut self, action: SettingsAction) {
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
        };
        let Some(workspace) = self.state.active_workspace().cloned() else {
            self.state.settings.open = false;
            return;
        };
        if let Err(error) = assignments::save(workspace.id, &entries) {
            self.state.settings.error = Some(error);
            self.request_redraw();
            return;
        }
        self.restore_workspace_names(workspace.id, workspace.panes.iter().copied());
        if let Some(assignment) =
            launch_id.and_then(|id| entries.into_iter().find(|entry| entry.id == id))
        {
            let (cols, rows) = self
                .compute_current_layout()
                .and_then(|layout| layout.panes.first().map(|pane| (pane.cols, pane.rows)))
                .unwrap_or((FALLBACK_PANE_COLS, FALLBACK_PANE_ROWS));
            if !self.send_command(UiCommand::LaunchAssignment {
                workspace_id: workspace.id,
                assignment,
                cols,
                rows,
            }) {
                self.state.settings.error =
                    Some("Settings saved. Reconnect to launch this terminal.".into());
                self.request_redraw();
                return;
            }
        }
        self.state.settings.open = false;
        self.request_redraw();
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
        match assignments::load_bindings(workspace)
            .and_then(|bindings| Ok((bindings, assignments::load(workspace)?)))
        {
            Ok((bindings, entries)) => {
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

    pub(super) fn assignment_started(
        &mut self,
        workspace: WorkspaceId,
        pane: PaneId,
        assignment: Assignment,
    ) {
        self.state.select_session(workspace, assignment.role, pane);
        self.state.session_names.insert(pane, assignment.name);
        if let Err(err) = assignments::bind(workspace, pane, &assignment.id) {
            self.show_toast(err, true);
        }
        self.schedule_resize_flush();
    }
}
