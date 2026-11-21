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

        let summary = handle.summary().await?;
        self.panes.write().await.insert(pane_id, handle);
        if let Some(ws) = self.workspaces.write().await.get_mut(&workspace_id) {
            ws.pane_ids.push(pane_id);
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
