use super::DaedalusApp;
use daedalus_proto::{PaneId, PaneStatus, WorkspaceId};
use daedalus_ui::setup_review::SetupAction;

#[derive(Default)]
pub(super) struct SetupInstaller {
    pane: Option<PaneId>,
    awaiting_snapshot: bool,
    pub(super) feedback: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
enum InstallerRecovery {
    Resume(WorkspaceId, PaneId),
    Finished(PaneId, Option<i32>),
}

impl SetupInstaller {
    pub(super) fn is_running(&self) -> bool {
        self.pane.is_some()
    }

    pub(super) fn wait_for_snapshot(&mut self) -> bool {
        self.awaiting_snapshot = self.is_running();
        self.awaiting_snapshot
    }

    fn reconnect(
        &mut self,
        workspace: Option<WorkspaceId>,
        status: impl FnOnce(WorkspaceId, PaneId) -> Option<PaneStatus>,
    ) -> Option<InstallerRecovery> {
        if !std::mem::take(&mut self.awaiting_snapshot) {
            return None;
        }
        let (workspace, pane) = workspace.zip(self.pane)?;
        Some(match status(workspace, pane) {
            Some(PaneStatus::Exited { code }) => InstallerRecovery::Finished(pane, code),
            None => InstallerRecovery::Finished(pane, None),
            Some(_) => InstallerRecovery::Resume(workspace, pane),
        })
    }

    fn finish(&mut self, pane: PaneId, code: Option<i32>) -> bool {
        if self.awaiting_snapshot || self.pane != Some(pane) {
            return false;
        }
        self.pane = None;
        self.feedback = Some(match code {
            Some(0) => "Installer finished. Tool availability refreshed.".into(),
            Some(code) => format!("Installer exited {code}. See Setup installer for its output."),
            None => "Setup terminal closed. Tool availability refreshed.".into(),
        });
        true
    }
}

impl DaedalusApp {
    pub(crate) fn setup_installer_is_running(&self) -> bool {
        self.setup.installer.is_running()
    }

    pub(crate) fn setup_installer_started(&mut self, id: WorkspaceId, pane: PaneId) {
        if self.setup.workspace_id() != Some(id) {
            return;
        }
        self.setup.awaiting_mutation = false;
        self.setup.installer.pane = Some(pane);
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

    pub(crate) fn reconcile_setup_installer(&mut self) {
        let workspace_id = self.setup.workspace_id();
        let recovery = self
            .setup
            .installer
            .reconnect(workspace_id, |workspace, pane| {
                self.state
                    .panes
                    .get(&pane)
                    .filter(|p| p.summary.workspace_id == workspace)
                    .map(|p| p.summary.status)
            });
        match recovery {
            Some(InstallerRecovery::Resume(workspace, pane)) => {
                self.setup_installer_started(workspace, pane)
            }
            Some(InstallerRecovery::Finished(pane, code)) => {
                self.setup_installer_finished(pane, code)
            }
            None => {}
        }
    }

    pub(crate) fn setup_installer_finished(&mut self, pane: PaneId, code: Option<i32>) {
        if !self.setup.installer.finish(pane, code) {
            return;
        }
        self.state.setup_review.open = true;
        self.state.setup_review.busy = false;
        self.handle_setup_action(SetupAction::Check);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installer_reconnect_resumes_or_completes_exactly_once() {
        let workspace = WorkspaceId::new();
        let pane = PaneId::new();
        for (status, expected) in [
            (
                Some(PaneStatus::Busy),
                InstallerRecovery::Resume(workspace, pane),
            ),
            (
                Some(PaneStatus::Exited { code: Some(7) }),
                InstallerRecovery::Finished(pane, Some(7)),
            ),
            (None, InstallerRecovery::Finished(pane, None)),
        ] {
            let mut installer = SetupInstaller {
                pane: Some(pane),
                ..Default::default()
            };
            assert!(installer.wait_for_snapshot());
            assert_eq!(installer.pane, Some(pane));
            assert!(!installer.finish(pane, None));
            assert_eq!(
                installer.reconnect(Some(workspace), |w, p| {
                    assert_eq!((w, p), (workspace, pane));
                    status
                }),
                Some(expected)
            );
            assert_eq!(
                installer.reconnect(Some(workspace), |_, _| unreachable!()),
                None
            );
            assert!(!installer.finish(PaneId::new(), None));
            assert!(installer.finish(pane, None));
            assert!(!installer.finish(pane, None));
        }
    }
}
