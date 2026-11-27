mod background_rpc;
mod image_requests;
mod launch;
#[cfg(test)]
mod negotiation_tests;
mod recent_projects;
mod rpc;
mod session_init;
mod setup;
pub(crate) mod terminal;
mod widget_lane;
mod workspace;

use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use daedalus_proto::{
    AgentState, ClientMessage, ContainerHealth, GitWorkspaceStatus, GridRow, GridSnapshot, PaneId,
    PaneSummary, PortMapping, ResponsePayload, SearchMatch, ServerEvent, WorkspaceId,
    WorkspaceInfo, WorkspaceManifest,
};
use launch::{connect_endpoint, should_auto_spawn_daemon, spawn_daemon_detached};
use rpc::{RpcConnection, RpcError};
use session_init::initialize_session;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tracing::{debug, info, warn};
use workspace::{
    create_and_populate_workspace, create_worktree_and_populate_workspace, ensure_workspace_panes,
    replace_agent_preset,
};

use crate::state::AgentPreset;

const DEFAULT_PANE_COLS: u16 = 120;
const DEFAULT_PANE_ROWS: u16 = 36;
const INITIAL_BACKOFF: Duration = Duration::from_millis(50);
const MAX_BACKOFF: Duration = Duration::from_secs(2);
const RECONNECT_DELAY: Duration = Duration::from_millis(500);

/// Abstract event sink delivered to the UI thread (implemented by `EventLoopProxy<UiEvent>`
/// in the GUI binary and by an async channel in headless integration tests).
pub trait UiEventSink: Send + Sync + 'static {
    fn emit(&self, event: UiEvent);
}

/// Configuration for `DaemonBridge`.
#[derive(Clone)]
pub struct BridgeConfig {
    pub socket_path: PathBuf,
    pub auth_token: Option<String>,
    pub initial_dir: PathBuf,
    pub bootstrap_explicit_dir: bool,
    pub default_agent: AgentPreset,
    pub auto_spawn_daemon: bool,
}

impl std::fmt::Debug for BridgeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BridgeConfig")
            .field("socket_path", &self.socket_path)
            .field("initial_dir", &self.initial_dir)
            .field("bootstrap_explicit_dir", &self.bootstrap_explicit_dir)
            .field("default_agent", &self.default_agent)
            .field("auto_spawn_daemon", &self.auto_spawn_daemon)
            .field(
                "auth_token",
                &self.auth_token.as_ref().map(|_| "[redacted]"),
            )
            .finish()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum WorkbenchOperation {
    #[default]
    Read,
    Layout(uuid::Uuid),
    Migration,
    PaneWidgets(u64),
    WidgetEdit,
    WidgetPreview(uuid::Uuid),
    WidgetAction {
        widget: String,
        detail_session: uuid::Uuid,
    },
    ImportPreview(uuid::Uuid),
    Import,
    Export(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignmentLaunch {
    Direct,
    SavedRunner,
    ReviewedRunner,
}

/// Commands sent from the UI thread to `DaemonBridge`.
#[derive(Debug, Clone)]
pub enum UiCommand {
    Workbench {
        workspace_id: WorkspaceId,
        operation: WorkbenchOperation,
        request: Box<ClientMessage>,
    },
    ReconcileWorkspaceMembership,
    PreviewImage {
        workspace_id: WorkspaceId,
        pane_id: PaneId,
        token: uuid::Uuid,
        path: String,
    },
    InspectSetup {
        session: u64,
        root_dir: PathBuf,
        executables: Vec<String>,
    },
    ApplySetup {
        workspace_id: WorkspaceId,
        assignments: Vec<crate::assignments::Assignment>,
        launch: Vec<String>,
        expected: daedalus_proto::ProjectSetupBaseline,
    },
    LaunchStoredSetup(WorkspaceId),
    InstallSetupTool {
        workspace_id: WorkspaceId,
        argv: Vec<String>,
    },
    /// Reloads recent projects. Does nothing on a daemon that does not serve them.
    RefreshRecentProjects,
    SaveProjectSetup {
        workspace_id: WorkspaceId,
        assignments: Vec<crate::assignments::Assignment>,
        request_id: u64,
    },
    SaveWorkspaceSettings {
        workspace_id: WorkspaceId,
        assignments: Vec<crate::assignments::Assignment>,
        expected: daedalus_proto::ProjectSetupBaseline,
        request_id: u64,
    },
    ReadActivity {
        workspace_id: WorkspaceId,
        cursor: Option<daedalus_proto::ActivityCursor>,
        token: u64,
    },
    CreateManualWorkspace {
        session: u64,
        name: String,
        root_dir: PathBuf,
    },
    CreateWorkspace {
        name: String,
        root_dir: PathBuf,
        agent_preset: AgentPreset,
    },
    CreateWorktreeWorkspace {
        repo_dir: PathBuf,
        branch: String,
        default_agent: AgentPreset,
    },
    ClosePane(PaneId),
    CloseWorkspace {
        id: WorkspaceId,
        compose_down: bool,
        remove_worktree: bool,
        force_remove_worktree: bool,
    },
    LaunchAssignment {
        workspace_id: WorkspaceId,
        assignment: crate::assignments::Assignment,
        launch: AssignmentLaunch,
        cols: u16,
        rows: u16,
    },
    AddAgent {
        workspace_id: WorkspaceId,
        preset: AgentPreset,
        cols: u16,
        rows: u16,
    },
    ReplaceAgentPreset {
        workspace_id: WorkspaceId,
        old_pane_id: Option<PaneId>,
        preset: AgentPreset,
        cols: u16,
        rows: u16,
    },
    RunWorkspaceAction {
        workspace_id: WorkspaceId,
        action_id: String,
    },
    SendInput {
        pane_id: PaneId,
        data: Vec<u8>,
    },
    Paste {
        pane_id: PaneId,
        data: Vec<u8>,
    },
    ResizePane {
        pane_id: PaneId,
        cols: u16,
        rows: u16,
    },
    RequestSnapshot {
        pane_id: PaneId,
    },
    RequestScrollback {
        pane_id: PaneId,
        generation: u64,
        offset_from_bottom: usize,
        count: usize,
    },
    SearchPane {
        pane_id: PaneId,
        query: String,
        case_sensitive: bool,
        generation: u64,
    },
    EnsureWorkspacePanes {
        workspace_id: WorkspaceId,
        default_agent: AgentPreset,
        manifest: Option<WorkspaceManifest>,
    },
    ForwardRunnerFailureToAgent {
        runner_pane_id: PaneId,
        agent_pane_id: PaneId,
    },
    Shutdown,
}

/// Events emitted by `DaemonBridge` back to the UI thread.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum UiEvent {
    WorkbenchCapability(bool),
    Workbench {
        workspace_id: WorkspaceId,
        operation: WorkbenchOperation,
        response: Box<Result<ResponsePayload, String>>,
    },
    ImagePreview {
        pane_id: PaneId,
        token: uuid::Uuid,
        result: Result<daedalus_proto::rpc::ImagePreview, String>,
    },
    SetupInspected {
        session: u64,
        result: Result<daedalus_proto::SetupInspection, String>,
    },
    SetupApplied {
        workspace_id: WorkspaceId,
        error: Option<String>,
    },
    SetupInstallerStarted {
        workspace_id: WorkspaceId,
        pane_id: PaneId,
    },
    SetupInstallerFinished(PaneId, Option<i32>),
    SetupInstallerFailed {
        workspace_id: WorkspaceId,
        message: String,
    },
    JournalCapability(bool),
    DaemonCapabilities {
        persistence_health: Option<daedalus_proto::PersistenceHealth>,
        home_dir: Option<PathBuf>,
        project_setups: bool,
        recent_projects: bool,
    },
    RecentProjectsLoaded(Vec<daedalus_proto::RecentProject>),
    ProjectSetupChanged {
        root_dir: PathBuf,
        assignments: Vec<crate::assignments::Assignment>,
    },
    ProjectSetupMigrationFinished {
        workspace_id: WorkspaceId,
        request_id: u64,
        result: Result<(), String>,
    },
    WorkspaceSettingsSaved {
        workspace_id: WorkspaceId,
        request_id: u64,
        result: Result<(), String>,
    },
    ActivityRead {
        token: u64,
        page: daedalus_proto::ActivityPage,
    },
    ActivityReadFailed(u64),
    /// The session has loaded initial terminal snapshots and can dispatch commands.
    Connected,
    Disconnected,
    WorkspacesLoaded(Vec<WorkspaceInfo>),
    WorkspaceMembershipLoaded(Vec<WorkspaceInfo>),
    WorkspaceCloseFailed(Vec<WorkspaceInfo>),
    WorkspaceCloseFinished(WorkspaceId),
    WorkspaceCreated(WorkspaceInfo),
    ManualWorkspaceCreated {
        session: u64,
        workspace: WorkspaceInfo,
    },
    ManualWorkspaceFailed {
        session: u64,
        message: String,
    },
    WorkspacePanesEnsured {
        workspace_id: WorkspaceId,
        complete: bool,
    },
    PaneSpawned {
        summary: PaneSummary,
        select_tab: bool,
    },
    AssignmentStarted {
        workspace_id: WorkspaceId,
        pane_id: PaneId,
        assignment: crate::assignments::Assignment,
    },
    PaneClosed {
        pane_id: PaneId,
    },
    Snapshot(GridSnapshot),
    ScrollbackLoaded {
        pane_id: PaneId,
        generation: u64,
        total_scrollback: usize,
        rows: Vec<GridRow>,
    },
    SearchResults {
        pane_id: PaneId,
        generation: u64,
        total_scrollback: usize,
        matches: Vec<SearchMatch>,
    },
    AgentStateChanged {
        workspace_id: WorkspaceId,
        pane_id: PaneId,
        state: AgentState,
    },
    WorkspaceRuntimeUpdated {
        workspace_id: WorkspaceId,
        bound_ports: Vec<PortMapping>,
        containers: Vec<ContainerHealth>,
        git_status: Option<GitWorkspaceStatus>,
        git_branch: Option<String>,
    },
    ServerEvent(ServerEvent),
    Toast {
        message: String,
        is_error: bool,
    },
}

/// Background async bridge between the UI thread and `daedalusd`.
pub struct DaemonBridge {
    session: watch::Receiver<Option<mpsc::UnboundedSender<UiCommand>>>,
    sink: Arc<dyn UiEventSink>,
    shutdown_tx: watch::Sender<()>,
    thread_handle: Option<JoinHandle<()>>,
}

impl DaemonBridge {
    /// Spawns a dedicated 2-worker Tokio runtime thread and begins connecting to `daedalusd`.
    pub fn start(config: BridgeConfig, sink: Arc<dyn UiEventSink>) -> Self {
        let (session_tx, session) = watch::channel(None);
        let thread_sink = Arc::clone(&sink);
        let (shutdown_tx, mut shutdown_rx) = watch::channel(());
        let thread_handle = std::thread::Builder::new()
            .name("daedalus-bridge".to_string())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .expect("failed to build Tokio runtime for DaemonBridge");
                runtime.block_on(async {
                    tokio::select! {
                        biased;
                        _ = shutdown_rx.changed() => {}
                        _ = run_bridge(config, thread_sink, session_tx) => {}
                    }
                });
            })
            .expect("failed to spawn DaemonBridge thread");

        Self {
            session,
            sink,
            shutdown_tx,
            thread_handle: Some(thread_handle),
        }
    }

    /// Submits a command only to the current session. Returns whether it was accepted.
    pub fn send(&self, cmd: UiCommand) -> bool {
        if matches!(cmd, UiCommand::Shutdown) {
            return self.shutdown_tx.send(()).is_ok();
        }
        let accepted = self
            .session
            .borrow()
            .as_ref()
            .is_some_and(|tx| tx.send(cmd).is_ok());
        if accepted {
            return true;
        }
        self.sink.emit(UiEvent::Toast {
            message: "Daemon unavailable. Try again after reconnecting.".into(),
            is_error: true,
        });
        false
    }

    /// Requests a clean shutdown and waits for the background runtime thread to exit.
    pub fn shutdown(self) {
        drop(self);
    }
}

impl Drop for DaemonBridge {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(());
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }
}

async fn run_bridge(
    config: BridgeConfig,
    sink: Arc<dyn UiEventSink>,
    session: watch::Sender<Option<mpsc::UnboundedSender<UiCommand>>>,
) {
    let mut bootstrapped = false;
    let mut backoff = INITIAL_BACKOFF;
    let mut spawn_attempted = false;

    loop {
        match connect_endpoint(&config.socket_path).await {
            Ok((stream, tunnel)) => {
                backoff = INITIAL_BACKOFF;
                spawn_attempted = false;
                let mut conn = RpcConnection::new(stream, Arc::clone(&sink));

                // A queue belongs to exactly one connection. Dropping the connection
                // also invalidates commands accepted just before its loss.
                let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
                let initialized = initialize_session(
                    &mut conn,
                    &config,
                    &sink,
                    &mut bootstrapped,
                    &session,
                    cmd_tx,
                )
                .await;
                match initialized {
                    Ok(()) => {
                        let endpoint = tunnel
                            .as_ref()
                            .map_or(config.socket_path.as_path(), |tunnel| {
                                tunnel.local_endpoint()
                            });
                        session_loop(
                            &mut conn,
                            &sink,
                            &mut cmd_rx,
                            endpoint,
                            config.auth_token.as_deref(),
                        )
                        .await;
                    }
                    Err(RpcError::Disconnected) => {}
                    Err(RpcError::TimedOut) => {
                        sink.emit(UiEvent::Toast {
                            message: "Daemon setup timed out. Reconnecting.".into(),
                            is_error: true,
                        });
                    }
                    Err(RpcError::Proto(err)) => {
                        warn!(error = %err, "session initialization RPC failed");
                        sink.emit(UiEvent::Toast {
                            message: format!("Daemon error: {err}"),
                            is_error: true,
                        });
                    }
                }
                session.send_replace(None);
                if cmd_rx.try_recv().is_ok() {
                    sink.emit(UiEvent::Toast {
                        message: "Connection lost. Pending commands were not sent.".into(),
                        is_error: true,
                    });
                }
                sink.emit(UiEvent::Disconnected);
                conn.close().await;
                tokio::time::sleep(RECONNECT_DELAY).await;
            }
            Err(err) => {
                debug!(
                    socket = %config.socket_path.display(),
                    error = %err,
                    "unable to connect to daedalusd"
                );
                if should_auto_spawn_daemon(
                    config.auto_spawn_daemon,
                    spawn_attempted,
                    &config.socket_path,
                ) {
                    spawn_attempted = true;
                    match spawn_daemon_detached(&config.socket_path, config.auth_token.as_deref()) {
                        Ok(bin) => {
                            info!(
                                binary = %bin.display(),
                                socket = %config.socket_path.display(),
                                "auto-spawned daedalusd"
                            );
                        }
                        Err(spawn_err) => {
                            warn!(error = %spawn_err, "failed to auto-spawn daedalusd");
                            sink.emit(UiEvent::Toast {
                                message: format!("Failed to launch daedalusd: {spawn_err}"),
                                is_error: true,
                            });
                        }
                    }
                }

                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }
}

async fn reconcile_membership(
    conn: &RpcConnection,
    sink: &Arc<dyn UiEventSink>,
) -> Result<(), RpcError> {
    if let ResponsePayload::Workspaces(workspaces) =
        conn.call(ClientMessage::ListWorkspaces).await?
    {
        sink.emit(UiEvent::WorkspaceMembershipLoaded(workspaces));
    }
    Ok(())
}

/// Processes commands for a single connection until it closes.
async fn session_loop(
    conn: &mut RpcConnection,
    sink: &Arc<dyn UiEventSink>,
    cmd_rx: &mut mpsc::UnboundedReceiver<UiCommand>,
    endpoint: &std::path::Path,
    auth_token: Option<&str>,
) {
    let membership = conn.membership_notifications();
    let mut widgets = widget_lane::WidgetLane::new(endpoint, auth_token, sink);
    let mut recents = JoinSet::new();
    let mut images = JoinSet::new();
    let mut next_image: Option<image_requests::ImageRequest> = None;
    let mut refresh_recents = conn.recent_projects_supported;
    loop {
        if refresh_recents && recents.is_empty() {
            recent_projects::request(&mut recents, endpoint, auth_token);
            refresh_recents = false;
        }
        if images.is_empty()
            && let Some(request) = next_image.take()
        {
            request.start(&mut images, endpoint, auth_token);
        }
        tokio::select! {
            biased;
            _ = conn.disconnected() => {
                return;
            }
            _ = widgets.join_next(), if widgets.has_jobs() => {}
            Some(result) = images.join_next(), if !images.is_empty() => {
                match result {
                    Ok(event) => sink.emit(event),
                    Err(error) => warn!(%error, "image preview task failed"),
                }
            }
            Some(result) = recents.join_next(), if !recents.is_empty() => {
                match result {
                    Ok(Ok(projects)) => sink.emit(UiEvent::RecentProjectsLoaded(projects)),
                    Ok(Err(error)) => warn!(%error, "listing recent projects failed"),
                    Err(error) => warn!(%error, "recent projects task failed"),
                }
            }
            _ = membership.notified() => {
                if let Err(error) = reconcile_membership(conn, sink).await {
                    debug!("membership reconciliation interrupted: {error:?}");
                    return;
                }
            }
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else {
                    return;
                };
                if matches!(cmd, UiCommand::Shutdown) {
                    return;
                }
                if matches!(cmd, UiCommand::RefreshRecentProjects) {
                    refresh_recents = conn.recent_projects_supported;
                    continue;
                }
                if let UiCommand::PreviewImage { workspace_id, pane_id, token, path } = cmd {
                    if conn.image_preview_supported {
                        next_image = Some(image_requests::ImageRequest {
                            workspace_id, pane_id, token, path,
                        });
                    } else {
                        sink.emit(UiEvent::ImagePreview {
                            pane_id,
                            token,
                            result: Err("This daemon does not support workspace image previews.".into()),
                        });
                    }
                    continue;
                }
                if conn.workbench_supported && widgets.dispatch(&cmd) {
                    continue;
                }
                let close_id = match &cmd {
                    UiCommand::CloseWorkspace { id, .. } => Some(*id),
                    _ => None,
                };
                match handle_command(conn, sink, cmd).await {
                    Ok(()) => {}
                    Err(failure @ (RpcError::Disconnected | RpcError::TimedOut)) => {
                        let message = if matches!(failure, RpcError::TimedOut) {
                            "Daemon response timed out. Command outcome is unknown."
                        } else {
                            "Connection lost before the daemon confirmed the command."
                        };
                        sink.emit(UiEvent::Toast {
                            message: message.into(),
                            is_error: true,
                        });
                        return;
                    }
                    Err(RpcError::Proto(err)) => {
                        warn!(error = %err, "daemon RPC returned error");
                        sink.emit(UiEvent::Toast {
                            message: format!("Daemon error: {err}"),
                            is_error: true,
                        });
                    }
                }
                if let Some(id) = close_id {
                    sink.emit(UiEvent::WorkspaceCloseFinished(id));
                }
            }
        }
    }
}

async fn handle_command(
    conn: &RpcConnection,
    sink: &Arc<dyn UiEventSink>,
    cmd: UiCommand,
) -> Result<(), RpcError> {
    match cmd {
        UiCommand::InspectSetup {
            session,
            root_dir,
            executables,
        } => {
            setup::inspect(conn, sink, session, root_dir, executables).await?;
        }
        UiCommand::ApplySetup {
            workspace_id,
            assignments,
            launch,
            expected,
        } => {
            setup::apply(conn, sink, workspace_id, assignments, launch, expected).await?;
        }
        UiCommand::LaunchStoredSetup(workspace_id) => {
            setup::launch_stored(conn, sink, workspace_id).await?;
        }
        UiCommand::InstallSetupTool { workspace_id, argv } => {
            setup::install(conn, sink, workspace_id, argv).await?;
        }
        UiCommand::RefreshRecentProjects => unreachable!("recents are handled by the session loop"),
        UiCommand::SaveProjectSetup {
            workspace_id,
            assignments,
            request_id,
        } => {
            setup::save_project_setup(conn, sink, workspace_id, assignments, request_id).await?;
        }
        UiCommand::SaveWorkspaceSettings {
            workspace_id,
            assignments,
            expected,
            request_id,
        } => {
            setup::save_workspace_settings(
                conn,
                sink,
                workspace_id,
                assignments,
                expected,
                request_id,
            )
            .await?;
        }
        UiCommand::ReadActivity {
            workspace_id,
            cursor,
            token,
        } => {
            match conn
                .call(ClientMessage::ReadActivity {
                    workspace_id,
                    cursor,
                })
                .await
            {
                Ok(ResponsePayload::Activity(page)) if page.workspace_id == workspace_id => {
                    sink.emit(UiEvent::ActivityRead { token, page })
                }
                Err(err @ (RpcError::Disconnected | RpcError::TimedOut)) => return Err(err),
                _ => sink.emit(UiEvent::ActivityReadFailed(token)),
            }
        }
        UiCommand::Workbench {
            workspace_id,
            operation,
            request,
        } => {
            if conn.workbench_supported {
                let response = if matches!(*request, ClientMessage::ResolveWorkspaceImport { .. })
                    && !conn.resolved_import_preview_supported
                {
                    Err("Upgrade the daemon to review and apply a workspace with conflicts.".into())
                } else {
                    match conn.call(*request).await {
                        Ok(response) => Ok(response),
                        Err(RpcError::Proto(error)) => Err(error.to_string()),
                        Err(error) => return Err(error),
                    }
                };
                sink.emit(UiEvent::Workbench {
                    workspace_id,
                    operation,
                    response: Box::new(response),
                });
            }
        }
        UiCommand::ReconcileWorkspaceMembership => reconcile_membership(conn, sink).await?,
        UiCommand::CreateManualWorkspace {
            session,
            name,
            root_dir,
        } => {
            workspace::create_manual_workspace(conn, sink, session, name, root_dir).await?;
        }
        UiCommand::CreateWorkspace {
            name,
            root_dir,
            agent_preset,
        } => {
            create_and_populate_workspace(
                conn,
                sink,
                name,
                root_dir,
                agent_preset,
                DEFAULT_PANE_COLS,
                DEFAULT_PANE_ROWS,
            )
            .await?;
        }
        UiCommand::CreateWorktreeWorkspace {
            repo_dir,
            branch,
            default_agent,
        } => {
            create_worktree_and_populate_workspace(
                conn,
                sink,
                repo_dir,
                branch,
                default_agent,
                DEFAULT_PANE_COLS,
                DEFAULT_PANE_ROWS,
            )
            .await?;
        }
        UiCommand::ClosePane(pane_id) => {
            conn.call(ClientMessage::ClosePane { pane_id }).await?;
            sink.emit(UiEvent::PaneClosed { pane_id });
        }
        UiCommand::CloseWorkspace {
            id,
            compose_down,
            remove_worktree,
            force_remove_worktree,
        } => {
            workspace::close_workspace(
                conn,
                sink,
                id,
                compose_down,
                remove_worktree,
                force_remove_worktree,
            )
            .await?;
        }
        UiCommand::LaunchAssignment {
            workspace_id,
            assignment,
            launch,
            cols,
            rows,
        } => {
            setup::launch_assignment(conn, sink, workspace_id, assignment, launch, cols, rows)
                .await?;
        }
        UiCommand::AddAgent {
            workspace_id,
            preset,
            cols,
            rows,
        } => {
            workspace::add_agent(conn, sink, workspace_id, preset, cols, rows).await?;
        }
        UiCommand::ReplaceAgentPreset {
            workspace_id,
            old_pane_id,
            preset,
            cols,
            rows,
        } => {
            replace_agent_preset(conn, sink, workspace_id, old_pane_id, preset, cols, rows).await?;
        }
        UiCommand::PreviewImage { .. } => {}
        UiCommand::RunWorkspaceAction {
            workspace_id,
            action_id,
        } => {
            conn.call(ClientMessage::RunWorkspaceAction {
                workspace_id,
                action_id,
            })
            .await?;
        }
        UiCommand::SendInput { pane_id, data } => {
            let _ = conn
                .call(ClientMessage::SendInput { pane_id, data })
                .await?;
        }
        UiCommand::Paste { pane_id, data } => {
            terminal::paste(conn, pane_id, data).await?;
        }
        UiCommand::ResizePane {
            pane_id,
            cols,
            rows,
        } => {
            let _ = conn
                .call(ClientMessage::ResizePane {
                    pane_id,
                    cols: cols.max(1),
                    rows: rows.max(1),
                })
                .await?;
        }
        UiCommand::RequestSnapshot { pane_id } => {
            terminal::snapshot(conn, sink, pane_id).await?;
        }
        UiCommand::RequestScrollback {
            pane_id,
            generation,
            offset_from_bottom,
            count,
        } => {
            terminal::scrollback(conn, sink, pane_id, generation, offset_from_bottom, count)
                .await?;
        }
        UiCommand::SearchPane {
            pane_id,
            query,
            case_sensitive,
            generation,
        } => {
            terminal::search(conn, sink, pane_id, query, case_sensitive, generation).await?;
        }
        UiCommand::EnsureWorkspacePanes {
            workspace_id,
            default_agent,
            manifest,
        } => {
            ensure_workspace_panes(
                conn,
                sink,
                workspace_id,
                default_agent,
                manifest,
                DEFAULT_PANE_COLS,
                DEFAULT_PANE_ROWS,
            )
            .await?;
        }
        UiCommand::ForwardRunnerFailureToAgent {
            runner_pane_id,
            agent_pane_id,
        } => {
            terminal::forward_failure(conn, sink, runner_pane_id, agent_pane_id).await?;
        }
        UiCommand::Shutdown => {}
    }

    Ok(())
}

#[cfg(test)]
mod test_support {
    use super::{UiEvent, UiEventSink};
    use daedalus_proto::transport::{IpcListener, IpcStream};
    use std::path::Path;
    pub(super) struct NoopSink;
    impl UiEventSink for NoopSink {
        fn emit(&self, _event: UiEvent) {}
    }

    pub(super) async fn ipc_pair() -> (IpcStream, IpcStream) {
        let mut listener = IpcListener::bind(Path::new("tcp://127.0.0.1:0"))
            .await
            .unwrap();
        let endpoint = listener.local_endpoint();
        let (client_res, server_res) =
            tokio::join!(IpcStream::connect(&endpoint), listener.accept());
        (client_res.unwrap(), server_res.unwrap())
    }
}
