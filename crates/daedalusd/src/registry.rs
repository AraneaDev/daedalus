use crate::isolation;
use crate::pane_actor::PaneHandle;
use crate::runtime_monitor::WorkspaceMonitorTarget;
use crate::shell_integration::ShellIntegration;
use crate::supervisor::{IsolationMode, Supervisor};
use crate::worktree;
use daedalus_proto::{
    ContainerHealth, ExternalStatus, GitWorktreeInfo, PaneId, PaneRole, PaneSummary, PortMapping,
    ProtoError, ServerEvent, StatusLevel, WorkspaceId, WorkspaceInfo,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, RwLock, broadcast};
use tracing::warn;

const MAX_EXTERNAL_STATUSES: usize = 16;

struct WorkspaceRecord {
    id: WorkspaceId,
    name: String,
    root_dir: PathBuf,
    canonical_root_dir: PathBuf,
    slot_index: u16,
    port_offset: u16,
    compose_project_name: Option<String>,
    git_branch: Option<String>,
    injected_env: HashMap<String, String>,
    reserved_ports: HashSet<u16>,
    pane_ids: Vec<PaneId>,
    bound_ports: Vec<PortMapping>,
    containers: Vec<ContainerHealth>,
    external_statuses: Vec<ExternalStatus>,
}

impl WorkspaceRecord {
    fn to_info(&self, panes: Vec<PaneSummary>) -> WorkspaceInfo {
        WorkspaceInfo {
            id: self.id,
            name: self.name.clone(),
            root_dir: self.root_dir.clone(),
            slot_index: self.slot_index,
            injected_env: self.injected_env.clone(),
            panes,
            port_offset: self.port_offset,
            compose_project_name: self.compose_project_name.clone(),
            git_branch: self.git_branch.clone(),
            bound_ports: self.bound_ports.clone(),
            containers: self.containers.clone(),
            external_statuses: self.external_statuses.clone(),
        }
    }
}

pub struct Registry {
    supervisor: Supervisor,
    shell_integration: ShellIntegration,
    event_tx: broadcast::Sender<ServerEvent>,
    create_lock: Mutex<()>,
    worktrees_dir: std::sync::RwLock<Option<PathBuf>>,
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
            create_lock: Mutex::new(()),
            worktrees_dir: std::sync::RwLock::new(None),
            workspaces: RwLock::new(HashMap::new()),
            panes: RwLock::new(HashMap::new()),
        }
    }

    pub fn set_worktrees_dir(&self, dir: PathBuf) {
        if let Ok(mut guard) = self.worktrees_dir.write() {
            *guard = Some(dir);
        }
    }

    fn worktrees_base_dir(&self) -> PathBuf {
        if let Ok(guard) = self.worktrees_dir.read()
            && let Some(dir) = guard.as_ref()
        {
            return dir.clone();
        }
        worktree::default_worktrees_dir()
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

        let _create_guard = self.create_lock.lock().await;
        let (slot_index, reserved_ports) = {
            let ws_map = self.workspaces.read().await;
            let used_slots: HashSet<u16> = ws_map.values().map(|ws| ws.slot_index).collect();
            let slot_index = (0..=u16::MAX)
                .find(|slot| !used_slots.contains(slot))
                .ok_or_else(|| ProtoError::Internal("no workspace slots available".to_string()))?;
            let reserved_ports: HashSet<u16> = ws_map
                .values()
                .flat_map(|ws| ws.reserved_ports.iter().copied())
                .collect();
            (slot_index, reserved_ports)
        };

        let name_clone = name.clone();
        let root_clone = root_dir.clone();
        let env_clone = env.clone();
        let (plan_res, git_branch, canonical_root_dir) = tokio::task::spawn_blocking(move || {
            let plan = isolation::prepare_workspace_isolation_with_reserved(
                &name_clone,
                &root_clone,
                slot_index,
                &env_clone,
                &reserved_ports,
            );
            let branch = worktree::current_branch(&root_clone);
            let canonical = std::fs::canonicalize(&root_clone).unwrap_or(root_clone);
            (plan, branch, canonical)
        })
        .await
        .map_err(|e| ProtoError::Internal(format!("workspace preparation task failed: {e}")))?;

        let plan = plan_res.map_err(|e| ProtoError::IoError(e.to_string()))?;
        let reserved_ports: HashSet<u16> = plan.remapped_ports.values().copied().collect();

        let id = WorkspaceId::new();
        let record = WorkspaceRecord {
            id,
            name,
            root_dir,
            canonical_root_dir,
            slot_index: plan.slot_index,
            port_offset: plan.port_offset,
            compose_project_name: plan.compose_project_name,
            git_branch,
            injected_env: plan.injected_env,
            reserved_ports,
            pane_ids: Vec::new(),
            bound_ports: Vec::new(),
            containers: Vec::new(),
            external_statuses: Vec::new(),
        };
        let info = record.to_info(Vec::new());
        self.workspaces.write().await.insert(id, record);
        Ok(info)
    }

    pub async fn discover_worktrees(
        &self,
        repo_dir: PathBuf,
    ) -> Result<Vec<GitWorktreeInfo>, ProtoError> {
        if !repo_dir.is_dir() {
            return Err(ProtoError::InvalidRequest(format!(
                "repo_dir does not exist or is not a directory: {}",
                repo_dir.display()
            )));
        }

        tokio::task::spawn_blocking(move || worktree::discover_worktrees(&repo_dir))
            .await
            .map_err(|e| ProtoError::Internal(format!("worktree discovery task failed: {e}")))?
            .map_err(ProtoError::InvalidRequest)
    }

    pub async fn create_worktree_workspace(
        &self,
        repo_dir: PathBuf,
        branch: String,
        create_branch: bool,
        env: HashMap<String, String>,
    ) -> Result<WorkspaceInfo, ProtoError> {
        if !repo_dir.is_dir() {
            return Err(ProtoError::InvalidRequest(format!(
                "repo_dir does not exist or is not a directory: {}",
                repo_dir.display()
            )));
        }

        let base_dir = self.worktrees_base_dir();
        let branch_clone = branch.clone();
        let wt = tokio::task::spawn_blocking(move || {
            worktree::create_worktree(&repo_dir, &branch_clone, create_branch, &base_dir)
        })
        .await
        .map_err(|e| ProtoError::Internal(format!("worktree creation task failed: {e}")))?
        .map_err(ProtoError::InvalidRequest)?;

        let name = wt
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| wt.branch.clone())
            .unwrap_or(branch);

        self.create_workspace(name, wt.path, env).await
    }

    pub async fn list_workspaces(&self) -> Vec<WorkspaceInfo> {
        let mut snapshot: Vec<(WorkspaceInfo, Vec<PaneHandle>)> = {
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
                    (ws.to_info(Vec::new()), handles)
                })
                .collect()
        };

        snapshot.sort_by_key(|(info, _)| info.slot_index);

        let mut out = Vec::with_capacity(snapshot.len());
        for (mut info, handles) in snapshot {
            let mut panes = Vec::new();
            for handle in handles {
                if let Ok(summary) = handle.summary().await {
                    panes.push(summary);
                }
            }
            info.panes = panes;
            out.push(info);
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

    pub async fn close_workspace(
        &self,
        workspace_id: WorkspaceId,
        compose_down: bool,
    ) -> Result<(), ProtoError> {
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

        if compose_down && let Some(project) = ws.compose_project_name {
            let root_dir = ws.root_dir;
            let project_for_task = project.clone();
            let res = tokio::task::spawn_blocking(move || {
                isolation::run_compose_down(&root_dir, &project_for_task)
            })
            .await;
            match res {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    warn!(
                        workspace_id = %workspace_id,
                        project = %project,
                        error = %err,
                        "docker compose down failed during workspace close"
                    );
                }
                Err(err) => {
                    warn!(
                        workspace_id = %workspace_id,
                        project = %project,
                        error = %err,
                        "docker compose down task panicked"
                    );
                }
            }
        }

        Ok(())
    }

    pub async fn close_all(&self) {
        let ws_ids: Vec<WorkspaceId> = self.workspaces.read().await.keys().copied().collect();
        for id in ws_ids {
            let _ = self.close_workspace(id, false).await;
        }
    }

    pub async fn publish_external_status(
        &self,
        workspace_id: Option<WorkspaceId>,
        root_dir: Option<PathBuf>,
        source: String,
        level: StatusLevel,
        label: String,
        detail: Option<String>,
    ) -> Result<ExternalStatus, ProtoError> {
        if source.trim().is_empty() {
            return Err(ProtoError::InvalidRequest(
                "external status source cannot be empty".to_string(),
            ));
        }
        if label.trim().is_empty() {
            return Err(ProtoError::InvalidRequest(
                "external status label cannot be empty".to_string(),
            ));
        }
        if workspace_id.is_none() && root_dir.is_none() {
            return Err(ProtoError::InvalidRequest(
                "either workspace_id or root_dir must be provided".to_string(),
            ));
        }

        let canonical_target = match (&workspace_id, &root_dir) {
            (None, Some(dir)) => {
                let dir_clone = dir.clone();
                tokio::task::spawn_blocking(move || std::fs::canonicalize(&dir_clone).ok())
                    .await
                    .ok()
                    .flatten()
            }
            _ => None,
        };

        let updated_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0);

        let status = ExternalStatus {
            source,
            level,
            label,
            detail,
            updated_at_ms,
        };

        let target_id = {
            let mut ws_map = self.workspaces.write().await;
            let resolved_id = if let Some(id) = workspace_id {
                if !ws_map.contains_key(&id) {
                    return Err(ProtoError::WorkspaceNotFound(id));
                }
                id
            } else {
                let target_dir = root_dir
                    .as_deref()
                    .expect("checked workspace_id.is_none() && root_dir.is_none() above");
                find_longest_prefix_workspace(&ws_map, target_dir, canonical_target.as_deref())
                    .ok_or_else(|| {
                        ProtoError::InvalidRequest(format!(
                            "no workspace matches root_dir: {}",
                            target_dir.display()
                        ))
                    })?
            };

            let ws = ws_map
                .get_mut(&resolved_id)
                .ok_or(ProtoError::WorkspaceNotFound(resolved_id))?;

            if let Some(pos) = ws
                .external_statuses
                .iter()
                .position(|existing| existing.source == status.source)
            {
                ws.external_statuses.remove(pos);
            } else if ws.external_statuses.len() >= MAX_EXTERNAL_STATUSES {
                ws.external_statuses.remove(0);
            }
            ws.external_statuses.push(status.clone());
            resolved_id
        };

        let _ = self.event_tx.send(ServerEvent::ExternalStatusUpdated {
            workspace_id: target_id,
            status: status.clone(),
        });

        Ok(status)
    }

    pub async fn monitor_targets(&self) -> Vec<WorkspaceMonitorTarget> {
        let ws_map = self.workspaces.read().await;
        let pane_map = self.panes.read().await;
        ws_map
            .values()
            .map(|ws| {
                let root_pids = ws
                    .pane_ids
                    .iter()
                    .filter_map(|pid| pane_map.get(pid))
                    .filter_map(|handle| handle.root_pid())
                    .collect();
                WorkspaceMonitorTarget {
                    workspace_id: ws.id,
                    compose_project_name: ws.compose_project_name.clone(),
                    root_pids,
                }
            })
            .collect()
    }

    pub async fn update_workspace_runtime(
        &self,
        workspace_id: WorkspaceId,
        bound_ports: Vec<PortMapping>,
        containers: Vec<ContainerHealth>,
    ) -> bool {
        let changed = {
            let mut ws_map = self.workspaces.write().await;
            let Some(ws) = ws_map.get_mut(&workspace_id) else {
                return false;
            };
            if ws.bound_ports == bound_ports && ws.containers == containers {
                return false;
            }
            ws.bound_ports = bound_ports.clone();
            ws.containers = containers.clone();
            true
        };

        if changed {
            let _ = self.event_tx.send(ServerEvent::WorkspaceRuntimeUpdated {
                workspace_id,
                bound_ports,
                containers,
            });
        }
        changed
    }
}

fn find_longest_prefix_workspace(
    ws_map: &HashMap<WorkspaceId, WorkspaceRecord>,
    target_dir: &Path,
    canonical_target: Option<&Path>,
) -> Option<WorkspaceId> {
    let mut best: Option<(usize, u16, WorkspaceId)> = None;
    for ws in ws_map.values() {
        let match_len = if target_dir.starts_with(&ws.root_dir) {
            Some(ws.root_dir.components().count())
        } else if let Some(canon_target) = canonical_target
            && canon_target.starts_with(&ws.canonical_root_dir)
        {
            Some(ws.canonical_root_dir.components().count())
        } else {
            None
        };

        if let Some(len) = match_len {
            let replace = match best {
                None => true,
                Some((best_len, best_slot, _)) => {
                    len > best_len || (len == best_len && ws.slot_index < best_slot)
                }
            };
            if replace {
                best = Some((len, ws.slot_index, ws.id));
            }
        }
    }
    best.map(|(_, _, id)| id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use daedalus_proto::PortSource;
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
        assert_eq!(ws.slot_index, 0);

        let listed = registry.list_workspaces().await;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, ws.id);

        registry.close_workspace(ws.id, false).await.unwrap();
        assert!(registry.list_workspaces().await.is_empty());

        let err = registry.close_workspace(ws.id, false).await.unwrap_err();
        assert!(matches!(err, ProtoError::WorkspaceNotFound(_)));
    }

    #[tokio::test]
    async fn slot_allocation_reuses_lowest_freed_slot() {
        let dir = TempDir::new().unwrap();
        let registry = test_registry(dir.path());

        let ws0 = registry
            .create_workspace("zero".to_string(), dir.path().to_path_buf(), HashMap::new())
            .await
            .unwrap();
        let ws1 = registry
            .create_workspace("one".to_string(), dir.path().to_path_buf(), HashMap::new())
            .await
            .unwrap();
        let ws2 = registry
            .create_workspace("two".to_string(), dir.path().to_path_buf(), HashMap::new())
            .await
            .unwrap();

        assert_eq!(ws0.slot_index, 0);
        assert_eq!(ws1.slot_index, 1);
        assert_eq!(ws2.slot_index, 2);

        registry.close_workspace(ws1.id, false).await.unwrap();
        let ws_reused = registry
            .create_workspace(
                "reused".to_string(),
                dir.path().to_path_buf(),
                HashMap::new(),
            )
            .await
            .unwrap();
        assert_eq!(ws_reused.slot_index, 1);
    }

    #[tokio::test]
    async fn concurrent_workspace_creation_avoids_unbound_reserved_ports() {
        let dir = TempDir::new().unwrap();
        let ws_dir = dir.path().join("app");
        std::fs::create_dir_all(&ws_dir).unwrap();
        std::fs::write(ws_dir.join(".env"), "PORT=18000\n").unwrap();

        let registry = test_registry(dir.path());
        let _primary = registry
            .create_workspace("primary".to_string(), ws_dir.clone(), HashMap::new())
            .await
            .unwrap();

        let blocker = std::net::TcpListener::bind(("127.0.0.1", 18100))
            .expect("port 18100 must be free before blocking");

        // Slot 1 bumps past 18100 to 18200 (without binding 18200 yet).
        let ws1 = registry
            .create_workspace("wt1".to_string(), ws_dir.clone(), HashMap::new())
            .await
            .unwrap();
        assert_eq!(ws1.slot_index, 1);
        assert_eq!(ws1.port_offset, 200);
        assert_eq!(
            ws1.injected_env.get("PORT").map(String::as_str),
            Some("18200")
        );

        // Slot 2 must skip 18200 (reserved by ws1) and allocate 18300.
        let ws2 = registry
            .create_workspace("wt2".to_string(), ws_dir, HashMap::new())
            .await
            .unwrap();
        drop(blocker);

        assert_eq!(ws2.slot_index, 2);
        assert_eq!(ws2.port_offset, 300);
        assert_eq!(
            ws2.injected_env.get("PORT").map(String::as_str),
            Some("18300")
        );
    }

    #[tokio::test]
    async fn publish_external_status_matches_longest_prefix_and_caps_at_16_sources() {
        let dir = TempDir::new().unwrap();
        let parent_dir = dir.path().join("repo");
        let child_dir = parent_dir.join("packages").join("web");
        std::fs::create_dir_all(&child_dir).unwrap();

        let registry = test_registry(dir.path());
        let parent_ws = registry
            .create_workspace("parent".to_string(), parent_dir.clone(), HashMap::new())
            .await
            .unwrap();
        let child_ws = registry
            .create_workspace("child".to_string(), child_dir.clone(), HashMap::new())
            .await
            .unwrap();

        let mut events = registry.subscribe_events();
        let nested_file_dir = child_dir.join("src");
        std::fs::create_dir_all(&nested_file_dir).unwrap();

        // Longest prefix should match child_ws rather than parent_ws
        registry
            .publish_external_status(
                None,
                Some(nested_file_dir),
                "phrourion".to_string(),
                StatusLevel::Warn,
                "2 dirty".to_string(),
                None,
            )
            .await
            .unwrap();

        let evt = events.recv().await.unwrap();
        match evt {
            ServerEvent::ExternalStatusUpdated {
                workspace_id,
                status,
            } => {
                assert_eq!(workspace_id, child_ws.id);
                assert_eq!(status.source, "phrourion");
                assert_eq!(status.level, StatusLevel::Warn);
            }
            other => panic!("unexpected event: {other:?}"),
        }

        // Publish 17 distinct sources to parent_ws -> oldest (s0) should be evicted, capped at 16
        for i in 0..17 {
            registry
                .publish_external_status(
                    Some(parent_ws.id),
                    None,
                    format!("s{i}"),
                    StatusLevel::Info,
                    format!("label-{i}"),
                    None,
                )
                .await
                .unwrap();
        }
        // Upserting existing source s1 should not evict others or grow past 16
        registry
            .publish_external_status(
                Some(parent_ws.id),
                None,
                "s1".to_string(),
                StatusLevel::Ok,
                "updated".to_string(),
                None,
            )
            .await
            .unwrap();

        let listed = registry.list_workspaces().await;
        let parent_info = listed.iter().find(|w| w.id == parent_ws.id).unwrap();
        assert_eq!(parent_info.external_statuses.len(), 16);
        assert!(
            !parent_info
                .external_statuses
                .iter()
                .any(|s| s.source == "s0")
        );
        let s1 = parent_info
            .external_statuses
            .iter()
            .find(|s| s.source == "s1")
            .unwrap();
        assert_eq!(s1.level, StatusLevel::Ok);
        assert_eq!(s1.label, "updated");
    }

    #[tokio::test]
    async fn update_workspace_runtime_broadcasts_only_on_changes() {
        let dir = TempDir::new().unwrap();
        let registry = test_registry(dir.path());
        let ws = registry
            .create_workspace("rt".to_string(), dir.path().to_path_buf(), HashMap::new())
            .await
            .unwrap();
        let mut events = registry.subscribe_events();

        let ports = vec![PortMapping {
            host_port: 3000,
            container_port: None,
            service: Some("node".to_string()),
            source: PortSource::LocalProcess,
        }];
        let containers = vec![];

        assert!(
            registry
                .update_workspace_runtime(ws.id, ports.clone(), containers.clone())
                .await
        );
        assert!(
            !registry
                .update_workspace_runtime(ws.id, ports.clone(), containers.clone())
                .await
        );

        let evt = events.recv().await.unwrap();
        assert!(matches!(
            evt,
            ServerEvent::WorkspaceRuntimeUpdated { workspace_id, .. } if workspace_id == ws.id
        ));
        assert!(events.try_recv().is_err());
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

        let targets = registry.monitor_targets().await;
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].workspace_id, ws.id);
        assert!(!targets[0].root_pids.is_empty());

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

        registry.close_workspace(ws.id, false).await.unwrap();
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
            let close_fut = reg_close.close_workspace(ws.id, false);

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
