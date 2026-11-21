use crate::pane_actor::PaneHandle;
use crate::shell_integration::ShellIntegration;
use crate::supervisor::{IsolationMode, Supervisor};
use daedalus_proto::{
    PaneId, PaneRole, PaneSummary, ProtoError, ServerEvent, WorkspaceId, WorkspaceInfo,
};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::sync::{RwLock, broadcast};

struct WorkspaceRecord {
    id: WorkspaceId,
    name: String,
    root_dir: PathBuf,
    slot_index: u16,
    injected_env: HashMap<String, String>,
    pane_ids: Vec<PaneId>,
}

pub struct Registry {
    supervisor: Supervisor,
    shell_integration: ShellIntegration,
    event_tx: broadcast::Sender<ServerEvent>,
    workspaces: RwLock<HashMap<WorkspaceId, WorkspaceRecord>>,
    panes: RwLock<HashMap<PaneId, PaneHandle>>,
}

impl Registry {
    pub fn new(
        supervisor: Supervisor,
        shell_integration: ShellIntegration,
        event_tx: broadcast::Sender<ServerEvent>,
    ) -> Self {
        Self {
            supervisor,
            shell_integration,
            event_tx,
            workspaces: RwLock::new(HashMap::new()),
            panes: RwLock::new(HashMap::new()),
        }
    }

    pub fn isolation_mode(&self) -> IsolationMode {
        self.supervisor.isolation_mode()
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<ServerEvent> {
        self.event_tx.subscribe()
    }

    pub async fn create_workspace(
        &self,
        name: String,
        root_dir: PathBuf,
        env: HashMap<String, String>,
    ) -> Result<WorkspaceInfo, ProtoError> {
        if !root_dir.is_dir() {
            return Err(ProtoError::InvalidRequest(format!(
                "root_dir does not exist or is not a directory: {}",
                root_dir.display()
            )));
        }
        let id = WorkspaceId::new();
        let record = WorkspaceRecord {
            id,
            name: name.clone(),
            root_dir: root_dir.clone(),
            slot_index: 0,
            injected_env: env.clone(),
            pane_ids: Vec::new(),
        };
        self.workspaces.write().await.insert(id, record);
        Ok(WorkspaceInfo {
            id,
            name,
            root_dir,
            slot_index: 0,
            injected_env: env,
            panes: Vec::new(),
        })
    }

    pub async fn list_workspaces(&self) -> Vec<WorkspaceInfo> {
        #[allow(clippy::type_complexity)]
        let snapshot: Vec<(
            WorkspaceId,
            String,
            PathBuf,
            u16,
            HashMap<String, String>,
            Vec<PaneHandle>,
        )> = {
            let ws_map = self.workspaces.read().await;
            let pane_map = self.panes.read().await;
            ws_map
                .values()
                .map(|ws| {
                    let handles = ws
                        .pane_ids
                        .iter()
                        .filter_map(|pid| pane_map.get(pid).cloned())
                        .collect();
                    (
                        ws.id,
                        ws.name.clone(),
                        ws.root_dir.clone(),
                        ws.slot_index,
                        ws.injected_env.clone(),
                        handles,
                    )
                })
                .collect()
        };

        let mut out = Vec::with_capacity(snapshot.len());
        for (id, name, root_dir, slot_index, injected_env, handles) in snapshot {
            let mut panes = Vec::new();
            for handle in handles {
                if let Ok(summary) = handle.summary().await {
                    panes.push(summary);
                }
            }
            out.push(WorkspaceInfo {
                id,
                name,
                root_dir,
                slot_index,
                injected_env,
                panes,
            });
        }
        out
    }

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
        let (root_dir, mut merged_env) = {
            let ws_map = self.workspaces.read().await;
            let ws = ws_map
                .get(&workspace_id)
                .ok_or(ProtoError::WorkspaceNotFound(workspace_id))?;
            (ws.root_dir.clone(), ws.injected_env.clone())
        };
        merged_env.extend(pane_env);

        let pane_id = PaneId::new();
        let launch = self
            .shell_integration
            .build_launch_spec(command.as_deref(), shell.as_deref());

        let spawned = self
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

        let handle = PaneHandle::spawn(
            workspace_id,
            pane_id,
            role,
            cols,
            rows,
            spawned,
            self.event_tx.clone(),
        );

        let summary = match handle.summary().await {
            Ok(s) => s,
            Err(e) => {
                handle.close().await;
                return Err(e);
            }
        };

        let inserted = {
            let mut ws_map = self.workspaces.write().await;
            if let Some(ws) = ws_map.get_mut(&workspace_id) {
                ws.pane_ids.push(pane_id);
                self.panes.write().await.insert(pane_id, handle.clone());
                true
            } else {
                false
            }
        };

        if !inserted {
            handle.close().await;
            return Err(ProtoError::WorkspaceNotFound(workspace_id));
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
        handle.close().await;
        Ok(())
    }

    pub async fn close_workspace(&self, workspace_id: WorkspaceId) -> Result<(), ProtoError> {
        let ws = self
            .workspaces
            .write()
            .await
            .remove(&workspace_id)
            .ok_or(ProtoError::WorkspaceNotFound(workspace_id))?;
        let mut handles = Vec::new();
        {
            let mut pane_map = self.panes.write().await;
            for pid in ws.pane_ids {
                if let Some(h) = pane_map.remove(&pid) {
                    handles.push(h);
                }
            }
        }
        for handle in handles {
            handle.close().await;
        }
        Ok(())
    }

    pub async fn close_all(&self) {
        let ws_ids: Vec<WorkspaceId> = self.workspaces.read().await.keys().copied().collect();
        for id in ws_ids {
            let _ = self.close_workspace(id).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::TempDir;

    fn test_registry(runtime_dir: &std::path::Path) -> Registry {
        let supervisor = Supervisor::with_mode(IsolationMode::PgidFallback);
        let shell_integration = ShellIntegration::prepare(runtime_dir).unwrap();
        let (event_tx, _) = broadcast::channel(256);
        Registry::new(supervisor, shell_integration, event_tx)
    }

    #[tokio::test]
    async fn workspace_create_list_and_close_lifecycle() {
        let dir = TempDir::new().unwrap();
        let registry = test_registry(dir.path());

        let missing = dir.path().join("does-not-exist");
        let err = registry
            .create_workspace("bad".to_string(), missing, HashMap::new())
            .await
            .unwrap_err();
        assert!(matches!(err, ProtoError::InvalidRequest(_)));

        let ws = registry
            .create_workspace(
                "alpha".to_string(),
                dir.path().to_path_buf(),
                HashMap::from([("WS_VAR".to_string(), "1".to_string())]),
            )
            .await
            .unwrap();
        assert_eq!(ws.name, "alpha");
        assert_eq!(ws.root_dir, dir.path());

        let listed = registry.list_workspaces().await;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, ws.id);

        registry.close_workspace(ws.id).await.unwrap();
        assert!(registry.list_workspaces().await.is_empty());

        let err = registry.close_workspace(ws.id).await.unwrap_err();
        assert!(matches!(err, ProtoError::WorkspaceNotFound(_)));
    }

    #[tokio::test]
    async fn pane_spawn_send_input_snapshot_resize_and_workspace_close() {
        let dir = TempDir::new().unwrap();
        let registry = test_registry(dir.path());
        let ws = registry
            .create_workspace("main".to_string(), dir.path().to_path_buf(), HashMap::new())
            .await
            .unwrap();

        let summary = registry
            .spawn_pane(
                ws.id,
                PaneRole::Runner,
                Some(vec!["/bin/sh".to_string()]),
                None,
                80,
                24,
                HashMap::new(),
            )
            .await
            .unwrap();
        assert_eq!(summary.cols, 80);
        assert_eq!(summary.rows, 24);

        let handle = registry.get_pane(summary.id).await.unwrap();
        handle
            .send_input(b"echo DAEDALUS_PANE_OK\n".to_vec())
            .await
            .unwrap();

        let mut saw_output = false;
        for _ in 0..40 {
            let snap = handle.snapshot().await.unwrap();
            let text: String = snap
                .rows_data
                .iter()
                .map(|r| r.cells.iter().map(|c| c.text.as_str()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n");
            if text.contains("DAEDALUS_PANE_OK") {
                saw_output = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(saw_output, "expected echoed marker in pane snapshot");

        handle.resize(100, 30).await.unwrap();
        let resized = handle.snapshot().await.unwrap();
        assert_eq!(resized.cols, 100);
        assert_eq!(resized.rows, 30);
        assert_eq!(handle.summary().await.unwrap().cols, 100);

        registry.close_workspace(ws.id).await.unwrap();
        let err = registry.get_pane(summary.id).await.unwrap_err();
        assert!(matches!(err, ProtoError::PaneNotFound(_)));
    }

    #[tokio::test]
    async fn pane_captures_exit_code_on_process_exit() {
        let dir = TempDir::new().unwrap();
        let registry = test_registry(dir.path());
        let mut events = registry.subscribe_events();
        let ws = registry
            .create_workspace("exit".to_string(), dir.path().to_path_buf(), HashMap::new())
            .await
            .unwrap();

        let summary = registry
            .spawn_pane(
                ws.id,
                PaneRole::Service,
                Some(vec![
                    "/bin/sh".to_string(),
                    "-c".to_string(),
                    "exit 42".to_string(),
                ]),
                None,
                80,
                24,
                HashMap::new(),
            )
            .await
            .unwrap();

        let exit_code = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                match events.recv().await {
                    Ok(ServerEvent::PaneExited { pane_id, exit_code }) if pane_id == summary.id => {
                        break exit_code;
                    }
                    Ok(_) => continue,
                    Err(e) => panic!("event stream error: {e}"),
                }
            }
        })
        .await
        .expect("timed out waiting for PaneExited");

        assert_eq!(exit_code, Some(42));
        registry.close_all().await;
    }

    #[tokio::test]
    async fn concurrent_spawn_pane_and_close_workspace_does_not_leak_pane() {
        let dir = TempDir::new().unwrap();
        let registry = std::sync::Arc::new(test_registry(dir.path()));

        for _ in 0..10 {
            let ws = registry
                .create_workspace("race".to_string(), dir.path().to_path_buf(), HashMap::new())
                .await
                .unwrap();

            let reg_spawn = registry.clone();
            let reg_close = registry.clone();
            let spawn_fut = reg_spawn.spawn_pane(
                ws.id,
                PaneRole::Service,
                Some(vec![
                    "/bin/sh".to_string(),
                    "-c".to_string(),
                    "sleep 5".to_string(),
                ]),
                None,
                80,
                24,
                HashMap::new(),
            );
            let close_fut = reg_close.close_workspace(ws.id);

            let (spawn_res, close_res) = tokio::join!(spawn_fut, close_fut);
            assert!(close_res.is_ok());
            match spawn_res {
                Ok(summary) => {
                    // If spawn finished before close, close_workspace must have removed it.
                    let err = registry.get_pane(summary.id).await.unwrap_err();
                    assert!(matches!(err, ProtoError::PaneNotFound(_)));
                }
                Err(ProtoError::WorkspaceNotFound(_)) => {}
                Err(other) => panic!("unexpected spawn error: {other:?}"),
            }
            assert!(registry.panes.read().await.is_empty());
        }
    }
}
