mod launch;
mod rpc;
mod workspace;

use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use daedalus_proto::{
    AgentState, ClientMessage, ContainerHealth, GitWorkspaceStatus, GridRow, GridSnapshot, PaneId,
    PaneSummary, PortMapping, ProtoError, ResponsePayload, SearchMatch, ServerEvent, WorkspaceId,
    WorkspaceInfo, WorkspaceManifest,
};
use launch::{connect_endpoint, should_auto_spawn_daemon, spawn_daemon_detached};
use rpc::{RpcConnection, RpcError};
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};
use workspace::{
    create_and_populate_workspace, create_worktree_and_populate_workspace, ensure_workspace_panes,
    replace_agent_preset, workspace_name_from_dir,
};

use crate::input::format_failure_trace_prompt;
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
    pub default_agent: AgentPreset,
    pub auto_spawn_daemon: bool,
}

impl std::fmt::Debug for BridgeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BridgeConfig")
            .field("socket_path", &self.socket_path)
            .field("initial_dir", &self.initial_dir)
            .field("default_agent", &self.default_agent)
            .field("auto_spawn_daemon", &self.auto_spawn_daemon)
            .field(
                "auth_token",
                &self.auth_token.as_ref().map(|_| "[redacted]"),
            )
            .finish()
    }
}

/// Commands sent from the UI thread to `DaemonBridge`.
#[derive(Debug, Clone)]
pub enum UiCommand {
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
    JournalCapability(bool),
    ActivityRead {
        token: u64,
        page: daedalus_proto::ActivityPage,
    },
    ActivityReadFailed(u64),
    Connected,
    Disconnected,
    WorkspacesLoaded(Vec<WorkspaceInfo>),
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
        agent_preset: Option<AgentPreset>,
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
            Ok((stream, _tunnel)) => {
                backoff = INITIAL_BACKOFF;
                spawn_attempted = false;
                let mut conn = RpcConnection::new(stream, Arc::clone(&sink));

                // A queue belongs to exactly one connection. Dropping the connection
                // also invalidates commands accepted just before its loss.
                let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
                let initialized =
                    initialize_session(&conn, &config, &sink, &mut bootstrapped, &session, cmd_tx)
                        .await;
                match initialized {
                    Ok(()) => {
                        session_loop(&mut conn, &sink, &mut cmd_rx).await;
                    }
                    Err(RpcError::Disconnected) => {}
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

async fn initialize_session(
    conn: &RpcConnection,
    config: &BridgeConfig,
    sink: &Arc<dyn UiEventSink>,
    bootstrapped: &mut bool,
    session: &watch::Sender<Option<mpsc::UnboundedSender<UiCommand>>>,
    cmd_tx: mpsc::UnboundedSender<UiCommand>,
) -> Result<(), RpcError> {
    if let Some(token) = config.auth_token.as_ref().filter(|token| !token.is_empty()) {
        let response = conn
            .call(ClientMessage::Authenticate {
                token: token.clone(),
            })
            .await?;
        if !matches!(response, ResponsePayload::Ok) {
            return Err(RpcError::Proto(ProtoError::InvalidRequest(
                "unexpected authentication response".into(),
            )));
        }
    }
    let journal = matches!(
        conn.call(ClientMessage::Ping).await?,
        ResponsePayload::Pong {
            journal_supported: true,
            ..
        }
    );
    let _ = conn
        .call(ClientMessage::Subscribe { pane_ids: None })
        .await?;
    session.send_replace(Some(cmd_tx));
    sink.emit(UiEvent::Connected);
    sink.emit(UiEvent::JournalCapability(journal));

    let workspaces = match conn.call(ClientMessage::ListWorkspaces).await? {
        ResponsePayload::Workspaces(ws) => ws,
        _ => Vec::new(),
    };

    if workspaces.is_empty() && !*bootstrapped {
        let name = workspace_name_from_dir(&config.initial_dir);
        create_and_populate_workspace(
            conn,
            sink,
            name,
            config.initial_dir.clone(),
            config.default_agent,
            DEFAULT_PANE_COLS,
            DEFAULT_PANE_ROWS,
        )
        .await?;
        *bootstrapped = true;
    } else {
        *bootstrapped = true;
        sink.emit(UiEvent::WorkspacesLoaded(workspaces.clone()));
        for ws in &workspaces {
            for pane in &ws.panes {
                if let ResponsePayload::Snapshot(snapshot) = conn
                    .call(ClientMessage::GetSnapshot { pane_id: pane.id })
                    .await?
                {
                    sink.emit(UiEvent::Snapshot(snapshot));
                }
            }
        }
    }

    Ok(())
}

/// Processes commands for a single connection until it closes.
async fn session_loop(
    conn: &mut RpcConnection,
    sink: &Arc<dyn UiEventSink>,
    cmd_rx: &mut mpsc::UnboundedReceiver<UiCommand>,
) {
    loop {
        tokio::select! {
            biased;
            _ = conn.disconnected() => {
                return;
            }
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else {
                    return;
                };
                if matches!(cmd, UiCommand::Shutdown) {
                    return;
                }
                let close_id = match &cmd {
                    UiCommand::CloseWorkspace { id, .. } => Some(*id),
                    _ => None,
                };
                match handle_command(conn, sink, cmd).await {
                    Ok(()) => {}
                    Err(RpcError::Disconnected) => {
                        sink.emit(UiEvent::Toast {
                            message: "Connection lost before the daemon confirmed the command.".into(),
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
                Err(RpcError::Disconnected) => return Err(RpcError::Disconnected),
                _ => sink.emit(UiEvent::ActivityReadFailed(token)),
            }
        }
        UiCommand::CreateManualWorkspace {
            session,
            name,
            root_dir,
        } => {
            let event = match conn
                .call(ClientMessage::CreateManualWorkspace { name, root_dir })
                .await
            {
                Ok(ResponsePayload::WorkspaceCreated(workspace)) => {
                    UiEvent::ManualWorkspaceCreated { session, workspace }
                }
                Ok(_) => UiEvent::ManualWorkspaceFailed {
                    session,
                    message: "Unexpected reply to workspace creation.".into(),
                },
                Err(RpcError::Proto(error)) => UiEvent::ManualWorkspaceFailed {
                    session,
                    message: error.to_string(),
                },
                Err(RpcError::Disconnected) => return Err(RpcError::Disconnected),
            };
            sink.emit(event);
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
            let close = conn
                .call(ClientMessage::CloseWorkspace {
                    id,
                    compose_down,
                    remove_worktree,
                    force_remove_worktree,
                })
                .await;
            if matches!(close, Err(RpcError::Disconnected)) {
                return Err(RpcError::Disconnected);
            }
            if let ResponsePayload::Workspaces(ws) =
                conn.call(ClientMessage::ListWorkspaces).await?
            {
                if close.is_ok() {
                    sink.emit(UiEvent::WorkspacesLoaded(ws));
                } else {
                    sink.emit(UiEvent::WorkspaceCloseFailed(ws));
                }
            }
            close?;
        }
        UiCommand::LaunchAssignment {
            workspace_id,
            assignment,
            cols,
            rows,
        } => {
            let preset = (assignment.role == daedalus_proto::PaneRole::Agent)
                .then(|| {
                    assignment
                        .argv
                        .first()
                        .and_then(|program| AgentPreset::from_cli_name(program))
                })
                .flatten();
            let response = conn
                .call(ClientMessage::SpawnPane {
                    workspace_id,
                    role: assignment.role,
                    command: (!assignment.argv.is_empty()).then(|| assignment.argv.clone()),
                    cols,
                    rows,
                    shell: None,
                    env: Default::default(),
                })
                .await?;
            let ResponsePayload::PaneSpawned(summary) = response else {
                return Err(RpcError::Proto(ProtoError::Internal(
                    "unexpected assignment launch response".into(),
                )));
            };
            let pane_id = summary.id;
            sink.emit(UiEvent::PaneSpawned {
                summary,
                agent_preset: preset,
            });
            sink.emit(UiEvent::AssignmentStarted {
                workspace_id,
                pane_id,
                assignment,
            });
            if let ResponsePayload::Snapshot(snapshot) =
                conn.call(ClientMessage::GetSnapshot { pane_id }).await?
            {
                sink.emit(UiEvent::Snapshot(snapshot));
            }
        }
        UiCommand::AddAgent {
            workspace_id,
            preset,
            cols,
            rows,
        } => {
            let response = conn
                .call(ClientMessage::SpawnPane {
                    workspace_id,
                    role: daedalus_proto::PaneRole::Agent,
                    command: preset.command(),
                    shell: None,
                    env: Default::default(),
                    cols,
                    rows,
                })
                .await?;
            if let ResponsePayload::PaneSpawned(summary) = response {
                workspace::publish_pane(conn, sink, summary, Some(preset)).await?;
            }
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
            if let ResponsePayload::Snapshot(snapshot) =
                conn.call(ClientMessage::GetSnapshot { pane_id }).await?
            {
                sink.emit(UiEvent::Snapshot(snapshot));
            }
        }
        UiCommand::RequestScrollback {
            pane_id,
            generation,
            offset_from_bottom,
            count,
        } => {
            if let ResponsePayload::ScrollbackRows {
                pane_id,
                total_scrollback,
                rows,
            } = conn
                .call(ClientMessage::GetScrollback {
                    pane_id,
                    offset_from_bottom,
                    count,
                })
                .await?
            {
                sink.emit(UiEvent::ScrollbackLoaded {
                    pane_id,
                    generation,
                    total_scrollback,
                    rows,
                });
            }
        }
        UiCommand::SearchPane {
            pane_id,
            query,
            case_sensitive,
            generation,
        } => {
            match conn
                .call(ClientMessage::SearchPane {
                    pane_id,
                    query,
                    case_sensitive,
                })
                .await?
            {
                ResponsePayload::SearchMatches {
                    pane_id,
                    total_scrollback,
                    matches,
                } => {
                    sink.emit(UiEvent::SearchResults {
                        pane_id,
                        generation,
                        total_scrollback,
                        matches,
                    });
                }
                other => {
                    warn!(response = ?other, "unexpected response to SearchPane");
                }
            }
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
            match conn
                .call(ClientMessage::GetFailureTrace {
                    pane_id: runner_pane_id,
                })
                .await?
            {
                ResponsePayload::FailureTrace(Some(trace)) => {
                    let exit_code = trace.exit_code;
                    let data = format_failure_trace_prompt(&trace);
                    let _ = conn
                        .call(ClientMessage::SendInput {
                            pane_id: agent_pane_id,
                            data,
                        })
                        .await?;
                    sink.emit(UiEvent::Toast {
                        message: format!("Sent failure trace (exit {exit_code}) to Agent"),
                        is_error: false,
                    });
                }
                ResponsePayload::FailureTrace(None) => {
                    sink.emit(UiEvent::Toast {
                        message: "No failure trace captured in Runner pane yet".to_string(),
                        is_error: true,
                    });
                }
                other => {
                    warn!(response = ?other, "unexpected response to GetFailureTrace");
                }
            }
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
