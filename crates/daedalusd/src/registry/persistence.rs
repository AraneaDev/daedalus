use super::{Registry, WorkspaceRecord, build_workspace_record, prepare_workspace_components};
use crate::persistence::{PersistedState, PersistedWorkspace, STATE_VERSION};
use daedalus_proto::WorkspaceId;
use std::collections::{HashMap, HashSet};
use tracing::{info, warn};

impl Registry {
    pub(super) async fn persist_state(&self) {
        let Some(store) = self.state_store.clone() else {
            return;
        };
        let _persist_guard = self.persist_lock.lock().await;
        let mut workspaces: Vec<PersistedWorkspace> = {
            let ws_map = self.workspaces.read().await;
            ws_map.values().map(WorkspaceRecord::to_persisted).collect()
        };
        workspaces.extend(self.unresolved_legacy_roots.read().await.iter().cloned());
        workspaces.sort_by_key(|ws| ws.slot_index);
        let state = PersistedState {
            version: STATE_VERSION,
            workspaces,
            ..PersistedState::default()
        };
        match tokio::task::spawn_blocking(move || store.save(&state)).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                warn!(error = %err, "failed to persist daemon workspace state");
            }
            Err(err) => {
                warn!(error = %err, "state persistence task panicked");
            }
        }
    }

    /// Restores persisted workspaces from `state.json` as dormant workspaces (`panes: []`),
    /// skipping entries whose `root_dir` no longer exists and refreshing isolation, git branch,
    /// and `.daedalus.toml` manifest.
    pub async fn restore_persisted_workspaces(&self) -> usize {
        let Some(store) = self.state_store.clone() else {
            return 0;
        };

        let _create_guard = self.create_lock.lock().await;
        let load_path = store.path().to_path_buf();
        let mut persisted_state = match tokio::task::spawn_blocking(move || store.load()).await {
            Ok(Ok(state)) => state,
            Ok(Err(err)) => {
                warn!(path = %load_path.display(), error = %err, "failed to load persisted state; starting empty");
                return 0;
            }
            Err(err) => {
                warn!(path = %load_path.display(), error = %err, "state restore task panicked; starting empty");
                return 0;
            }
        };

        if persisted_state.workspaces.is_empty() {
            return 0;
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
        drop(_create_guard);

        if needs_resave {
            self.persist_state().await;
        }
        if restored_count > 0 {
            info!(
                count = restored_count,
                "restored dormant workspaces from state.json"
            );
        }

        restored_count
    }
}
