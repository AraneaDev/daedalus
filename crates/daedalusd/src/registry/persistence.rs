mod restore;

use super::{Registry, WorkspaceRecord};
use crate::persistence::{
    PersistedProjectSetup, PersistedState, PersistedWorkspace, STATE_VERSION, StateStore,
};
use daedalus_proto::{Assignment, ProtoError, ServerEvent};
use std::io;
use std::path::PathBuf;
use tracing::warn;

pub(super) enum PersistenceWriteState {
    Unchecked,
    Missing,
    Loaded,
    Rejected,
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
        let (recent_projects, project_setups) = self.project_lists_for_persist().await;
        PersistedState {
            version: STATE_VERSION,
            workspaces,
            recent_projects,
            project_setups,
        }
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
        assignments: Vec<Assignment>,
    ) -> Result<(), ProtoError> {
        let mut mode = self.persist_lock.clone().lock_owned().await;
        let snapshot = if let Some(store) = self.state_store.clone() {
            let mut state = self.persistence_snapshot().await;
            let setup = PersistedProjectSetup {
                root_dir: root_dir.clone(),
                assignments: assignments.clone(),
            };
            match state
                .project_setups
                .binary_search_by(|entry| entry.root_dir.cmp(&root_dir))
            {
                Ok(index) => state.project_setups[index] = setup,
                Err(index) => state.project_setups.insert(index, setup),
            }
            Some((store, state))
        } else {
            None
        };
        let project_setups = self.project_setups.clone();
        let event_tx = self.event_tx.clone();
        // Keep the write lock, state update, and announcement together even if the
        // connection drops while the blocking file operation is in progress.
        tokio::task::spawn_blocking(move || {
            if let Some((store, state)) = snapshot {
                write_snapshot(&store, &mut mode, &state).map_err(|err| {
                    ProtoError::IoError(format!("Failed to save project setup: {err}"))
                })?;
            }
            project_setups
                .blocking_write()
                .insert(root_dir.clone(), assignments.clone());
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
