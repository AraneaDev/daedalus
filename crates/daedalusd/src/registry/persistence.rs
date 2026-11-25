use super::{Registry, WorkspaceRecord, build_workspace_record, prepare_workspace_components};
use crate::persistence::{
    PersistedProjectSetup, PersistedState, PersistedWorkspace, STATE_VERSION, StateStore,
};
use daedalus_proto::{Assignment, ProtoError, ServerEvent, WorkspaceId};
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::PathBuf;
use tracing::{info, warn};

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
            ws_map.values().map(WorkspaceRecord::to_persisted).collect()
        };
        workspaces.extend(self.unresolved_legacy_roots.read().await.iter().cloned());
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

    /// Restores persisted workspaces from `state.json` as dormant workspaces (`panes: []`),
    /// skipping entries whose `root_dir` no longer exists and refreshing isolation, git branch,
    /// and `.daedalus.toml` manifest.
    pub async fn restore_persisted_workspaces(&self) -> io::Result<usize> {
        let Some(store) = self.state_store.clone() else {
            return Ok(0);
        };

        let create_guard = self.create_lock.lock().await;
        let mut mode = self.persist_lock.lock().await;
        if matches!(*mode, PersistenceWriteState::Rejected) {
            return Err(rejected_snapshot(store.path()));
        }
        let load_path = store.path().to_path_buf();
        let loaded = tokio::task::spawn_blocking(move || store.load_snapshot())
            .await
            .map_err(io::Error::other)
            .and_then(|result| result);
        let mut persisted_state = match loaded {
            Ok(Some(state)) => {
                *mode = PersistenceWriteState::Loaded;
                state
            }
            Ok(None) => {
                *mode = PersistenceWriteState::Missing;
                return Ok(0);
            }
            Err(err) => {
                *mode = PersistenceWriteState::Rejected;
                return Err(io::Error::new(
                    err.kind(),
                    format!("failed to load {}: {err}", load_path.display()),
                ));
            }
        };

        self.load_project_lists(&mut persisted_state).await;

        if persisted_state.workspaces.is_empty() {
            return Ok(0);
        }

        persisted_state.workspaces.sort_by_key(|ws| ws.slot_index);

        let (mut used_slots, mut reserved_ports, mut existing_ids) = {
            let ws_map = self.workspaces.read().await;
            let used_slots: HashSet<u16> = ws_map.values().map(|ws| ws.slot_index).collect();
            let reserved_ports: HashSet<u16> = ws_map
                .values()
                .flat_map(|ws| ws.reserved_ports.iter().copied())
                .collect();
            let existing_ids: HashSet<WorkspaceId> = ws_map.keys().copied().collect();
            (used_slots, reserved_ports, existing_ids)
        };

        let mut unresolved = Vec::new();
        let mut restored_records = Vec::new();
        let mut needs_resave = false;

        for entry in persisted_state.workspaces {
            if existing_ids.contains(&entry.id) {
                needs_resave = true;
                continue;
            }
            if entry.root_dir.is_relative() {
                warn!(workspace_id = %entry.id, root_dir = %entry.root_dir.display(),
                    "legacy relative root has no recorded base; set its absolute root in state.json with the daemon stopped");
                unresolved.push(entry);
                continue;
            }
            if !entry.root_dir.is_dir() {
                info!(
                    workspace_id = %entry.id,
                    root_dir = %entry.root_dir.display(),
                    "skipping persisted workspace whose root_dir no longer exists"
                );
                needs_resave = true;
                continue;
            }

            let slot_index = if !used_slots.contains(&entry.slot_index) {
                entry.slot_index
            } else if let Some(slot) = (0..=u16::MAX).find(|s| !used_slots.contains(s)) {
                needs_resave = true;
                slot
            } else {
                warn!(workspace_id = %entry.id, "no workspace slots available during restore");
                needs_resave = true;
                continue;
            };

            let name_clone = entry.name.clone();
            let root_clone = entry.root_dir.clone();
            let reserved_clone = reserved_ports.clone();
            let prepared_res = tokio::task::spawn_blocking(move || {
                prepare_workspace_components(
                    &name_clone,
                    &root_clone,
                    slot_index,
                    HashMap::new(),
                    &reserved_clone,
                )
            })
            .await;

            let prepared = match prepared_res {
                Ok(Ok(p)) => p,
                Ok(Err(err)) => {
                    warn!(
                        workspace_id = %entry.id,
                        root_dir = %entry.root_dir.display(),
                        error = %err,
                        "failed to prepare isolation for persisted workspace; skipping"
                    );
                    needs_resave = true;
                    continue;
                }
                Err(err) => {
                    warn!(workspace_id = %entry.id, error = %err, "workspace restore task panicked");
                    needs_resave = true;
                    continue;
                }
            };

            let record = build_workspace_record(
                entry.id,
                entry.name,
                entry.root_dir,
                prepared,
                entry.manual_launch,
            );
            used_slots.insert(record.slot_index);
            reserved_ports.extend(record.reserved_ports.iter().copied());
            existing_ids.insert(record.id);
            restored_records.push(record);
        }

        *self.unresolved_legacy_roots.write().await = unresolved;
        let restored_count = restored_records.len();
        if restored_count > 0 {
            let mut ws_map = self.workspaces.write().await;
            for record in restored_records {
                ws_map.insert(record.id, record);
            }
        }
        drop(mode);
        drop(create_guard);

        if needs_resave {
            self.persist_state().await;
        }
        if restored_count > 0 {
            info!(
                count = restored_count,
                "restored dormant workspaces from state.json"
            );
        }

        Ok(restored_count)
    }
}
