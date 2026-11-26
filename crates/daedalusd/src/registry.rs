mod panes;
mod persistence;
mod projects;
mod status;
mod widget_runtime;
mod workbench_config;
mod workbench_import;

use self::panes::DefaultPanes;
use self::persistence::WorkspaceChange;
use crate::isolation;
use crate::journal::Journal;
use crate::manifest;
use crate::pane_actor::PaneHandle;
use crate::persistence::{PersistedRecentProject, PersistedWorkspace, StateStore};
use crate::runtime_monitor::{ActiveMonitorTarget, WorkspaceMonitorTarget};
use crate::shell_integration::ShellIntegration;
use crate::supervisor::{IsolationMode, Supervisor};
use crate::workbench::scheduler::WidgetCommands;
use crate::workbench::store::WorkbenchStore;
use crate::worktree::{self, WorktreeError};
use daedalus_proto::{
    AgentState, ContainerHealth, ExternalStatus, FailureTrace, GitWorkspaceStatus, GitWorktreeInfo,
    PaneId, PaneRole, PaneStatus, PaneSummary, PortMapping, ProtoError, ServerEvent, WorkspaceId,
    WorkspaceInfo, WorkspaceManifest,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock, broadcast};

struct WorkspaceRecord {
    id: WorkspaceId,
    name: String,
    root_dir: PathBuf,
    canonical_root_dir: PathBuf,
    slot_index: u16,
    port_offset: u16,
    compose_project_name: Option<String>,
    pending_compose_override: Option<isolation::PreparedComposeOverride>,
    git_branch: Option<String>,
    git_status: Option<GitWorkspaceStatus>,
    injected_env: HashMap<String, String>,
    reserved_ports: HashSet<u16>,
    pane_ids: Vec<PaneId>,
    default_panes: DefaultPanes,
    compose_removed: bool,
    worktree_removed: bool,
    lifecycle: std::sync::Arc<Mutex<()>>,
    widget_commands: Arc<WidgetCommands>,
    journal: Journal,
    bound_ports: Vec<PortMapping>,
    containers: Vec<ContainerHealth>,
    external_statuses: Vec<ExternalStatus>,
    manifest: Option<WorkspaceManifest>,
    project: daedalus_proto::ProjectInfo,
}

impl WorkspaceRecord {
    fn install_compose_override(&mut self) -> std::io::Result<()> {
        if let Some(compose_override) = self.pending_compose_override.take() {
            compose_override.install()?;
        }
        Ok(())
    }

    fn to_info(&self, panes: Vec<PaneSummary>) -> WorkspaceInfo {
        WorkspaceInfo {
            id: self.id,
            name: self.name.clone(),
            root_dir: self.root_dir.clone(),
            slot_index: self.slot_index,
            injected_env: self.injected_env.clone(),
            panes,
            default_panes_pending: Some(
                !self.worktree_removed && matches!(self.default_panes, DefaultPanes::Pending(_)),
            ),
            port_offset: self.port_offset,
            compose_project_name: self.compose_project_name.clone(),
            git_branch: self.git_branch.clone(),
            git_status: self.git_status,
            bound_ports: self.bound_ports.clone(),
            containers: self.containers.clone(),
            external_statuses: self.external_statuses.clone(),
            manifest: self.manifest.clone(),
            project: self.project.clone(),
            setup: None,
        }
    }

    fn snapshot_handles(
        &self,
        panes: &HashMap<PaneId, PaneHandle>,
    ) -> (WorkspaceInfo, Vec<PaneHandle>) {
        let handles = self
            .pane_ids
            .iter()
            .filter_map(|id| panes.get(id).cloned())
            .collect();
        (self.to_info(Vec::new()), handles)
    }

    fn to_persisted(&self) -> PersistedWorkspace {
        PersistedWorkspace {
            id: self.id,
            name: self.name.clone(),
            root_dir: self.root_dir.clone(),
            slot_index: self.slot_index,
            manual_launch: matches!(self.default_panes, DefaultPanes::Manual),
            agent_preset: self.manifest.as_ref().and_then(|m| m.agent_preset.clone()),
        }
    }
}

struct PreparedWorkspaceComponents {
    plan: isolation::IsolationPlan,
    compose_override: Option<isolation::PreparedComposeOverride>,
    git_branch: Option<String>,
    git_status: Option<GitWorkspaceStatus>,
    canonical_root_dir: PathBuf,
    manifest: Option<WorkspaceManifest>,
    project: daedalus_proto::ProjectInfo,
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

    let (plan, compose_override) = isolation::plan_workspace_isolation(
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
    let project = daedalus_proto::ProjectInfo {
        repository: worktree::primary_checkout(root_dir),
        tools: manifest::detect_tools(root_dir, plan.compose_project_name.is_some()),
    };
    Ok(PreparedWorkspaceComponents {
        project,
        plan,
        compose_override,
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
    manual_launch: bool,
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
        pending_compose_override: prepared.compose_override,
        git_branch: prepared.git_branch,
        git_status: prepared.git_status,
        injected_env: prepared.plan.injected_env,
        reserved_ports,
        pane_ids: Vec::new(),
        default_panes: if manual_launch {
            DefaultPanes::Manual
        } else {
            DefaultPanes::Pending(HashSet::new())
        },
        compose_removed: false,
        worktree_removed: false,
        lifecycle: Default::default(),
        widget_commands: Default::default(),
        journal: Journal::default(),
        bound_ports: Vec::new(),
        containers: Vec::new(),
        external_statuses: Vec::new(),
        manifest: prepared.manifest,
        project: prepared.project,
    }
}

pub struct Registry {
    journal_epoch: WorkspaceId,
    supervisor: Supervisor,
    shell_integration: ShellIntegration,
    event_tx: broadcast::Sender<ServerEvent>,
    state_store: Option<StateStore>,
    pub(crate) workbench_store: WorkbenchStore,
    persist_lock: Arc<Mutex<persistence::PersistenceWriteState>>,
    deferred_workspaces: Arc<RwLock<Vec<PersistedWorkspace>>>,
    create_lock: Mutex<()>,
    recent_projects: Arc<RwLock<Vec<PersistedRecentProject>>>,
    project_setups: Arc<RwLock<projects::ProjectSetupState>>,
    worktrees_dir: std::sync::RwLock<Option<PathBuf>>,
    workspaces: Arc<RwLock<HashMap<WorkspaceId, WorkspaceRecord>>>,
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
        let workbench_directory = state_store
            .as_ref()
            .and_then(|store| store.path().parent())
            .map(|parent| parent.join("workbench"));
        Self {
            journal_epoch: WorkspaceId::new(),
            supervisor,
            shell_integration,
            event_tx,
            state_store,
            workbench_store: WorkbenchStore::new(workbench_directory),
            persist_lock: Arc::new(Mutex::new(persistence::PersistenceWriteState::Unchecked)),
            deferred_workspaces: Default::default(),
            create_lock: Mutex::new(()),
            recent_projects: Default::default(),
            project_setups: Default::default(),
            worktrees_dir: std::sync::RwLock::new(None),
            workspaces: Default::default(),
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

    pub async fn read_activity(
        &self,
        id: WorkspaceId,
        cursor: Option<daedalus_proto::ActivityCursor>,
    ) -> Result<daedalus_proto::ActivityPage, ProtoError> {
        let workspaces = self.workspaces.read().await;
        let ws = workspaces
            .get(&id)
            .ok_or(ProtoError::WorkspaceNotFound(id))?;
        Ok(ws.journal.read(id, self.journal_epoch, cursor))
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
        self.create_workspace_with_mode(name, root_dir, env, false)
            .await
    }

    pub(crate) async fn create_workspace_with_mode(
        &self,
        name: String,
        root_dir: PathBuf,
        env: HashMap<String, String>,
        manual_launch: bool,
    ) -> Result<WorkspaceInfo, ProtoError> {
        let create_guard = self.create_lock.lock().await;
        let mut info = self
            .register_workspace(name, root_dir, env, manual_launch)
            .await?;
        drop(create_guard);
        self.attach_project_setup(&mut info).await;
        Ok(info)
    }

    /// Registers the workspace while the caller holds `create_lock`.
    async fn register_workspace(
        &self,
        name: String,
        root_dir: PathBuf,
        env: HashMap<String, String>,
        manual_launch: bool,
    ) -> Result<WorkspaceInfo, ProtoError> {
        let root_dir =
            std::path::absolute(root_dir).map_err(|err| ProtoError::IoError(err.to_string()))?;
        let deferred = self.find_deferred_workspace(&root_dir).await;
        let record = self
            .prepare_workspace_record(name, root_dir, env, manual_launch, deferred)
            .await?;
        self.publish_workspace(record).await
    }

    fn available_slot(used_slots: &HashSet<u16>, preferred: Option<u16>) -> Option<u16> {
        preferred
            .filter(|slot| !used_slots.contains(slot))
            .or_else(|| (0..=u16::MAX).find(|slot| !used_slots.contains(slot)))
    }

    /// Allocates a slot and prepares runtime settings without exposing the workspace.
    /// The caller holds `create_lock` until it publishes or drops the record.
    async fn prepare_workspace_record(
        &self,
        name: String,
        root_dir: PathBuf,
        env: HashMap<String, String>,
        manual_launch: bool,
        deferred: Option<PersistedWorkspace>,
    ) -> Result<WorkspaceRecord, ProtoError> {
        let root_dir =
            std::path::absolute(root_dir).map_err(|err| ProtoError::IoError(err.to_string()))?;
        if !root_dir.is_dir() {
            return Err(ProtoError::InvalidRequest(format!(
                "root_dir does not exist or is not a directory: {}",
                root_dir.display()
            )));
        }

        let (slot_index, reserved_ports) = {
            let ws_map = self.workspaces.read().await;
            let pending = self.deferred_workspaces.read().await;
            let used_slots: HashSet<u16> = ws_map
                .values()
                .map(|ws| ws.slot_index)
                .chain(
                    pending
                        .iter()
                        .filter(|entry| {
                            entry.root_dir.is_absolute()
                                && deferred.as_ref().is_none_or(|retry| retry.id != entry.id)
                        })
                        .map(|entry| entry.slot_index),
                )
                .collect();
            let slot_index =
                Self::available_slot(&used_slots, deferred.as_ref().map(|entry| entry.slot_index))
                    .ok_or_else(|| {
                        ProtoError::Internal("no workspace slots available".to_string())
                    })?;
            let reserved_ports: HashSet<u16> = ws_map
                .values()
                .flat_map(|ws| ws.reserved_ports.iter().copied())
                .collect();
            (slot_index, reserved_ports)
        };

        let mut entry = deferred.unwrap_or_else(|| PersistedWorkspace {
            id: WorkspaceId::new(),
            name,
            root_dir,
            slot_index,
            manual_launch,
            agent_preset: None,
        });
        entry.slot_index = slot_index;
        Self::prepare_registered_workspace(&entry, env, reserved_ports).await
    }

    async fn prepare_registered_workspace(
        entry: &PersistedWorkspace,
        env: HashMap<String, String>,
        reserved_ports: HashSet<u16>,
    ) -> Result<WorkspaceRecord, ProtoError> {
        let name = entry.name.clone();
        let root_dir = entry.root_dir.clone();
        let slot_index = entry.slot_index;
        let prepared = tokio::task::spawn_blocking(move || {
            prepare_workspace_components(&name, &root_dir, slot_index, env, &reserved_ports)
        })
        .await
        .map_err(|e| ProtoError::Internal(format!("workspace preparation task failed: {e}")))?
        .map_err(|e| ProtoError::IoError(e.to_string()))?;

        Ok(build_workspace_record(
            entry.id,
            entry.name.clone(),
            entry.root_dir.clone(),
            prepared,
            entry.manual_launch,
        ))
    }

    async fn publish_workspace(
        &self,
        record: WorkspaceRecord,
    ) -> Result<WorkspaceInfo, ProtoError> {
        let info = record.to_info(Vec::new());
        self.commit_workspace_change(WorkspaceChange::Register(Box::new(record)))
            .await?;
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
            worktree::create_worktree_for_registration(
                &repo_dir,
                &branch_clone,
                create_branch,
                &base_dir,
            )
        })
        .await
        .map_err(|e| ProtoError::Internal(format!("worktree creation task failed: {e}")))?
        .map_err(ProtoError::InvalidRequest)?;

        let path = wt.info.path.clone();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| wt.info.branch.clone())
            .unwrap_or(branch);

        // Git hooks can run while other workspace requests proceed. Serialize registration
        // and rollback from here, and check whether a request already claimed this checkout.
        let create_guard = self.create_lock.lock().await;
        let mut info = match self
            .register_workspace(name, path.clone(), env, false)
            .await
        {
            Ok(info) => info,
            Err(error) => {
                let already_registered = self
                    .workspaces
                    .read()
                    .await
                    .values()
                    .any(|record| record.canonical_root_dir == wt.canonical_target);
                let cleanup_error = if already_registered {
                    "another workspace is using the checkout".into()
                } else {
                    match tokio::task::spawn_blocking(move || wt.rollback()).await {
                        Ok(Ok(())) => return Err(error),
                        Ok(Err(reason)) => reason,
                        Err(task_error) => task_error.to_string(),
                    }
                };
                return Err(ProtoError::IoError(format!(
                    "{error}. Worktree cleanup at {} failed: {cleanup_error}. Inspect the checkout and branch with git worktree list before retrying.",
                    path.display()
                )));
            }
        };
        drop(create_guard);
        self.attach_project_setup(&mut info).await;
        Ok(info)
    }

    pub async fn list_workspaces(&self) -> Vec<WorkspaceInfo> {
        let mut snapshot: Vec<(WorkspaceInfo, Vec<PaneHandle>)> = {
            let ws_map = self.workspaces.read().await;
            let pane_map = self.panes.read().await;
            ws_map
                .values()
                .map(|ws| ws.snapshot_handles(&pane_map))
                .collect()
        };

        snapshot.sort_by_key(|(info, _)| info.slot_index);

        let mut out = Vec::with_capacity(snapshot.len());
        for (info, handles) in snapshot {
            let mut info = Self::with_pane_summaries(info, handles).await;
            self.attach_project_setup(&mut info).await;
            out.push(info);
        }
        out
    }

    pub async fn workspace_for_directory(
        &self,
        root_dir: PathBuf,
    ) -> Result<WorkspaceInfo, ProtoError> {
        let canonical = Self::canonicalize_target(None, Some(&root_dir)).await;
        let (info, handles) = {
            let workspaces = self.workspaces.read().await;
            let id = Self::resolve_workspace_in_map(
                &workspaces,
                None,
                Some(&root_dir),
                canonical.as_deref(),
            )?;
            let ws = &workspaces[&id];
            let panes = self.panes.read().await;
            ws.snapshot_handles(&panes)
        };
        let mut info = Self::with_pane_summaries(info, handles).await;
        self.attach_project_setup(&mut info).await;
        Ok(info)
    }

    async fn with_pane_summaries(
        mut info: WorkspaceInfo,
        handles: Vec<PaneHandle>,
    ) -> WorkspaceInfo {
        for handle in handles {
            if let Ok(summary) = handle.summary().await {
                info.panes.push(summary);
            }
        }
        info
    }

    async fn lock_workspace_lifecycle(
        &self,
        id: WorkspaceId,
    ) -> Result<tokio::sync::OwnedMutexGuard<()>, ProtoError> {
        let lifecycle = self
            .workspaces
            .read()
            .await
            .get(&id)
            .ok_or(ProtoError::WorkspaceNotFound(id))?
            .lifecycle
            .clone();
        Ok(lifecycle.lock_owned().await)
    }

    fn notify_membership(&self) {
        let _ = self.event_tx.send(ServerEvent::WorkspaceMembershipChanged);
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
        let _lifecycle = self.lock_workspace_lifecycle(workspace_id).await?;
        let (root_dir, project, remove_worktree) = {
            let workspaces = self.workspaces.read().await;
            let ws = workspaces
                .get(&workspace_id)
                .ok_or(ProtoError::WorkspaceNotFound(workspace_id))?;
            (
                ws.root_dir.clone(),
                ws.compose_project_name
                    .clone()
                    .filter(|_| compose_down && !ws.compose_removed),
                remove_worktree && !ws.worktree_removed,
            )
        };
        if remove_worktree {
            let path = root_dir.clone();
            tokio::task::spawn_blocking(move || {
                worktree::validate_worktree_removal(&path, force_remove_worktree)
            })
            .await
            .map_err(|e| ProtoError::Internal(format!("worktree validation task failed: {e}")))?
            .map_err(map_worktree_error)?;
        }

        // Keep the workspace and its slot reserved until cleanup succeeds. On failure,
        // clients can inspect the remaining record and retry with the same workspace ID.
        let handles: Vec<_> = {
            let mut workspaces = self.workspaces.write().await;
            let ws = workspaces
                .get_mut(&workspace_id)
                .ok_or(ProtoError::WorkspaceNotFound(workspace_id))?;
            let mut panes = self.panes.write().await;
            let handles: Vec<_> = ws
                .pane_ids
                .drain(..)
                .filter_map(|id| panes.remove(&id))
                .collect();
            if persist {
                ws.default_panes.record_drained(&handles);
            }
            handles
        };
        let had_panes = !handles.is_empty();
        for handle in handles {
            handle.close().await;
        }

        let compose_removed = project.is_some();
        if let Err(error) = Self::remove_workspace_resources(
            root_dir,
            project,
            remove_worktree,
            force_remove_worktree,
        )
        .await
        {
            if had_panes {
                self.notify_membership();
            }
            return Err(error);
        }

        if persist {
            if (compose_removed || remove_worktree)
                && let Some(workspace) = self.workspaces.write().await.get_mut(&workspace_id)
            {
                workspace.compose_removed |= compose_removed;
                workspace.worktree_removed |= remove_worktree;
            }
            if let Err(error) = self
                .commit_workspace_change(WorkspaceChange::Close(workspace_id))
                .await
            {
                if had_panes || compose_removed || remove_worktree {
                    self.notify_membership();
                    return Err(ProtoError::IoError(format!(
                        "Workspace cleanup completed and the registration was retained for retry. {error}"
                    )));
                }
                return Err(error);
            }
        } else {
            if let Some(workspace) = self.workspaces.write().await.remove(&workspace_id) {
                workspace.widget_commands.close();
            }
            self.project_setups
                .write()
                .await
                .forget_workspace(workspace_id);
            self.notify_membership();
        }
        Ok(())
    }

    async fn remove_workspace_resources(
        root_dir: PathBuf,
        compose_project: Option<String>,
        remove_worktree: bool,
        force_remove_worktree: bool,
    ) -> Result<(), ProtoError> {
        if let Some(project) = compose_project {
            let root_dir = root_dir.clone();
            let cleanup =
                tokio::task::spawn_blocking(move || isolation::run_compose_down(&root_dir, &project))
                    .await
                    .map_err(|error| {
                        ProtoError::Internal(format!(
                            "panes stopped; workspace retained for retry after Compose teardown task failed: {error}"
                        ))
                    })?;
            cleanup.map_err(|error| {
                ProtoError::IoError(format!(
                    "panes stopped; workspace retained for retry after Compose teardown failed: {error}"
                ))
            })?;
        }

        if remove_worktree {
            tokio::task::spawn_blocking(move || {
                worktree::remove_worktree(&root_dir, force_remove_worktree)
            })
            .await
            .map_err(|error| {
                ProtoError::Internal(format!("worktree removal task failed: {error}"))
            })?
            .map_err(map_worktree_error)?;
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

    pub async fn preview_image(
        &self,
        id: WorkspaceId,
        path: String,
    ) -> Result<daedalus_proto::rpc::ImagePreview, ProtoError> {
        let root = self
            .workspaces
            .read()
            .await
            .get(&id)
            .ok_or(ProtoError::WorkspaceNotFound(id))?
            .canonical_root_dir
            .clone();
        tokio::task::spawn_blocking(move || crate::preview::load(&root, &path))
            .await
            .map_err(|e| ProtoError::Internal(e.to_string()))?
    }

    /// Sends a manifest action to the first live pane with its target role.
    pub async fn run_workspace_action(
        &self,
        workspace_id: WorkspaceId,
        action_id: &str,
    ) -> Result<(), ProtoError> {
        let (action, candidates) = {
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
            let candidates = ws
                .pane_ids
                .iter()
                .filter_map(|pid| pane_map.get(pid))
                .filter(|handle| handle.role() == action.target)
                .cloned()
                .collect::<Vec<_>>();
            (action, candidates)
        };
        if candidates.is_empty() {
            return Err(ProtoError::InvalidRequest(format!(
                "no {} pane in workspace",
                action.target
            )));
        }
        let pane_handle = Self::first_live_action_pane(candidates)
            .await
            .ok_or_else(|| {
                ProtoError::InvalidRequest(format!("no live {} pane in workspace", action.target))
            })?;

        let mut data = action.command.into_bytes();
        if action.target == PaneRole::Agent {
            return pane_handle.paste_and_submit(data).await;
        }
        #[cfg(windows)]
        data.extend_from_slice(b"\r\n");
        #[cfg(not(windows))]
        data.push(b'\n');
        pane_handle.send_input(data).await
    }

    async fn first_live_action_pane(candidates: Vec<PaneHandle>) -> Option<PaneHandle> {
        for handle in candidates {
            if let Ok(summary) = handle.summary().await
                && !matches!(summary.status, PaneStatus::Exited { .. })
            {
                return Some(handle);
            }
        }
        None
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

    pub async fn monitor_targets(&self) -> Vec<WorkspaceMonitorTarget> {
        self.monitor_targets_with_roots()
            .await
            .into_iter()
            .map(|target| target.snapshot)
            .collect()
    }

    pub(crate) async fn monitor_targets_with_roots(&self) -> Vec<ActiveMonitorTarget> {
        let ws_map = self.workspaces.read().await;
        let pane_map = self.panes.read().await;
        ws_map
            .values()
            .map(|ws| {
                let monitor_roots: Vec<_> = ws
                    .pane_ids
                    .iter()
                    .filter_map(|pid| pane_map.get(pid))
                    .filter_map(PaneHandle::monitor_root)
                    .collect();
                let root_pids = monitor_roots
                    .iter()
                    .filter_map(|root| root.live_pid())
                    .collect();
                ActiveMonitorTarget {
                    snapshot: WorkspaceMonitorTarget {
                        workspace_id: ws.id,
                        root_dir: ws.root_dir.clone(),
                        compose_project_name: ws.compose_project_name.clone(),
                        root_pids,
                    },
                    monitor_roots,
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
            use daedalus_proto::ActivityKind;
            if ws.bound_ports != bound_ports {
                ws.journal.record(ActivityKind::Ports {
                    bound: bound_ports.len(),
                });
            }
            if ws.containers != containers {
                ws.journal.record(ActivityKind::Containers {
                    total: containers.len(),
                    unhealthy: containers
                        .iter()
                        .filter(|c| c.health.as_deref() == Some("unhealthy"))
                        .count(),
                });
            }
            if ws.git_status != git_status || ws.git_branch != git_branch {
                ws.journal.record(ActivityKind::Git);
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
        | WorktreeError::LockedWorktree(_)
        | WorktreeError::DirtyWorktree(_) => ProtoError::InvalidRequest(err.to_string()),
        WorktreeError::GitFailed(_) => ProtoError::Internal(err.to_string()),
    }
}

fn find_longest_prefix_workspace(
    ws_map: &HashMap<WorkspaceId, WorkspaceRecord>,
    target_dir: &Path,
    canonical_target: Option<&Path>,
) -> Option<WorkspaceId> {
    ws_map
        .values()
        .filter_map(|ws| {
            // Compare both paths in the same namespace. A raw prefix can point to a
            // different workspace when the target contains a symlink or `..`.
            let (target, root) = match canonical_target {
                Some(target) => (target, &ws.canonical_root_dir),
                None => (target_dir, &ws.root_dir),
            };
            target.starts_with(root).then(|| {
                (
                    root.components().count(),
                    std::cmp::Reverse(ws.slot_index),
                    ws.id,
                )
            })
        })
        .max_by_key(|(depth, slot, _)| (*depth, *slot))
        .map(|(_, _, id)| id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use daedalus_proto::PortSource;
    use daedalus_proto::StatusLevel;
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
    async fn failed_worktree_registration_preserves_existing_branch_and_allows_retry() {
        let dir = TempDir::new().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let worktrees = dir.path().join("worktrees");
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        let commit = |message| {
            git(&["add", ".env"]);
            git(&[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.com",
                "commit",
                "-m",
                message,
            ]);
        };
        git(&["init", "-b", "main"]);
        std::fs::write(repo.join(".env"), "PORT=65500\n").unwrap();
        commit("overflowing port");
        git(&["branch", "existing"]);

        let registry = test_registry(dir.path());
        registry.set_worktrees_dir(worktrees.clone());
        registry
            .create_workspace("slot zero".into(), dir.path().to_path_buf(), HashMap::new())
            .await
            .unwrap();

        for (branch, create_branch) in [("new", true), ("existing", false)] {
            let error = registry
                .create_worktree_workspace(
                    repo.clone(),
                    branch.into(),
                    create_branch,
                    HashMap::new(),
                )
                .await
                .unwrap_err();
            assert!(
                matches!(error, ProtoError::IoError(message) if message.contains("port block"))
            );
            assert!(!worktrees.join("repo").join(branch).exists());
        }
        git(&["show-ref", "--verify", "refs/heads/existing"]);
        assert_eq!(worktree::discover_worktrees(&repo).unwrap().len(), 1);
        assert_eq!(registry.list_workspaces().await.len(), 1);

        std::fs::write(repo.join(".env"), "# no port reservation\n").unwrap();
        commit("remove invalid port");
        let workspace = registry
            .create_worktree_workspace(repo, "new".into(), true, HashMap::new())
            .await
            .unwrap();
        assert_eq!(workspace.git_branch.as_deref(), Some("new"));
        assert!(workspace.root_dir.is_dir());
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
        let start = registry.read_activity(ws.id, None).await.unwrap().cursor;
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
        let history = registry.read_activity(ws.id, Some(start)).await.unwrap();
        assert!(!history.gap);
        assert_eq!(
            history
                .entries
                .iter()
                .map(|e| e.kind.clone())
                .collect::<Vec<_>>(),
            [
                daedalus_proto::ActivityKind::Ports { bound: 1 },
                daedalus_proto::ActivityKind::Git
            ]
        );
    }

    #[tokio::test]
    async fn agent_replacement_retains_previous_on_failure_and_rejects_stale_clients() {
        let dir = TempDir::new().unwrap();
        let reg = test_registry(dir.path());
        let ws = reg
            .create_workspace("main".into(), dir.path().to_owned(), HashMap::new())
            .await
            .unwrap();
        let original = reg
            .replace_agent_pane(ws.id, None, Some(interactive_shell_command()), 80, 24)
            .await
            .unwrap();
        let failed = reg
            .replace_agent_pane(
                ws.id,
                Some(original.id),
                Some(vec!["absent-agent-8bd055".into()]),
                80,
                24,
            )
            .await;
        assert!(matches!(failed, Err(ProtoError::SpawnFailed(_))));
        assert!(reg.get_pane(original.id).await.is_ok());
        let (first, second) = tokio::join!(
            reg.replace_agent_pane(
                ws.id,
                Some(original.id),
                Some(interactive_shell_command()),
                80,
                24
            ),
            reg.replace_agent_pane(
                ws.id,
                Some(original.id),
                Some(interactive_shell_command()),
                80,
                24
            ),
        );
        let winner = match (first, second) {
            (Ok(pane), Err(ProtoError::PaneNotFound(id)))
            | (Err(ProtoError::PaneNotFound(id)), Ok(pane))
                if id == original.id =>
            {
                pane
            }
            outcomes => panic!("unexpected replacements: {outcomes:?}"),
        };
        assert_eq!(reg.list_workspaces().await[0].panes[0].id, winner.id);
        assert_eq!(reg.list_workspaces().await[0].panes.len(), 1);
        reg.close_all().await;
    }

    #[tokio::test]
    async fn concurrent_default_initializers_share_panes_and_respect_closed_roles() {
        use daedalus_proto::ResponsePayload::{DefaultPaneExisting, PaneSpawned};
        let dir = TempDir::new().unwrap();
        let reg = test_registry(dir.path());
        let ws = reg
            .create_workspace("default".into(), dir.path().into(), HashMap::new())
            .await
            .unwrap();
        let initialize = || async {
            let mut results = Vec::new();
            for role in [PaneRole::Agent, PaneRole::Service, PaneRole::Runner] {
                results.push(
                    reg.ensure_default_pane(ws.id, role, Some(interactive_shell_command()), 80, 24)
                        .await
                        .unwrap(),
                );
            }
            results
        };
        let (first, second) = tokio::join!(initialize(), initialize());
        for (a, b) in first.into_iter().zip(second) {
            match (a, b) {
                (PaneSpawned(new), DefaultPaneExisting(Some(existing)))
                | (DefaultPaneExisting(Some(existing)), PaneSpawned(new)) => {
                    assert_eq!(new.id, existing.id)
                }
                results => panic!("both initializers must share each pane: {results:?}"),
            }
        }
        let current = reg.list_workspaces().await.pop().unwrap();
        assert_eq!(current.panes.len(), 3);
        assert_eq!(reg.panes.read().await.len(), 3);
        assert_eq!(
            current
                .panes
                .iter()
                .map(|p| p.role)
                .collect::<HashSet<_>>()
                .len(),
            3
        );
        assert!(!current.needs_default_panes());
        for pane in current.panes {
            reg.close_pane(pane.id).await.unwrap();
        }
        assert!(
            initialize()
                .await
                .iter()
                .all(|result| matches!(result, DefaultPaneExisting(None)))
        );
        assert!(reg.panes.read().await.is_empty());
        assert!(!reg.list_workspaces().await[0].needs_default_panes());
        reg.close_all().await;
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
        // A manual workspace with only a Runner remains exactly as its caller configured it.
        assert!(!registry.list_workspaces().await[0].needs_default_panes());
        assert!(matches!(
            registry
                .ensure_default_pane(ws.id, PaneRole::Service, None, 80, 24)
                .await
                .unwrap(),
            daedalus_proto::ResponsePayload::DefaultPaneExisting(None)
        ));
        assert_eq!(registry.panes.read().await.len(), 1);

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
        assert!(registry.get_pane(summary.id).await.is_ok());
        assert!(registry.monitor_targets().await[0].root_pids.is_empty());
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
    async fn failed_lifecycle_snapshot_keeps_registration_retriable_and_consistent() {
        let dir = TempDir::new().unwrap();
        let state_path = dir.path().join("state.json");
        let backup = dir.path().join("state.saved");
        let open_registry = || {
            Registry::new_with_state_store(
                test_supervisor(),
                ShellIntegration::prepare(dir.path()).unwrap(),
                broadcast::channel(8).0,
                Some(StateStore::new(state_path.clone())),
            )
        };
        let registry = open_registry();
        let project = dir.path().join("project");
        let other = dir.path().join("other");
        std::fs::create_dir(&project).unwrap();
        std::fs::create_dir(&other).unwrap();
        let saved = registry
            .create_workspace("existing".into(), project, HashMap::new())
            .await
            .unwrap();

        std::fs::rename(&state_path, &backup).unwrap();
        std::fs::create_dir(&state_path).unwrap();
        let mut events = registry.subscribe_events();
        let failed_create = registry
            .create_workspace("unpublished".into(), other.clone(), HashMap::new())
            .await;
        let failed_close = registry
            .close_workspace(saved.id, false, false, false)
            .await;
        assert!(
            matches!(failed_create, Err(ProtoError::IoError(_))),
            "{failed_create:?}"
        );
        assert!(
            matches!(failed_close, Err(ProtoError::IoError(_))),
            "{failed_close:?}"
        );
        assert_eq!(registry.list_workspaces().await[0].id, saved.id);
        assert_eq!(registry.list_workspaces().await.len(), 1);
        assert!(registry.recent_projects().await.is_empty());
        assert!(matches!(
            events.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));

        std::fs::remove_dir(&state_path).unwrap();
        std::fs::rename(&backup, &state_path).unwrap();
        assert_eq!(
            StateStore::new(state_path.clone())
                .load()
                .unwrap()
                .workspaces[0]
                .id,
            saved.id
        );
        registry
            .close_workspace(saved.id, false, false, false)
            .await
            .unwrap();
        let created = registry
            .create_workspace("persisted".into(), other, HashMap::new())
            .await
            .unwrap();
        assert_eq!(created.slot_index, saved.slot_index);
        drop(registry);

        let registry = open_registry();
        assert_eq!(registry.restore_persisted_workspaces().await.unwrap(), 1);
        assert_eq!(registry.list_workspaces().await[0].id, created.id);
    }

    #[tokio::test]
    async fn failed_registration_preserves_the_existing_compose_override() {
        let dir = TempDir::new().unwrap();
        let state_path = dir.path().join("state.json");
        let backup = dir.path().join("state.saved");
        let registry = Registry::new_with_state_store(
            test_supervisor(),
            ShellIntegration::prepare(dir.path()).unwrap(),
            broadcast::channel(8).0,
            Some(StateStore::new(state_path.clone())),
        );
        std::fs::write(
            dir.path().join("compose.yml"),
            "services:\n  app:\n    ports:\n      - '18640:80'\n",
        )
        .unwrap();
        let first = registry
            .create_workspace("existing".into(), dir.path().into(), HashMap::new())
            .await
            .unwrap();
        let override_path = dir.path().join(".daedalus/compose.override.yml");
        let original = std::fs::read(&override_path).unwrap();

        std::fs::rename(&state_path, &backup).unwrap();
        std::fs::create_dir(&state_path).unwrap();
        let failed = registry
            .create_workspace("rejected".into(), dir.path().into(), HashMap::new())
            .await;
        assert!(matches!(failed, Err(ProtoError::IoError(_))), "{failed:?}");
        assert_eq!(std::fs::read(&override_path).unwrap(), original);
        assert_eq!(registry.list_workspaces().await[0].id, first.id);
    }

    #[tokio::test]
    async fn failed_compose_install_restores_the_previous_workspace_snapshot() {
        let dir = TempDir::new().unwrap();
        let state_path = dir.path().join("state.json");
        let store = StateStore::new(state_path);
        let registry = Registry::new_with_state_store(
            test_supervisor(),
            ShellIntegration::prepare(dir.path()).unwrap(),
            broadcast::channel(8).0,
            Some(store.clone()),
        );
        registry
            .create_workspace("existing".into(), dir.path().into(), HashMap::new())
            .await
            .unwrap();
        let previous = store.load().unwrap();
        let project = dir.path().join("project");
        let override_path = project.join(".daedalus/compose.override.yml");
        std::fs::create_dir_all(&override_path).unwrap();
        std::fs::write(project.join("compose.yml"), "services: {}\n").unwrap();
        let mut events = registry.subscribe_events();

        let result = registry
            .create_workspace("blocked".into(), project.clone(), HashMap::new())
            .await;
        let error = result.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Failed to write the workspace Compose override")
        );
        assert_eq!(store.load().unwrap(), previous);
        assert_eq!(registry.list_workspaces().await.len(), 1);
        assert!(matches!(
            events.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));

        std::fs::remove_dir(&override_path).unwrap();
        registry
            .create_workspace("retry".into(), project, HashMap::new())
            .await
            .unwrap();
        assert!(override_path.is_file());
    }

    #[tokio::test]
    async fn failed_worktree_snapshot_rolls_back_creation_and_allows_close_retry() {
        let dir = TempDir::new().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-b", "main"]);
        git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.com",
            "commit",
            "--allow-empty",
            "-m",
            "initial",
        ]);
        let state_path = dir.path().join("state.json");
        let backup = dir.path().join("state.saved");
        let registry = Registry::new_with_state_store(
            test_supervisor(),
            ShellIntegration::prepare(dir.path()).unwrap(),
            broadcast::channel(16).0,
            Some(StateStore::new(state_path.clone())),
        );
        registry.set_worktrees_dir(dir.path().join("worktrees"));
        let original = registry
            .create_workspace("original".into(), repo.clone(), HashMap::new())
            .await
            .unwrap();
        let block_save = || {
            std::fs::rename(&state_path, &backup).unwrap();
            std::fs::create_dir(&state_path).unwrap();
        };
        let recover = || {
            std::fs::remove_dir(&state_path).unwrap();
            std::fs::rename(&backup, &state_path).unwrap();
        };
        let create = || {
            registry.create_worktree_workspace(repo.clone(), "topic".into(), true, HashMap::new())
        };
        block_save();
        assert!(matches!(create().await, Err(ProtoError::IoError(_))));
        assert_eq!(registry.list_workspaces().await.len(), 1);
        recover();
        let worktree = create().await.unwrap();

        block_save();
        let error = registry
            .close_workspace(worktree.id, false, true, false)
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("registration was retained for retry")
        );
        assert!(!worktree.root_dir.exists());
        assert_eq!(registry.list_workspaces().await.len(), 2);
        recover();
        registry
            .close_workspace(worktree.id, false, true, false)
            .await
            .unwrap();
        let loaded = StateStore::new(state_path).load().unwrap();
        assert_eq!(loaded.workspaces.len(), 1);
        assert_eq!(loaded.workspaces[0].id, original.id);
    }

    #[tokio::test]
    async fn rejected_snapshots_survive_workspace_mutations() {
        for original in [br#"{ "version": 99 }"#.as_slice(), b"{invalid json"] {
            let dir = TempDir::new().unwrap();
            let state_file = dir.path().join("state.json");
            std::fs::write(&state_file, original).unwrap();
            let registry = Registry::new_with_state_store(
                test_supervisor(),
                ShellIntegration::prepare(dir.path()).unwrap(),
                broadcast::channel(8).0,
                Some(StateStore::new(state_file.clone())),
            );
            assert!(registry.restore_persisted_workspaces().await.is_err());
            let ws = registry
                .create_workspace("temporary".into(), dir.path().into(), HashMap::new())
                .await
                .unwrap();
            assert!(matches!(
                registry.set_project_setup(ws.id, Vec::new()).await,
                Err(ProtoError::IoError(_))
            ));
            registry
                .close_workspace(ws.id, false, false, false)
                .await
                .unwrap();
            assert_eq!(std::fs::read(state_file).unwrap(), original);
        }
    }

    #[tokio::test]
    async fn ambiguous_legacy_roots_survive_new_state_writes_without_becoming_active() {
        let dir = TempDir::new().unwrap();
        let store = StateStore::new(dir.path().join("state.json"));
        let unresolved = PersistedWorkspace {
            id: WorkspaceId::new(),
            name: "legacy".into(),
            manual_launch: false,
            root_dir: ".".into(),
            slot_index: 0,
            agent_preset: None,
        };
        let mut state = crate::persistence::PersistedState::default();
        state.workspaces.push(unresolved.clone());
        store.save(&state).unwrap();
        let registry = Registry::new_with_state_store(
            test_supervisor(),
            ShellIntegration::prepare(dir.path()).unwrap(),
            broadcast::channel(8).0,
            Some(store.clone()),
        );
        assert_eq!(registry.restore_persisted_workspaces().await.unwrap(), 0);
        registry
            .create_workspace("new".into(), dir.path().into(), HashMap::new())
            .await
            .unwrap();
        assert!(store.load().unwrap().workspaces.contains(&unresolved));
        assert_eq!(registry.list_workspaces().await.len(), 1);
    }

    #[tokio::test]
    async fn restore_failure_preserves_registration_for_restart_and_explicit_retry() {
        for restart in [true, false] {
            let dir = TempDir::new().unwrap();
            let root = dir.path().join("project");
            let other = dir.path().join("other");
            std::fs::create_dir(&root).unwrap();
            std::fs::create_dir(&other).unwrap();
            std::fs::write(
                root.join(".daedalus.toml"),
                "[agent]\npreset = \"claude\"\n[env]\nSAVED_SETUP = \"kept\"\n",
            )
            .unwrap();
            let store = StateStore::new(dir.path().join("state.json"));
            let open_registry = || {
                Registry::new_with_state_store(
                    test_supervisor(),
                    ShellIntegration::prepare(dir.path()).unwrap(),
                    broadcast::channel(8).0,
                    Some(store.clone()),
                )
            };
            let original = open_registry()
                .create_workspace_with_mode("original".into(), root.clone(), HashMap::new(), true)
                .await
                .unwrap();
            let persisted = store.load().unwrap().workspaces.remove(0);
            std::fs::write(root.join(".env"), [0xff]).unwrap();

            let unavailable = open_registry();
            assert_eq!(unavailable.restore_persisted_workspaces().await.unwrap(), 0);
            assert_eq!(
                store.load().unwrap().workspaces,
                std::slice::from_ref(&persisted)
            );
            assert!(unavailable.list_workspaces().await.is_empty());
            let other_workspace = unavailable
                .create_workspace("other".into(), other, HashMap::new())
                .await
                .unwrap();
            assert_ne!(other_workspace.slot_index, original.slot_index);
            assert!(store.load().unwrap().workspaces.contains(&persisted));

            let alias = root.join("..").join("project");
            assert!(
                unavailable
                    .create_workspace("retry".into(), alias.clone(), HashMap::new())
                    .await
                    .is_err()
            );
            assert!(store.load().unwrap().workspaces.contains(&persisted));
            std::fs::write(root.join(".env"), "PORT=21470\n").unwrap();

            let recovered = if restart {
                let registry = open_registry();
                assert_eq!(registry.restore_persisted_workspaces().await.unwrap(), 2);
                registry
                    .workspace_for_directory(root.clone())
                    .await
                    .unwrap()
            } else {
                unavailable
                    .create_workspace("retry".into(), alias, HashMap::new())
                    .await
                    .unwrap()
            };
            assert_eq!(recovered.id, original.id);
            assert_eq!(recovered.name, original.name);
            assert_eq!(recovered.root_dir, original.root_dir);
            assert_eq!(recovered.slot_index, original.slot_index);
            assert!(!recovered.needs_default_panes());
            assert_eq!(recovered.injected_env.get("SAVED_SETUP").unwrap(), "kept");
            let state = store.load().unwrap();
            assert_eq!(state.workspaces.len(), 2);
            assert!(state.workspaces.contains(&persisted));
        }
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

        // Discovery is refreshed on restore, including for entries written by older daemons.
        assert!(ws_keep.project.tools.is_empty());
        std::fs::write(keep_dir.join("pyproject.toml"), "").unwrap();

        // Delete drop_dir before restart.
        std::fs::remove_dir_all(&drop_dir).unwrap();

        let reg2 = Registry::new_with_state_store(
            test_supervisor(),
            ShellIntegration::prepare(dir.path()).unwrap(),
            broadcast::channel(64).0,
            Some(StateStore::new(state_path.clone())),
        );
        let restored = reg2.restore_persisted_workspaces().await.unwrap();
        assert_eq!(restored, 1);

        let listed = reg2.list_workspaces().await;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, ws_keep.id);
        assert_eq!(listed[0].project.tools, ["Python"]);
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
