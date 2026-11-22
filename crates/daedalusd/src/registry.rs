use crate::isolation;
use crate::manifest;
use crate::pane_actor::PaneHandle;
use crate::persistence::{PersistedState, PersistedWorkspace, STATE_VERSION, StateStore};
use crate::runtime_monitor::WorkspaceMonitorTarget;
use crate::shell_integration::ShellIntegration;
use crate::supervisor::{IsolationMode, Supervisor};
use crate::worktree::{self, WorktreeError};
use daedalus_proto::{
    AgentState, ContainerHealth, ExternalStatus, FailureTrace, GitWorkspaceStatus, GitWorktreeInfo,
    PaneId, PaneRole, PaneSummary, PortMapping, ProtoError, ServerEvent, StatusLevel, WorkspaceId,
    WorkspaceInfo, WorkspaceManifest,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, RwLock, broadcast};
use tracing::{info, warn};

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
    git_status: Option<GitWorkspaceStatus>,
    injected_env: HashMap<String, String>,
    reserved_ports: HashSet<u16>,
    pane_ids: Vec<PaneId>,
    bound_ports: Vec<PortMapping>,
    containers: Vec<ContainerHealth>,
    external_statuses: Vec<ExternalStatus>,
    manifest: Option<WorkspaceManifest>,
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
            git_status: self.git_status,
            bound_ports: self.bound_ports.clone(),
            containers: self.containers.clone(),
            external_statuses: self.external_statuses.clone(),
            manifest: self.manifest.clone(),
        }
    }

    fn to_persisted(&self) -> PersistedWorkspace {
        PersistedWorkspace {
            id: self.id,
            name: self.name.clone(),
            root_dir: self.root_dir.clone(),
            slot_index: self.slot_index,
            agent_preset: self.manifest.as_ref().and_then(|m| m.agent_preset.clone()),
        }
    }
}

struct PreparedWorkspaceComponents {
    plan: isolation::IsolationPlan,
    git_branch: Option<String>,
    git_status: Option<GitWorkspaceStatus>,
    canonical_root_dir: PathBuf,
    manifest: Option<WorkspaceManifest>,
}

fn prepare_workspace_components(
    name: &str,
    root_dir: &Path,
    slot_index: u16,
    caller_env: HashMap<String, String>,
    reserved_ports: &HashSet<u16>,
) -> std::io::Result<PreparedWorkspaceComponents> {
    let loaded_manifest = manifest::load_project_manifest(root_dir);
    let extra_ports = loaded_manifest
        .as_ref()
        .map(|m| m.extra_ports.as_slice())
        .unwrap_or(&[]);
    let mut merged_env = loaded_manifest
        .as_ref()
        .map(|m| m.env.clone())
        .unwrap_or_default();
    merged_env.extend(caller_env);

    let plan = isolation::prepare_workspace_isolation_with_extra_ports(
        name,
        root_dir,
        slot_index,
        &merged_env,
        reserved_ports,
        extra_ports,
    )?;
    let (git_status, git_branch) = match worktree::git_workspace_status_and_branch(root_dir) {
        Some((status, branch)) => (Some(status), branch),
        None => (None, worktree::current_branch(root_dir)),
    };
    let canonical_root_dir = daedalus_proto::strip_windows_verbatim_prefix(
        std::fs::canonicalize(root_dir).unwrap_or_else(|_| root_dir.to_path_buf()),
    );
    Ok(PreparedWorkspaceComponents {
        plan,
        git_branch,
        git_status,
        canonical_root_dir,
        manifest: loaded_manifest.map(|m| m.workspace_manifest),
    })
}

fn build_workspace_record(
    id: WorkspaceId,
    name: String,
    root_dir: PathBuf,
    prepared: PreparedWorkspaceComponents,
) -> WorkspaceRecord {
    let reserved_ports: HashSet<u16> = prepared.plan.remapped_ports.values().copied().collect();
    WorkspaceRecord {
        id,
        name,
        root_dir,
        canonical_root_dir: prepared.canonical_root_dir,
        slot_index: prepared.plan.slot_index,
        port_offset: prepared.plan.port_offset,
        compose_project_name: prepared.plan.compose_project_name,
        git_branch: prepared.git_branch,
        git_status: prepared.git_status,
        injected_env: prepared.plan.injected_env,
        reserved_ports,
        pane_ids: Vec::new(),
        bound_ports: Vec::new(),
        containers: Vec::new(),
        external_statuses: Vec::new(),
        manifest: prepared.manifest,
    }
}

pub struct Registry {
    supervisor: Supervisor,
    shell_integration: ShellIntegration,
    event_tx: broadcast::Sender<ServerEvent>,
    state_store: Option<StateStore>,
    persist_lock: Mutex<()>,
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
        Self::new_with_state_store(supervisor, shell_integration, event_tx, None)
    }

    pub fn new_with_state_store(
        supervisor: Supervisor,
        shell_integration: ShellIntegration,
        event_tx: broadcast::Sender<ServerEvent>,
        state_store: Option<StateStore>,
    ) -> Self {
        Self {
            supervisor,
            shell_integration,
            event_tx,
            state_store,
            persist_lock: Mutex::new(()),
            create_lock: Mutex::new(()),
            worktrees_dir: std::sync::RwLock::new(None),
            workspaces: RwLock::new(HashMap::new()),
            panes: RwLock::new(HashMap::new()),
        }
    }

    async fn persist_state(&self) {
        let Some(store) = self.state_store.clone() else {
            return;
        };
        let _persist_guard = self.persist_lock.lock().await;
        let mut workspaces: Vec<PersistedWorkspace> = {
            let ws_map = self.workspaces.read().await;
            ws_map.values().map(WorkspaceRecord::to_persisted).collect()
        };
        workspaces.sort_by_key(|ws| ws.slot_index);
        let state = PersistedState {
            version: STATE_VERSION,
            workspaces,
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

        let mut restored_records = Vec::new();
        let mut needs_resave = false;

        for entry in persisted_state.workspaces {
            if existing_ids.contains(&entry.id) {
                needs_resave = true;
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

            let record = build_workspace_record(entry.id, entry.name, entry.root_dir, prepared);
            used_slots.insert(record.slot_index);
            reserved_ports.extend(record.reserved_ports.iter().copied());
            existing_ids.insert(record.id);
            restored_records.push(record);
        }

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
        let prepared = tokio::task::spawn_blocking(move || {
            prepare_workspace_components(&name_clone, &root_clone, slot_index, env, &reserved_ports)
        })
        .await
        .map_err(|e| ProtoError::Internal(format!("workspace preparation task failed: {e}")))?
        .map_err(|e| ProtoError::IoError(e.to_string()))?;

        let id = WorkspaceId::new();
        let record = build_workspace_record(id, name, root_dir, prepared);
        let info = record.to_info(Vec::new());
        self.workspaces.write().await.insert(id, record);
        drop(_create_guard);
        self.persist_state().await;
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
        remove_worktree: bool,
        force_remove_worktree: bool,
    ) -> Result<(), ProtoError> {
        self.close_workspace_internal(
            workspace_id,
            compose_down,
            remove_worktree,
            force_remove_worktree,
            true,
        )
        .await
    }

    async fn close_workspace_internal(
        &self,
        workspace_id: WorkspaceId,
        compose_down: bool,
        remove_worktree: bool,
        force_remove_worktree: bool,
        persist: bool,
    ) -> Result<(), ProtoError> {
        if remove_worktree {
            let root_dir = self
                .workspaces
                .read()
                .await
                .get(&workspace_id)
                .map(|ws| ws.root_dir.clone())
                .ok_or(ProtoError::WorkspaceNotFound(workspace_id))?;

            let validation = tokio::task::spawn_blocking(move || {
                worktree::validate_worktree_removal(&root_dir, force_remove_worktree)
            })
            .await
            .map_err(|e| ProtoError::Internal(format!("worktree validation task failed: {e}")))?;

            if let Err(err) = validation {
                return Err(map_worktree_error(err));
            }
        }

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

        if persist {
            self.persist_state().await;
        }

        if compose_down && let Some(project) = ws.compose_project_name {
            let root_dir = ws.root_dir.clone();
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

        if remove_worktree {
            let root_dir = ws.root_dir;
            let removal = tokio::task::spawn_blocking(move || {
                worktree::remove_worktree(&root_dir, force_remove_worktree)
            })
            .await
            .map_err(|e| ProtoError::Internal(format!("worktree removal task failed: {e}")))?;

            if let Err(err) = removal {
                return Err(map_worktree_error(err));
            }
        }

        Ok(())
    }

    /// Closes all active workspaces and their PTY panes during daemon shutdown
    /// without clearing persisted `state.json`.
    pub async fn close_all(&self) {
        let ws_ids: Vec<WorkspaceId> = self.workspaces.read().await.keys().copied().collect();
        for id in ws_ids {
            let _ = self
                .close_workspace_internal(id, false, false, false, false)
                .await;
        }
    }

    async fn canonicalize_target(
        workspace_id: Option<WorkspaceId>,
        root_dir: Option<&Path>,
    ) -> Option<PathBuf> {
        match (workspace_id, root_dir) {
            (None, Some(dir)) => {
                let dir_clone = dir.to_path_buf();
                tokio::task::spawn_blocking(move || {
                    std::fs::canonicalize(&dir_clone)
                        .ok()
                        .map(daedalus_proto::strip_windows_verbatim_prefix)
                })
                .await
                .ok()
                .flatten()
            }
            _ => None,
        }
    }

    fn resolve_workspace_in_map(
        ws_map: &HashMap<WorkspaceId, WorkspaceRecord>,
        workspace_id: Option<WorkspaceId>,
        root_dir: Option<&Path>,
        canonical_target: Option<&Path>,
    ) -> Result<WorkspaceId, ProtoError> {
        if let Some(id) = workspace_id {
            if !ws_map.contains_key(&id) {
                return Err(ProtoError::WorkspaceNotFound(id));
            }
            Ok(id)
        } else if let Some(target_dir) = root_dir {
            find_longest_prefix_workspace(ws_map, target_dir, canonical_target).ok_or_else(|| {
                ProtoError::InvalidRequest(format!(
                    "no workspace matches root_dir: {}",
                    target_dir.display()
                ))
            })
        } else {
            Err(ProtoError::InvalidRequest(
                "either workspace_id or root_dir must be provided".to_string(),
            ))
        }
    }

    pub async fn set_agent_state(
        &self,
        pane_id: Option<PaneId>,
        workspace_id: Option<WorkspaceId>,
        root_dir: Option<PathBuf>,
        state: AgentState,
    ) -> Result<(), ProtoError> {
        let handle = if let Some(pid) = pane_id {
            self.get_pane(pid).await?
        } else {
            if workspace_id.is_none() && root_dir.is_none() {
                return Err(ProtoError::InvalidRequest(
                    "one of pane_id, workspace_id, or root_dir must be provided".to_string(),
                ));
            }
            let canonical_target =
                Self::canonicalize_target(workspace_id, root_dir.as_deref()).await;

            let ws_id;
            let agent_handle = {
                let ws_map = self.workspaces.read().await;
                let pane_map = self.panes.read().await;
                ws_id = Self::resolve_workspace_in_map(
                    &ws_map,
                    workspace_id,
                    root_dir.as_deref(),
                    canonical_target.as_deref(),
                )?;
                let ws = ws_map
                    .get(&ws_id)
                    .ok_or(ProtoError::WorkspaceNotFound(ws_id))?;
                ws.pane_ids
                    .iter()
                    .filter_map(|pid| pane_map.get(pid))
                    .find(|h| h.role() == PaneRole::Agent)
                    .cloned()
            };

            agent_handle.ok_or_else(|| {
                ProtoError::InvalidRequest(format!("workspace {ws_id} has no agent pane"))
            })?
        };

        handle.set_agent_state(state).await
    }

    /// Executes a custom action from the workspace's `.daedalus.toml` `[[actions]]` manifest
    /// by sending its command to the first pane matching `action.target`.
    pub async fn run_workspace_action(
        &self,
        workspace_id: WorkspaceId,
        action_id: &str,
    ) -> Result<(), ProtoError> {
        let (action, pane_handle) = {
            let ws_map = self.workspaces.read().await;
            let pane_map = self.panes.read().await;
            let ws = ws_map
                .get(&workspace_id)
                .ok_or(ProtoError::WorkspaceNotFound(workspace_id))?;
            let action = ws
                .manifest
                .as_ref()
                .and_then(|m| m.actions.iter().find(|a| a.id == action_id))
                .cloned()
                .ok_or_else(|| {
                    ProtoError::InvalidRequest(format!("unknown action '{action_id}'"))
                })?;
            let handle = ws
                .pane_ids
                .iter()
                .filter_map(|pid| pane_map.get(pid))
                .find(|h| h.role() == action.target)
                .cloned()
                .ok_or_else(|| {
                    ProtoError::InvalidRequest(format!("no {} pane in workspace", action.target))
                })?;
            (action, handle)
        };

        let data = match action.target {
            PaneRole::Agent => {
                let mut bytes = daedalus_proto::wrap_bracketed_paste(action.command.as_bytes());
                bytes.push(b'\r');
                bytes
            }
            PaneRole::Service | PaneRole::Runner => {
                let mut bytes = action.command.into_bytes();
                #[cfg(windows)]
                bytes.extend_from_slice(b"\r\n");
                #[cfg(not(windows))]
                bytes.push(b'\n');
                bytes
            }
        };

        pane_handle.send_input(data).await
    }

    pub async fn get_failure_trace(
        &self,
        pane_id: PaneId,
    ) -> Result<Option<FailureTrace>, ProtoError> {
        let handle = self.get_pane(pane_id).await?;
        let Some(mut trace) = handle.failure_trace().await? else {
            return Ok(None);
        };

        let ws_root = self
            .workspaces
            .read()
            .await
            .get(&handle.workspace_id())
            .map(|ws| ws.root_dir.clone());
        let context_dir = ws_root.or_else(|| trace.cwd.clone());

        if let Some(dir) = context_dir
            && let Ok((git_branch, git_diff_stat)) =
                tokio::task::spawn_blocking(move || worktree::git_failure_context(&dir)).await
        {
            trace.git_branch = git_branch;
            trace.git_diff_stat = git_diff_stat;
        }

        Ok(Some(trace))
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

        let canonical_target = Self::canonicalize_target(workspace_id, root_dir.as_deref()).await;

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
            let resolved_id = Self::resolve_workspace_in_map(
                &ws_map,
                workspace_id,
                root_dir.as_deref(),
                canonical_target.as_deref(),
            )?;

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

    pub async fn clear_external_status(
        &self,
        workspace_id: Option<WorkspaceId>,
        root_dir: Option<PathBuf>,
        source: String,
    ) -> Result<(), ProtoError> {
        if source.trim().is_empty() {
            return Err(ProtoError::InvalidRequest(
                "external status source cannot be empty".to_string(),
            ));
        }
        if workspace_id.is_none() && root_dir.is_none() {
            return Err(ProtoError::InvalidRequest(
                "either workspace_id or root_dir must be provided".to_string(),
            ));
        }

        let canonical_target = Self::canonicalize_target(workspace_id, root_dir.as_deref()).await;
        let updated_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0);

        let cleared_ws_id = {
            let mut ws_map = self.workspaces.write().await;
            let resolved_id = Self::resolve_workspace_in_map(
                &ws_map,
                workspace_id,
                root_dir.as_deref(),
                canonical_target.as_deref(),
            )?;
            let ws = ws_map
                .get_mut(&resolved_id)
                .ok_or(ProtoError::WorkspaceNotFound(resolved_id))?;

            let before = ws.external_statuses.len();
            ws.external_statuses
                .retain(|existing| existing.source != source);
            (ws.external_statuses.len() != before).then_some(resolved_id)
        };

        if let Some(target_id) = cleared_ws_id {
            let _ = self.event_tx.send(ServerEvent::ExternalStatusUpdated {
                workspace_id: target_id,
                status: ExternalStatus {
                    source,
                    level: StatusLevel::Info,
                    label: String::new(),
                    detail: None,
                    updated_at_ms,
                },
            });
        }

        Ok(())
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
                    root_dir: ws.root_dir.clone(),
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
        git_status: Option<GitWorkspaceStatus>,
        git_branch: Option<String>,
    ) -> bool {
        let changed = {
            let mut ws_map = self.workspaces.write().await;
            let Some(ws) = ws_map.get_mut(&workspace_id) else {
                return false;
            };
            if ws.bound_ports == bound_ports
                && ws.containers == containers
                && ws.git_status == git_status
                && ws.git_branch == git_branch
            {
                return false;
            }
            ws.bound_ports = bound_ports.clone();
            ws.containers = containers.clone();
            ws.git_status = git_status;
            ws.git_branch = git_branch.clone();
            true
        };

        if changed {
            let _ = self.event_tx.send(ServerEvent::WorkspaceRuntimeUpdated {
                workspace_id,
                bound_ports,
                containers,
                git_status,
                git_branch,
            });
        }
        changed
    }
}

fn map_worktree_error(err: WorktreeError) -> ProtoError {
    match err {
        WorktreeError::PrimaryCheckout(_)
        | WorktreeError::NotAWorktree(_)
        | WorktreeError::DirtyWorktree(_) => ProtoError::InvalidRequest(err.to_string()),
        WorktreeError::GitFailed(_) => ProtoError::Internal(err.to_string()),
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

    fn test_supervisor() -> Supervisor {
        #[cfg(windows)]
        {
            Supervisor::with_mode(IsolationMode::WindowsProcessTree)
        }
        #[cfg(not(windows))]
        {
            Supervisor::with_mode(IsolationMode::PgidFallback)
        }
    }

    fn interactive_shell_command() -> Vec<String> {
        #[cfg(unix)]
        {
            vec!["/bin/sh".to_string()]
        }
        #[cfg(windows)]
        {
            vec!["cmd.exe".to_string(), "/Q".to_string()]
        }
    }

    fn exit_code_command(code: i32) -> Vec<String> {
        #[cfg(unix)]
        {
            vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                format!("exit {code}"),
            ]
        }
        #[cfg(windows)]
        {
            vec![
                "cmd.exe".to_string(),
                "/C".to_string(),
                "exit".to_string(),
                code.to_string(),
            ]
        }
    }

    fn sleep_5s_command() -> Vec<String> {
        #[cfg(unix)]
        {
            vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "sleep 5".to_string(),
            ]
        }
        #[cfg(windows)]
        {
            vec![
                "ping.exe".to_string(),
                "-n".to_string(),
                "6".to_string(),
                "127.0.0.1".to_string(),
            ]
        }
    }

    fn test_registry(runtime_dir: &std::path::Path) -> Registry {
        let supervisor = test_supervisor();
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

        registry
            .close_workspace(ws.id, false, false, false)
            .await
            .unwrap();
        assert!(registry.list_workspaces().await.is_empty());

        let err = registry
            .close_workspace(ws.id, false, false, false)
            .await
            .unwrap_err();
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

        registry
            .close_workspace(ws1.id, false, false, false)
            .await
            .unwrap();
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

        let git_status = Some(GitWorkspaceStatus {
            is_worktree: false,
            dirty: true,
            ahead: 1,
            behind: 0,
        });
        let git_branch = Some("main".to_string());

        assert!(
            registry
                .update_workspace_runtime(
                    ws.id,
                    ports.clone(),
                    containers.clone(),
                    git_status,
                    git_branch.clone(),
                )
                .await
        );
        assert!(
            !registry
                .update_workspace_runtime(
                    ws.id,
                    ports.clone(),
                    containers.clone(),
                    git_status,
                    git_branch.clone(),
                )
                .await
        );

        let evt = events.recv().await.unwrap();
        assert!(matches!(
            evt,
            ServerEvent::WorkspaceRuntimeUpdated {
                workspace_id,
                git_status: gs,
                git_branch: gb,
                ..
            } if workspace_id == ws.id && gs == git_status && gb == git_branch
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
                Some(interactive_shell_command()),
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
        #[cfg(windows)]
        let input_bytes = b"echo DAEDALUS_PANE_OK\r\n".to_vec();
        #[cfg(not(windows))]
        let input_bytes = b"echo DAEDALUS_PANE_OK\n".to_vec();
        handle.send_input(input_bytes).await.unwrap();

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

        registry
            .close_workspace(ws.id, false, false, false)
            .await
            .unwrap();
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
                Some(exit_code_command(42)),
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
                Some(sleep_5s_command()),
                None,
                80,
                24,
                HashMap::new(),
            );
            let close_fut = reg_close.close_workspace(ws.id, false, false, false);

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

    #[tokio::test]
    async fn create_workspace_loads_manifest_merges_env_and_reserves_extra_ports() {
        let dir = TempDir::new().unwrap();
        let app_dir = dir.path().join("app");
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::write(app_dir.join(".env"), "PORT=17000\nFROM_DOTENV=dotenv_val\n").unwrap();
        std::fs::write(
            app_dir.join(".daedalus.toml"),
            r#"
[agent]
preset = "claude"
command = ["claude", "--model", "sonnet"]

[service]
command = ["npm", "run", "dev"]

[runner]
command = ["cargo", "test"]

[ports]
extra = [17100]

[env]
RUST_LOG = "info"
SHARED_KEY = "from_manifest"
"#,
        )
        .unwrap();

        let registry = test_registry(dir.path());
        let ws0 = registry
            .create_workspace(
                "primary".to_string(),
                app_dir.clone(),
                HashMap::from([("SHARED_KEY".to_string(), "from_caller".to_string())]),
            )
            .await
            .unwrap();

        let manifest = ws0.manifest.expect("manifest should be populated");
        assert_eq!(manifest.agent_preset.as_deref(), Some("claude"));
        assert_eq!(
            manifest.agent_command,
            Some(vec![
                "claude".to_string(),
                "--model".to_string(),
                "sonnet".to_string()
            ])
        );
        assert_eq!(
            manifest.service_command,
            Some(vec![
                "npm".to_string(),
                "run".to_string(),
                "dev".to_string()
            ])
        );
        assert_eq!(
            manifest.runner_command,
            Some(vec!["cargo".to_string(), "test".to_string()])
        );
        assert_eq!(
            ws0.injected_env.get("RUST_LOG").map(String::as_str),
            Some("info")
        );
        assert_eq!(
            ws0.injected_env.get("SHARED_KEY").map(String::as_str),
            Some("from_caller")
        );

        let ws1 = registry
            .create_workspace("wt1".to_string(), app_dir.clone(), HashMap::new())
            .await
            .unwrap();
        assert_eq!(ws1.slot_index, 1);
        assert_eq!(ws1.port_offset, 100);
        // ws1 reserved 17000+100=17100 and 17100+100=17200.
        // ws2 starts at slot 2 (offset 200 -> 17200 and 17300), must skip 17200 -> offset 300.
        let ws2 = registry
            .create_workspace("wt2".to_string(), app_dir, HashMap::new())
            .await
            .unwrap();
        assert_eq!(ws2.slot_index, 2);
        assert_eq!(ws2.port_offset, 300);
    }

    #[tokio::test]
    async fn persistence_saves_on_create_close_and_restores_dormant_skipping_deleted_dirs() {
        let dir = TempDir::new().unwrap();
        let state_path = dir.path().join("state").join("state.json");
        let keep_dir = dir.path().join("keep");
        let drop_dir = dir.path().join("drop");
        std::fs::create_dir_all(&keep_dir).unwrap();
        std::fs::create_dir_all(&drop_dir).unwrap();
        std::fs::write(
            keep_dir.join(".daedalus.toml"),
            "[agent]\npreset = \"agy\"\n[env]\nSECRET_ENV = \"should_not_be_in_state_json\"\n",
        )
        .unwrap();

        let reg1 = Registry::new_with_state_store(
            test_supervisor(),
            ShellIntegration::prepare(dir.path()).unwrap(),
            broadcast::channel(64).0,
            Some(StateStore::new(state_path.clone())),
        );
        let ws_keep = reg1
            .create_workspace("keep".to_string(), keep_dir.clone(), HashMap::new())
            .await
            .unwrap();
        let ws_drop = reg1
            .create_workspace("drop".to_string(), drop_dir.clone(), HashMap::new())
            .await
            .unwrap();

        let raw_json = std::fs::read_to_string(&state_path).unwrap();
        assert!(
            !raw_json.contains("should_not_be_in_state_json"),
            "state.json must not persist raw [env] values: {raw_json}"
        );

        // Clean shutdown must preserve state.json.
        reg1.close_all().await;
        assert!(state_path.is_file());

        // Delete drop_dir before restart.
        std::fs::remove_dir_all(&drop_dir).unwrap();

        let reg2 = Registry::new_with_state_store(
            test_supervisor(),
            ShellIntegration::prepare(dir.path()).unwrap(),
            broadcast::channel(64).0,
            Some(StateStore::new(state_path.clone())),
        );
        let restored = reg2.restore_persisted_workspaces().await;
        assert_eq!(restored, 1);

        let listed = reg2.list_workspaces().await;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, ws_keep.id);
        assert!(listed[0].panes.is_empty());
        assert_eq!(
            listed[0].injected_env.get("SECRET_ENV").map(String::as_str),
            Some("should_not_be_in_state_json")
        );
        assert_eq!(
            listed[0]
                .manifest
                .as_ref()
                .and_then(|m| m.agent_preset.as_deref()),
            Some("agy")
        );

        // Deleted entry was pruned from state.json.
        let reloaded = StateStore::new(state_path.clone()).load().unwrap();
        assert_eq!(reloaded.workspaces.len(), 1);
        assert_ne!(reloaded.workspaces[0].id, ws_drop.id);

        // Closing workspace removes it from state.json.
        reg2.close_workspace(ws_keep.id, false, false, false)
            .await
            .unwrap();
        let after_close = StateStore::new(state_path).load().unwrap();
        assert!(after_close.workspaces.is_empty());
    }
}
