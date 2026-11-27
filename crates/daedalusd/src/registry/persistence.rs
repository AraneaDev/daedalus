mod restore;

use super::{Registry, WorkspaceRecord};
use crate::persistence::{
    PersistedProjectSetup, PersistedState, PersistedWorkspace, PersistedWorkspaceSetup,
    STATE_VERSION, StateStore,
};
use daedalus_proto::workbench::WorkbenchState;
use daedalus_proto::{Assignment, ProtoError, ServerEvent, WorkspaceId};
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::warn;

pub(super) enum PersistenceWriteState {
    Unchecked,
    Missing,
    Loaded,
    Rejected,
}

pub(super) enum WorkspaceChange {
    Register(Box<WorkspaceRecord>),
    Close(WorkspaceId),
}

fn write_snapshot(
    store: &StateStore,
    mode: &mut PersistenceWriteState,
    state: &PersistedState,
) -> io::Result<()> {
    if matches!(mode, PersistenceWriteState::Unchecked) {
        *mode = match store.load_snapshot() {
            Ok(Some(_)) => PersistenceWriteState::Loaded,
            Ok(None) => PersistenceWriteState::Missing,
            Err(err) => {
                *mode = PersistenceWriteState::Rejected;
                return Err(err);
            }
        };
    }
    if matches!(mode, PersistenceWriteState::Rejected) {
        return Err(rejected_snapshot(store.path()));
    }
    store.save(state)?;
    *mode = PersistenceWriteState::Loaded;
    Ok(())
}

fn rejected_snapshot(path: &std::path::Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "Cannot save to {} because the snapshot could not be loaded. Repair the snapshot and restart the daemon.",
            path.display()
        ),
    )
}

impl Registry {
    pub(crate) async fn persistence_health(&self) -> daedalus_proto::PersistenceHealth {
        use daedalus_proto::PersistenceHealth;
        if self.state_store.is_none() {
            return PersistenceHealth::NotConfigured;
        }
        match *self.persist_lock.lock().await {
            PersistenceWriteState::Rejected => PersistenceHealth::TemporarySnapshotRejected,
            _ => PersistenceHealth::Durable,
        }
    }

    /// Persists the proposed lifecycle change before publishing it to any client.
    pub(super) async fn commit_workspace_change(
        &self,
        change: WorkspaceChange,
    ) -> Result<(), ProtoError> {
        let mut mode = self.persist_lock.clone().lock_owned().await;
        let (id, name, root, canonical_root) = {
            let workspaces = self.workspaces.read().await;
            let record = match &change {
                WorkspaceChange::Register(record) => record.as_ref(),
                WorkspaceChange::Close(id) => workspaces
                    .get(id)
                    .ok_or(ProtoError::WorkspaceNotFound(*id))?,
            };
            (
                record.id,
                record.name.clone(),
                record.root_dir.clone(),
                record.canonical_root_dir.clone(),
            )
        };
        let recent = self
            .recent_projects_after_contact(&name, &root, &canonical_root)
            .await;
        let mut state = self.persistence_snapshot().await;
        let previous = match &change {
            WorkspaceChange::Register(record)
                if record.pending_compose_override.is_some()
                    && self.state_store.is_some()
                    && !matches!(*mode, PersistenceWriteState::Rejected) =>
            {
                Some(state.clone())
            }
            _ => None,
        };
        state.workspaces.retain(|workspace| workspace.id != id);
        match &change {
            WorkspaceChange::Register(record) => {
                state.workspaces.push(record.to_persisted());
                state
                    .workspaces
                    .sort_by_key(|workspace| workspace.slot_index);
            }
            WorkspaceChange::Close(_) => state
                .workspace_setups
                .retain(|setup| setup.workspace_id != id),
        }
        state.recent_projects.clone_from(&recent);

        let store = self.state_store.clone();
        let mut workspaces = self.workspaces.clone().write_owned().await;
        let mut deferred = self.deferred_workspaces.clone().write_owned().await;
        let workbench_store = self.workbench_store.clone();
        let project_setups = Arc::clone(&self.project_setups);
        let recent_projects = Arc::clone(&self.recent_projects);
        let event_tx = self.event_tx.clone();
        // Hold publication and write guards through the blocking save so the
        // in-memory result follows a committed write even if the caller leaves.
        tokio::task::spawn_blocking(move || {
            if let Some(store) = &store {
                if matches!(*mode, PersistenceWriteState::Rejected) {
                    warn!(workspace = %id, "workspace change is temporary because the startup snapshot was rejected");
                } else {
                    write_snapshot(store, &mut mode, &state)
                        .map_err(|err| ProtoError::IoError(format!("Failed to save workspace state: {err}")))?;
                }
            }
            match change {
                WorkspaceChange::Register(mut record) => {
                    if let Err(error) = record.install_compose_override() {
                        let message = format!("Failed to write the workspace Compose override: {error}");
                        if let (Some(store), Some(previous)) = (&store, &previous)
                            && let Err(restore) = store.save(previous)
                        {
                            return Err(ProtoError::IoError(format!(
                                "{message}. Failed to restore the previous workspace snapshot at {}: {restore}",
                                store.path().display()
                            )));
                        }
                        return Err(ProtoError::IoError(message));
                    }
                    workspaces.insert(id, *record);
                    deferred.retain(|entry| entry.id != id);
                }
                WorkspaceChange::Close(_) => {
                    if let Some(record) = workspaces.remove(&id) {
                        record.widget_commands.close();
                        workbench_store.evict_workspace(id);
                    }
                    project_setups.blocking_write().forget_workspace(id);
                }
            }
            *recent_projects.blocking_write() = recent;
            drop(deferred);
            drop(workspaces);
            let _ = event_tx.send(ServerEvent::WorkspaceMembershipChanged);
            drop(mode);
            Ok(())
        })
        .await
        .map_err(|err| ProtoError::Internal(format!("Workspace state save task failed: {err}")))?
    }

    async fn persistence_snapshot(&self) -> PersistedState {
        let mut workspaces: Vec<PersistedWorkspace> = {
            let ws_map = self.workspaces.read().await;
            // Read both collections together so a retry cannot move a registration
            // between them halfway through the snapshot.
            let deferred = self.deferred_workspaces.read().await;
            ws_map
                .values()
                .map(WorkspaceRecord::to_persisted)
                .chain(deferred.iter().cloned())
                .collect()
        };
        workspaces.sort_by_key(|ws| ws.slot_index);
        let mut snapshot = PersistedState {
            version: STATE_VERSION,
            workspaces,
            recent_projects: self.recent_projects.read().await.clone(),
            ..PersistedState::default()
        };
        self.project_setups.read().await.save_into(&mut snapshot);
        snapshot
    }

    pub(super) async fn persist_state(&self) {
        let Some(store) = self.state_store.clone() else {
            return;
        };
        let mut mode = self.persist_lock.clone().lock_owned().await;
        let state = self.persistence_snapshot().await;
        match tokio::task::spawn_blocking(move || write_snapshot(&store, &mut mode, &state)).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                warn!(error = %err, "failed to persist daemon workspace state");
            }
            Err(err) => {
                warn!(error = %err, "state persistence task panicked");
            }
        }
    }

    pub(super) async fn persist_project_setup(
        &self,
        root_dir: PathBuf,
        canonical_root: PathBuf,
        assignments: Vec<Assignment>,
        versioned: Option<(WorkspaceId, WorkbenchState)>,
        lifecycle: tokio::sync::OwnedMutexGuard<()>,
        mut mode: tokio::sync::OwnedMutexGuard<PersistenceWriteState>,
    ) -> Result<(), ProtoError> {
        let workspace_setup = versioned
            .as_ref()
            .map(|(id, state)| PersistedWorkspaceSetup {
                workspace_id: *id,
                revision: state.revision,
                assignments: assignments.clone(),
            });
        let runtime = if let Some((id, state)) = versioned {
            Some((id, state, self.workbench_widget_commands(id).await?))
        } else {
            None
        };
        let snapshot = if let Some(store) = self.state_store.clone() {
            let mut state = self.persistence_snapshot().await;
            let setup = PersistedProjectSetup {
                root_dir: canonical_root.clone(),
                assignments: assignments.clone(),
            };
            match state
                .project_setups
                .binary_search_by(|entry| entry.root_dir.cmp(&canonical_root))
            {
                Ok(index) => state.project_setups[index] = setup,
                Err(index) => state.project_setups.insert(index, setup),
            }
            if let Some(setup) = &workspace_setup {
                match state
                    .workspace_setups
                    .binary_search_by_key(&setup.workspace_id.0, |entry| entry.workspace_id.0)
                {
                    Ok(index) => state.workspace_setups[index] = setup.clone(),
                    Err(index) => state.workspace_setups.insert(index, setup.clone()),
                }
            }
            Some((store, state))
        } else {
            None
        };
        let project_setups = self.project_setups.clone();
        let event_tx = self.event_tx.clone();
        // Keep the write locks, state update, and announcement together even if the
        // connection drops while the blocking file operation is in progress.
        tokio::task::spawn_blocking(move || {
            if let Some((store, state)) = snapshot {
                write_snapshot(&store, &mut mode, &state).map_err(|err| {
                    ProtoError::IoError(format!("Failed to save project setup: {err}"))
                })?;
            }
            let _lifecycle = lifecycle;
            project_setups.blocking_write().confirm(
                canonical_root,
                assignments.clone(),
                workspace_setup,
            );
            if let Some((id, state, widget_commands)) = runtime {
                Self::publish_workbench_change(&event_tx, id, &widget_commands, &state);
            }
            let _ = event_tx.send(ServerEvent::ProjectSetupChanged {
                root_dir,
                assignments,
            });
            drop(mode);
            Ok(())
        })
        .await
        .map_err(|err| ProtoError::Internal(format!("Project setup save task failed: {err}")))?
    }
}
