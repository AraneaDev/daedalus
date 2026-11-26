use super::{PersistenceWriteState, Registry, WorkspaceRecord, rejected_snapshot};
use crate::persistence::{PersistedState, PersistedWorkspace};
use daedalus_proto::WorkspaceId;
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::Path;
use tracing::{info, warn};

struct RestoreBatch {
    seen_ids: HashSet<WorkspaceId>,
    used_slots: HashSet<u16>,
    reserved_ports: HashSet<u16>,
    ready: Vec<WorkspaceRecord>,
    deferred: Vec<PersistedWorkspace>,
    needs_resave: bool,
}

impl RestoreBatch {
    fn new(workspaces: &HashMap<WorkspaceId, WorkspaceRecord>) -> Self {
        Self {
            seen_ids: workspaces.keys().copied().collect(),
            used_slots: workspaces.values().map(|ws| ws.slot_index).collect(),
            reserved_ports: workspaces
                .values()
                .flat_map(|ws| ws.reserved_ports.iter().copied())
                .collect(),
            ready: Vec::new(),
            deferred: Vec::new(),
            needs_resave: false,
        }
    }

    async fn add(&mut self, mut entry: PersistedWorkspace) {
        if self.seen_ids.contains(&entry.id) {
            self.needs_resave = true;
            return;
        }
        if entry.root_dir.is_relative() {
            warn!(workspace_id = %entry.id, root_dir = %entry.root_dir.display(),
                "legacy relative root has no recorded base; set its absolute root in state.json with the daemon stopped");
            self.seen_ids.insert(entry.id);
            self.deferred.push(entry);
            return;
        }

        let root_status = root_is_directory(&entry.root_dir).await;
        if matches!(root_status, Ok(false)) {
            info!(workspace_id = %entry.id, root_dir = %entry.root_dir.display(),
                "skipping persisted workspace whose root_dir no longer exists");
            self.needs_resave = true;
            return;
        }

        self.seen_ids.insert(entry.id);
        let Some(slot) = Registry::available_slot(&self.used_slots, Some(entry.slot_index)) else {
            warn!(workspace_id = %entry.id, "no workspace slots available during restore; deferring");
            self.deferred.push(entry);
            return;
        };
        self.used_slots.insert(slot);
        self.needs_resave |= entry.slot_index != slot;
        entry.slot_index = slot;

        if let Err(error) = root_status {
            warn!(workspace_id = %entry.id, root_dir = %entry.root_dir.display(), %error,
                "could not inspect persisted workspace root; deferring");
            self.deferred.push(entry);
            return;
        }

        match prepare_restored_workspace(&entry, HashMap::new(), self.reserved_ports.clone()).await
        {
            Ok(record) => {
                self.reserved_ports
                    .extend(record.reserved_ports.iter().copied());
                self.ready.push(record);
            }
            Err(error) => {
                warn!(workspace_id = %entry.id, root_dir = %entry.root_dir.display(), %error,
                    "failed to prepare persisted workspace; deferring");
                self.deferred.push(entry);
            }
        }
    }
}

async fn prepare_restored_workspace(
    entry: &PersistedWorkspace,
    environment: HashMap<String, String>,
    reserved_ports: HashSet<u16>,
) -> Result<WorkspaceRecord, daedalus_proto::ProtoError> {
    use daedalus_proto::ProtoError;
    let mut record =
        Registry::prepare_registered_workspace(entry, environment, reserved_ports).await?;
    if record.pending_compose_override.is_none() {
        return Ok(record);
    }
    tokio::task::spawn_blocking(move || {
        record
            .install_compose_override()
            .map_err(|error| ProtoError::IoError(error.to_string()))?;
        Ok(record)
    })
    .await
    .map_err(|error| ProtoError::Internal(format!("Workspace restoration task failed: {error}")))?
}

async fn root_is_directory(root: &Path) -> io::Result<bool> {
    match tokio::fs::metadata(root).await {
        Ok(metadata) => Ok(metadata.is_dir()),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

impl Registry {
    pub(in crate::registry) async fn find_deferred_workspace(
        &self,
        root_dir: &Path,
    ) -> Option<PersistedWorkspace> {
        let candidates: Vec<_> = self
            .deferred_workspaces
            .read()
            .await
            .iter()
            .filter(|entry| entry.root_dir.is_absolute())
            .cloned()
            .collect();
        if candidates.is_empty() {
            return None;
        }
        if let Some(entry) = candidates.iter().find(|entry| entry.root_dir == root_dir) {
            return Some(entry.clone());
        }

        let root_dir = root_dir.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let canonical = std::fs::canonicalize(root_dir).ok()?;
            candidates.into_iter().find(|entry| {
                std::fs::canonicalize(&entry.root_dir).is_ok_and(|path| path == canonical)
            })
        })
        .await
        .ok()
        .flatten()
    }

    async fn load_restore_snapshot(
        &self,
        mode: &mut PersistenceWriteState,
    ) -> io::Result<Option<PersistedState>> {
        let Some(store) = self.state_store.clone() else {
            return Ok(None);
        };
        if matches!(mode, PersistenceWriteState::Rejected) {
            return Err(rejected_snapshot(store.path()));
        }
        let path = store.path().to_path_buf();
        let loaded = tokio::task::spawn_blocking(move || store.load_snapshot())
            .await
            .map_err(io::Error::other)
            .and_then(|result| result);
        match loaded {
            Ok(snapshot) => {
                *mode = if snapshot.is_some() {
                    PersistenceWriteState::Loaded
                } else {
                    PersistenceWriteState::Missing
                };
                Ok(snapshot)
            }
            Err(err) => {
                *mode = PersistenceWriteState::Rejected;
                Err(io::Error::new(
                    err.kind(),
                    format!("failed to load {}: {err}", path.display()),
                ))
            }
        }
    }

    /// Restores runnable workspaces without panes and keeps unavailable registrations
    /// for a later retry. Roots that no longer exist are removed from the snapshot.
    pub async fn restore_persisted_workspaces(&self) -> io::Result<usize> {
        let create_guard = self.create_lock.lock().await;
        let mut mode = self.persist_lock.lock().await;
        let Some(mut state) = self.load_restore_snapshot(&mut mode).await? else {
            return Ok(0);
        };
        self.load_project_lists(&mut state).await;
        state.workspaces.sort_by_key(|ws| ws.slot_index);

        let mut batch = {
            let workspaces = self.workspaces.read().await;
            RestoreBatch::new(&workspaces)
        };
        for entry in state.workspaces {
            batch.add(entry).await;
        }

        let count = batch.ready.len();
        {
            let mut workspaces = self.workspaces.write().await;
            let mut deferred = self.deferred_workspaces.write().await;
            workspaces.extend(batch.ready.into_iter().map(|record| (record.id, record)));
            *deferred = batch.deferred;
        }
        drop(mode);
        drop(create_guard);

        if batch.needs_resave {
            self.persist_state().await;
        }
        if count > 0 {
            info!(count, "restored dormant workspaces from state.json");
        }
        Ok(count)
    }
}
