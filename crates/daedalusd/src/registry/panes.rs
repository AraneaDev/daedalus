use super::Registry;
use crate::pane_actor::PaneHandle;
use daedalus_proto::{Assignment, PaneId, PaneRole, PaneSummary, ProtoError, WorkspaceId};
use std::collections::{HashMap, HashSet};

const DEFAULT_ROLES: [PaneRole; 3] = [PaneRole::Agent, PaneRole::Service, PaneRole::Runner];

pub(super) enum DefaultPanes {
    Manual,
    Pending(HashSet<PaneRole>),
    Complete,
}

impl DefaultPanes {
    fn record_install(&mut self, role: PaneRole, is_default: bool) {
        if let Self::Pending(roles) = self {
            // A manually populated workspace keeps the pane arrangement its caller selected.
            if !is_default && roles.is_empty() {
                *self = Self::Complete;
            } else {
                roles.insert(role);
                if DEFAULT_ROLES.iter().all(|role| roles.contains(role)) {
                    *self = Self::Complete;
                }
            }
        }
    }

    pub(super) fn record_drained(&mut self, panes: &[PaneHandle]) {
        if panes.is_empty() {
            return;
        }
        // Cleanup can still fail. Reopen roles drained by this close while
        // preserving any roles the user had already left closed.
        let mut finished = match self {
            Self::Pending(roles) => roles.clone(),
            Self::Complete => DEFAULT_ROLES.into_iter().collect(),
            Self::Manual => return,
        };
        for pane in panes {
            finished.remove(&pane.role());
        }
        *self = Self::Pending(finished);
    }
}

impl Registry {
    #[allow(clippy::too_many_arguments)]
    pub async fn spawn_pane(
        &self,
        workspace_id: WorkspaceId,
        role: PaneRole,
        command: Option<Vec<String>>,
        shell: Option<String>,
        cols: u16,
        rows: u16,
        pane_env: HashMap<String, String>,
    ) -> Result<PaneSummary, ProtoError> {
        let _lifecycle = self.lock_workspace_lifecycle(workspace_id).await?;
        self.install_pane(
            workspace_id,
            role,
            command,
            shell,
            cols,
            rows,
            pane_env,
            None,
            false,
            None,
        )
        .await
    }

    pub(crate) async fn launch_runner_assignment(
        &self,
        workspace_id: WorkspaceId,
        assignment_id: &str,
        expected: Option<&Assignment>,
        cols: u16,
        rows: u16,
    ) -> Result<PaneSummary, ProtoError> {
        let _lifecycle = self.lock_workspace_lifecycle(workspace_id).await?;
        let assignment = self
            .workbench(workspace_id)
            .await?
            .document
            .assignments
            .into_iter()
            .find(|entry| entry.id == assignment_id && entry.role == PaneRole::Runner)
            .ok_or_else(|| {
                ProtoError::InvalidRequest(format!("No saved runner assignment '{assignment_id}'"))
            })?;
        if expected.is_some_and(|reviewed| reviewed != &assignment) {
            return Err(ProtoError::InvalidRequest(
                "The saved runner changed. Reopen Settings to review it before launching.".into(),
            ));
        }
        self.install_pane(
            workspace_id,
            PaneRole::Runner,
            Some(assignment.argv),
            None,
            cols,
            rows,
            HashMap::new(),
            None,
            false,
            Some(assignment.id),
        )
        .await
    }

    pub async fn ensure_default_pane(
        &self,
        workspace_id: WorkspaceId,
        role: PaneRole,
        command: Option<Vec<String>>,
        cols: u16,
        rows: u16,
    ) -> Result<daedalus_proto::ResponsePayload, ProtoError> {
        use daedalus_proto::ResponsePayload;
        let _lifecycle = self.lock_workspace_lifecycle(workspace_id).await?;
        let workspaces = self.workspaces.read().await;
        let ws = workspaces
            .get(&workspace_id)
            .ok_or(ProtoError::WorkspaceNotFound(workspace_id))?;
        if matches!(&ws.default_panes, DefaultPanes::Pending(done) if !done.contains(&role)) {
            drop(workspaces);
            let workbench = self.workbench(workspace_id).await?;
            let saved = (workbench.revision > 0)
                .then(|| {
                    workbench
                        .document
                        .assignments
                        .into_iter()
                        .find(|entry| entry.role == role)
                })
                .flatten();
            let assignment_id = saved
                .as_ref()
                .filter(|_| role == PaneRole::Runner)
                .map(|entry| entry.id.clone());
            let command = if let Some(saved) = saved {
                if saved.uses_default_shell() {
                    None
                } else {
                    Some(saved.argv)
                }
            } else {
                command
            };
            return self
                .install_pane(
                    workspace_id,
                    role,
                    command,
                    None,
                    cols,
                    rows,
                    HashMap::new(),
                    None,
                    true,
                    assignment_id,
                )
                .await
                .map(ResponsePayload::PaneSpawned);
        }

        let existing = {
            let panes = self.panes.read().await;
            ws.pane_ids
                .iter()
                .rev()
                .filter_map(|id| panes.get(id))
                .find(|pane| pane.role() == role)
                .cloned()
        };
        drop(workspaces);
        let summary = match existing {
            Some(handle) => Some(handle.summary().await?),
            None => None,
        };
        Ok(ResponsePayload::DefaultPaneExisting(summary))
    }

    pub async fn replace_agent_pane(
        &self,
        workspace_id: WorkspaceId,
        old_pane_id: Option<PaneId>,
        command: Option<Vec<String>>,
        cols: u16,
        rows: u16,
    ) -> Result<PaneSummary, ProtoError> {
        let _lifecycle = self.lock_workspace_lifecycle(workspace_id).await?;
        {
            let workspaces = self.workspaces.read().await;
            let ws = workspaces
                .get(&workspace_id)
                .ok_or(ProtoError::WorkspaceNotFound(workspace_id))?;
            let panes = self.panes.read().await;
            let mut agents = ws
                .pane_ids
                .iter()
                .filter(|id| panes.get(id).is_some_and(|p| p.role() == PaneRole::Agent));
            match old_pane_id {
                Some(id) if !agents.any(|agent_id| *agent_id == id) => {
                    return Err(ProtoError::PaneNotFound(id));
                }
                None if agents.next().is_some() => {
                    return Err(ProtoError::InvalidRequest(
                        "workspace already has an Agent pane".into(),
                    ));
                }
                _ => {}
            }
        }
        self.install_pane(
            workspace_id,
            PaneRole::Agent,
            command,
            None,
            cols,
            rows,
            HashMap::new(),
            old_pane_id,
            false,
            None,
        )
        .await
    }

    /// The caller holds the workspace lifecycle lock until installation completes.
    #[allow(clippy::too_many_arguments)]
    async fn install_pane(
        &self,
        workspace_id: WorkspaceId,
        role: PaneRole,
        command: Option<Vec<String>>,
        shell: Option<String>,
        cols: u16,
        rows: u16,
        pane_env: HashMap<String, String>,
        old_pane_id: Option<PaneId>,
        is_default: bool,
        assignment_id: Option<String>,
    ) -> Result<PaneSummary, ProtoError> {
        let (root_dir, mut merged_env, journal, manifest_preset) = {
            let ws_map = self.workspaces.read().await;
            let ws = ws_map
                .get(&workspace_id)
                .ok_or(ProtoError::WorkspaceNotFound(workspace_id))?;
            (
                ws.root_dir.clone(),
                ws.injected_env.clone(),
                ws.journal.clone(),
                ws.manifest
                    .as_ref()
                    .filter(|_| role == PaneRole::Agent)
                    .filter(|manifest| {
                        command.as_ref().is_some_and(|argv| {
                            !argv.is_empty() && manifest.agent_command.as_ref() == Some(argv)
                        })
                    })
                    .and_then(|manifest| manifest.agent_preset.as_deref())
                    .and_then(daedalus_proto::canonical_agent_preset),
            )
        };
        merged_env.extend(pane_env);

        let pane_id = PaneId::new();
        let launch = self
            .shell_integration
            .build_launch_spec(command.as_deref(), shell.as_deref());

        let mut spawned = self
            .supervisor
            .spawn_pty(
                workspace_id,
                pane_id,
                role,
                &root_dir,
                cols,
                rows,
                &launch,
                &merged_env,
            )
            .map_err(ProtoError::SpawnFailed)?;
        if spawned.launch_preset.is_none() {
            spawned.launch_preset = manifest_preset.map(str::to_owned);
        }

        let handle = PaneHandle::spawn_journaled(
            workspace_id,
            pane_id,
            role,
            cols,
            rows,
            spawned,
            self.event_tx.clone(),
            journal,
        )
        .with_assignment_id(assignment_id);

        let summary = match handle.summary().await {
            Ok(s) => s,
            Err(e) => {
                handle.close().await;
                return Err(e);
            }
        };

        let previous = {
            let mut workspaces = self.workspaces.write().await;
            let ws = workspaces
                .get_mut(&workspace_id)
                .expect("workspace lifecycle lock held");
            let mut panes = self.panes.write().await;
            let previous = old_pane_id.and_then(|old| {
                ws.pane_ids.retain(|id| *id != old);
                panes.remove(&old)
            });
            ws.pane_ids.push(pane_id);
            ws.default_panes.record_install(role, is_default);
            panes.insert(pane_id, handle);
            previous
        };
        self.notify_membership();
        if let Some(previous) = previous {
            previous.close().await;
        }

        Ok(summary)
    }

    pub async fn get_pane(&self, pane_id: PaneId) -> Result<PaneHandle, ProtoError> {
        self.panes
            .read()
            .await
            .get(&pane_id)
            .cloned()
            .ok_or(ProtoError::PaneNotFound(pane_id))
    }

    pub async fn close_pane(&self, pane_id: PaneId) -> Result<(), ProtoError> {
        let workspace_id = self.get_pane(pane_id).await?.workspace_id();
        let _lifecycle = self.lock_workspace_lifecycle(workspace_id).await?;
        let handle = self
            .panes
            .write()
            .await
            .remove(&pane_id)
            .ok_or(ProtoError::PaneNotFound(pane_id))?;
        if let Some(ws) = self
            .workspaces
            .write()
            .await
            .get_mut(&handle.workspace_id())
        {
            ws.pane_ids.retain(|id| *id != pane_id);
        }
        self.notify_membership();
        handle.close().await;
        Ok(())
    }
}
