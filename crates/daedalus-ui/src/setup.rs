use super::{DaedalusApp, UiCommand};
use daedalus_proto::{PaneId, PaneRole, PaneStatus, SetupInspection, WorkspaceId, WorkspaceInfo};
use daedalus_ui::{
    assignments::Assignment,
    links::{DetectedLink, open_link},
    setup_review::SetupAction,
    workspace_form::Control,
};
use std::path::PathBuf;
use winit::keyboard::Key;

enum AfterCreate {
    Install(Vec<String>),
    Apply {
        assignments: Vec<Assignment>,
        launch: Vec<String>,
    },
}

#[derive(Default)]
pub(super) struct SetupFlow {
    first_list_seen: bool,
    request: u64,
    folder_request: Option<(u64, String, PathBuf)>,
    workspace: Option<WorkspaceId>,
    after_create: Option<AfterCreate>,
    installer: Option<PaneId>,
    editing_folder: bool,
    awaiting_mutation: bool,
    installer_result: Option<String>,
}

impl DaedalusApp {
    pub(super) fn setup_installer_is_running(&self) -> bool {
        self.setup.installer.is_some()
    }
    pub(super) fn reset_setup_draft(&mut self) {
        self.setup.editing_folder = false;
        self.setup.installer_result = None;
        self.setup.workspace = None;
        self.setup.folder_request = None;
    }

    pub(super) fn offer_first_workspace(&mut self) {
        if std::mem::replace(&mut self.setup.first_list_seen, true) {
            return;
        }
        if self.state.workspaces.is_empty()
            && !self.config.bootstrap_explicit_dir
            && !self.state.workspace_form.open
            && !self.state.setup_review.open
        {
            self.create_new_workspace();
        }
    }

    pub(super) fn inspect_setup_folder(&mut self, name: String, folder: PathBuf) {
        self.setup.request = self.setup.request.wrapping_add(1);
        let session = self.setup.request;
        if self.send_command(UiCommand::InspectSetup {
            session,
            root_dir: folder.clone(),
            executables: if self.setup.editing_folder && self.state.setup_review.folder == folder {
                self.state.setup_review.executables()
            } else {
                Vec::new()
            },
        }) {
            self.setup.folder_request = Some((self.state.workspace_form.session(), name, folder));
            self.state.workspace_form.start_submission();
        } else {
            self.state.workspace_form.error = Some("Reconnect to check the project folder.".into());
        }
    }

    pub(super) fn setup_inspected(
        &mut self,
        session: u64,
        result: Result<SetupInspection, String>,
    ) {
        if session != self.setup.request {
            return;
        }
        if let Some((form, name, folder)) = self.setup.folder_request.take() {
            if !self
                .state
                .workspace_form
                .finish_submission(form, result.as_ref().err().cloned())
            {
                return;
            }
            if let Ok(info) = result {
                if self.state.setup_review.folder != folder || self.state.setup_review.name != name
                {
                    self.setup.workspace = None;
                }
                if self.setup.editing_folder && self.state.setup_review.folder == folder {
                    self.state.setup_review.name = name;
                    self.state.setup_review.open = true;
                    self.state.setup_review.refresh(info);
                } else {
                    self.state.setup_review.begin(name, folder, info);
                }
                self.setup.editing_folder = false;
            }
        } else if self.state.setup_review.open {
            self.state.setup_review.busy = false;
            match result {
                Ok(info) => {
                    self.state.setup_review.refresh(info);
                    self.state.setup_review.feedback = self
                        .setup
                        .installer_result
                        .take()
                        .unwrap_or_else(|| "Tool check complete.".into());
                }
                Err(message) => self.state.setup_review.feedback = message,
            }
        }
    }

    pub(super) fn setup_key(&mut self, key: &Key) {
        if self.form_paste_key(key) {
            if let Some(text) = self.form_clipboard() {
                self.state.setup_review.insert_text(&text);
            }
        } else {
            let action = self.state.setup_review.key(key, self.modifiers);
            self.handle_setup_action(action);
        }
        self.request_redraw();
    }

    pub(super) fn handle_setup_action(&mut self, action: SetupAction) {
        let review = &mut self.state.setup_review;
        match action {
            SetupAction::None => {}
            SetupAction::Cancel => {
                review.open = false;
                self.setup.workspace = None;
            }
            SetupAction::Back => {
                self.setup.editing_folder = true;
                review.open = false;
                self.state.workspace_form.begin(&review.folder);
                self.state
                    .workspace_form
                    .set_value(Control::Name, &review.name);
                self.state.workspace_form.set_focus(Control::Folder);
            }
            SetupAction::Help(url) => {
                if let Err(e) = open_link(&DetectedLink::Url(url), None) {
                    review.feedback = e.to_string();
                }
            }
            SetupAction::Check => {
                self.setup.request = self.setup.request.wrapping_add(1);
                let cmd = UiCommand::InspectSetup {
                    session: self.setup.request,
                    root_dir: review.folder.clone(),
                    executables: review.executables(),
                };
                self.setup_send(cmd, "Checking tools on the daemon host…");
            }
            SetupAction::Install(argv) => self.prepare_setup(AfterCreate::Install(argv)),
            SetupAction::Open => match review.commands() {
                Err(e) => review.feedback = e,
                Ok(rows) => {
                    let mut assignments = Vec::new();
                    let mut launch = Vec::new();
                    for (role, argv, start) in rows {
                        let (id, name) = match role {
                            PaneRole::Agent => ("setup-agent", "Coding agent"),
                            PaneRole::Service => ("setup-service", "Development server"),
                            PaneRole::Runner => ("setup-runner", "Run tests"),
                        };
                        assignments.push(Assignment {
                            id: id.into(),
                            name: name.into(),
                            role,
                            argv,
                        });
                        if start {
                            launch.push(id.into());
                        }
                    }
                    self.prepare_setup(AfterCreate::Apply {
                        assignments,
                        launch,
                    });
                }
            },
        }
        self.request_redraw();
    }

    fn setup_send(&mut self, cmd: UiCommand, message: &str) -> bool {
        let sent = self.send_command(cmd);
        self.state.setup_review.busy = sent;
        self.state.setup_review.feedback = if sent {
            message
        } else {
            "Reconnect and try again."
        }
        .into();
        sent
    }

    fn prepare_setup(&mut self, action: AfterCreate) {
        if self.state.setup_review.busy {
            return;
        }
        if let Some(id) = self
            .setup
            .workspace
            .filter(|id| self.state.workspaces.iter().any(|ws| ws.id == *id))
        {
            self.run_setup_action(id, action);
        } else {
            self.setup.request = self.setup.request.wrapping_add(1);
            let cmd = UiCommand::CreateManualWorkspace {
                session: self.setup.request,
                name: self.state.setup_review.name.clone(),
                root_dir: self.state.setup_review.folder.clone(),
            };
            if self.setup_send(cmd, "Creating workspace…") {
                self.setup.after_create = Some(action);
            }
        }
    }

    fn run_setup_action(&mut self, workspace_id: WorkspaceId, action: AfterCreate) {
        let cmd = match action {
            AfterCreate::Install(argv) => UiCommand::InstallSetupTool { workspace_id, argv },
            AfterCreate::Apply {
                assignments,
                launch,
            } => UiCommand::ApplySetup {
                workspace_id,
                assignments,
                launch,
            },
        };
        self.setup.awaiting_mutation = self.setup_send(cmd, "Preparing workspace…");
    }

    pub(super) fn setup_created(&mut self, session: u64, ws: WorkspaceInfo) {
        let id = ws.id;
        self.state.upsert_workspace(ws);
        if self.setup.request == session
            && let Some(action) = self.setup.after_create.take()
        {
            self.setup.workspace = Some(id);
            self.state.select_workspace(id);
            self.run_setup_action(id, action);
        }
        self.schedule_resize_flush();
    }

    pub(super) fn setup_creation_failed(&mut self, session: u64, message: String) {
        if self.setup.request == session {
            self.setup.after_create = None;
            self.state.setup_review.busy = false;
            self.state.setup_review.feedback = message;
        }
    }

    pub(super) fn setup_applied(&mut self, id: WorkspaceId, error: Option<String>) {
        self.setup.awaiting_mutation = false;
        self.refresh_test_runner(id);
        if self.setup.workspace != Some(id) {
            return;
        }
        if let Some(message) = error {
            self.setup_failed(id, message);
            return;
        }
        self.state.setup_review.busy = false;
        self.state.setup_review.open = false;
        self.setup.workspace = None;
        self.state.select_workspace(id);
        self.request_redraw();
    }

    pub(super) fn setup_failed(&mut self, id: WorkspaceId, message: String) {
        self.setup.awaiting_mutation = false;
        if self.setup.workspace == Some(id) {
            self.state.setup_review.busy = false;
            self.state.setup_review.open = true;
            self.state.setup_review.feedback = message;
        }
    }

    pub(super) fn setup_installer_started(&mut self, id: WorkspaceId, pane: PaneId) {
        if self.setup.workspace != Some(id) {
            return;
        }
        self.setup.awaiting_mutation = false;
        self.setup.installer = Some(pane);
        self.state.setup_review.open = false;
        self.state.select_workspace(id);
        if let Some(p) = self.state.panes.get(&pane) {
            let role = p.summary.role;
            let status = p.summary.status;
            self.state
                .session_names
                .insert(pane, "Setup installer".into());
            self.state.select_session(id, role, pane);
            if let PaneStatus::Exited { code } = status {
                self.setup_installer_finished(pane, code);
            }
        }
        self.schedule_resize_flush();
    }

    pub(super) fn setup_installer_finished(&mut self, pane: PaneId, code: Option<i32>) {
        if self.setup.installer != Some(pane) {
            return;
        }
        self.setup.installer = None;
        self.setup.installer_result = Some(match code {
            Some(0) => "Installer finished. Tool availability refreshed.".into(),
            Some(code) => format!("Installer exited {code}. See Setup installer for its output."),
            None => "Setup terminal closed. Tool availability refreshed.".into(),
        });
        self.state.setup_review.open = true;
        self.state.setup_review.busy = false;
        self.handle_setup_action(SetupAction::Check);
    }

    pub(super) fn setup_disconnected(&mut self) {
        self.setup.request = self.setup.request.wrapping_add(1);
        self.setup.folder_request = None;
        let uncertain = self.setup.after_create.take().is_some() || self.setup.awaiting_mutation;
        self.setup.awaiting_mutation = false;
        if uncertain {
            self.state.setup_review.open = false;
            self.state.setup_review.busy = false;
            self.setup.workspace = None;
            self.show_toast("Connection lost during setup. Check the workspace list and Settings before retrying.", true);
            return;
        }
        let installing = self.setup.installer.take().is_some();
        if self.state.setup_review.open || installing {
            self.state.setup_review.open = true;
            self.state.setup_review.busy = false;
            self.state.setup_review.feedback =
                "Connection lost. Check the workspace and tools after reconnecting.".into();
        }
    }
}
