use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use daedalus_proto::{
    ClientMessage, GridRow, GridSnapshot, PaneId, PaneRole, PaneSummary, ProtoError, Request,
    ResponsePayload, ServerEvent, ServerMessage, WorkspaceId, WorkspaceInfo, read_frame,
    write_frame,
};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle as TokioJoinHandle;
use tracing::{debug, info, warn};

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
#[derive(Debug, Clone)]
pub struct BridgeConfig {
    pub socket_path: PathBuf,
    pub initial_dir: PathBuf,
    pub default_agent: AgentPreset,
    pub auto_spawn_daemon: bool,
}

/// Commands sent from the UI thread to `DaemonBridge`.
#[derive(Debug, Clone)]
pub enum UiCommand {
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
    CloseWorkspace {
        id: WorkspaceId,
    },
    ReplaceAgentPreset {
        workspace_id: WorkspaceId,
        old_pane_id: Option<PaneId>,
        preset: AgentPreset,
        cols: u16,
        rows: u16,
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
        offset_from_bottom: usize,
        count: usize,
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
    Connected,
    Disconnected,
    WorkspacesLoaded(Vec<WorkspaceInfo>),
    WorkspaceCreated(WorkspaceInfo),
    PaneSpawned {
        summary: PaneSummary,
        agent_preset: Option<AgentPreset>,
    },
    PaneClosed {
        pane_id: PaneId,
    },
    Snapshot(GridSnapshot),
    ScrollbackLoaded {
        pane_id: PaneId,
        total_scrollback: usize,
        rows: Vec<GridRow>,
    },
    ServerEvent(ServerEvent),
    Toast {
        message: String,
        is_error: bool,
    },
}

/// Background async bridge between the UI thread and `daedalusd`.
pub struct DaemonBridge {
    cmd_tx: mpsc::UnboundedSender<UiCommand>,
    thread_handle: Option<JoinHandle<()>>,
}

impl DaemonBridge {
    /// Spawns a dedicated 2-worker Tokio runtime thread and begins connecting to `daedalusd`.
    pub fn start(config: BridgeConfig, sink: Arc<dyn UiEventSink>) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let thread_handle = std::thread::Builder::new()
            .name("daedalus-bridge".to_string())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .expect("failed to build Tokio runtime for DaemonBridge");
                runtime.block_on(run_bridge(config, sink, cmd_rx));
            })
            .expect("failed to spawn DaemonBridge thread");

        Self {
            cmd_tx,
            thread_handle: Some(thread_handle),
        }
    }

    /// Enqueues a command for the background bridge runtime.
    pub fn send(&self, cmd: UiCommand) {
        let _ = self.cmd_tx.send(cmd);
    }

    /// Requests a clean shutdown and waits for the background runtime thread to exit.
    pub fn shutdown(mut self) {
        let _ = self.cmd_tx.send(UiCommand::Shutdown);
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for DaemonBridge {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(UiCommand::Shutdown);
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }
}

#[derive(Debug)]
enum RpcError {
    Disconnected,
    Proto(ProtoError),
}

type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<ResponsePayload, ProtoError>>>>>;

struct RpcConnection {
    write_tx: mpsc::UnboundedSender<Request>,
    pending: PendingMap,
    next_id: AtomicU64,
    disconnect_rx: watch::Receiver<bool>,
    reader_task: TokioJoinHandle<()>,
    writer_task: TokioJoinHandle<()>,
}

impl RpcConnection {
    fn new(stream: UnixStream, sink: Arc<dyn UiEventSink>) -> Self {
        let (mut reader, mut writer) = stream.into_split();
        let (write_tx, mut write_rx) = mpsc::unbounded_channel::<Request>();
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (disconnect_tx, disconnect_rx) = watch::channel(false);

        let writer_pending = Arc::clone(&pending);
        let writer_disconnect = disconnect_tx.clone();
        let writer_task = tokio::spawn(async move {
            while let Some(req) = write_rx.recv().await {
                if let Err(err) = write_frame(&mut writer, &req).await {
                    debug!(error = %err, "bridge write_frame failed");
                    break;
                }
            }
            writer_pending
                .lock()
                .expect("pending mutex poisoned")
                .clear();
            let _ = writer_disconnect.send(true);
        });

        let reader_pending = Arc::clone(&pending);
        let reader_disconnect = disconnect_tx;
        let reader_task = tokio::spawn(async move {
            loop {
                match read_frame::<_, ServerMessage>(&mut reader).await {
                    Ok(Some(ServerMessage::Event(event))) => {
                        sink.emit(UiEvent::ServerEvent(event));
                    }
                    Ok(Some(ServerMessage::Response { id, result })) => {
                        let tx = reader_pending
                            .lock()
                            .expect("pending mutex poisoned")
                            .remove(&id);
                        if let Some(tx) = tx {
                            let _ = tx.send(result);
                        }
                    }
                    Ok(None) => {
                        debug!("daedalusd socket closed cleanly");
                        break;
                    }
                    Err(err) => {
                        warn!(error = %err, "daedalusd read_frame failed");
                        break;
                    }
                }
            }
            // Drop all pending waiters so in-flight RPC calls immediately see Disconnected.
            reader_pending
                .lock()
                .expect("pending mutex poisoned")
                .clear();
            let _ = reader_disconnect.send(true);
        });

        Self {
            write_tx,
            pending,
            next_id: AtomicU64::new(1),
            disconnect_rx,
            reader_task,
            writer_task,
        }
    }

    async fn call(&self, payload: ClientMessage) -> Result<ResponsePayload, RpcError> {
        let mut disconnect_rx = self.disconnect_rx.clone();
        if *disconnect_rx.borrow() {
            return Err(RpcError::Disconnected);
        }

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        {
            let mut guard = self.pending.lock().expect("pending mutex poisoned");
            guard.insert(id, tx);
        }

        if self.write_tx.send(Request { id, payload }).is_err() {
            self.pending
                .lock()
                .expect("pending mutex poisoned")
                .remove(&id);
            return Err(RpcError::Disconnected);
        }

        tokio::select! {
            biased;
            res = rx => match res {
                Ok(Ok(resp)) => Ok(resp),
                Ok(Err(proto_err)) => Err(RpcError::Proto(proto_err)),
                Err(_) => Err(RpcError::Disconnected),
            },
            _ = disconnect_rx.wait_for(|&disconnected| disconnected) => {
                self.pending
                    .lock()
                    .expect("pending mutex poisoned")
                    .remove(&id);
                Err(RpcError::Disconnected)
            }
        }
    }

    async fn disconnected(&mut self) {
        if *self.disconnect_rx.borrow() {
            return;
        }
        while self.disconnect_rx.changed().await.is_ok() {
            if *self.disconnect_rx.borrow() {
                return;
            }
        }
    }

    fn close(self) {
        drop(self.write_tx);
        self.reader_task.abort();
        self.writer_task.abort();
    }
}

async fn run_bridge(
    config: BridgeConfig,
    sink: Arc<dyn UiEventSink>,
    mut cmd_rx: mpsc::UnboundedReceiver<UiCommand>,
) {
    let mut bootstrapped = false;
    let mut backoff = INITIAL_BACKOFF;
    let mut spawn_attempted = false;

    loop {
        match UnixStream::connect(&config.socket_path).await {
            Ok(stream) => {
                backoff = INITIAL_BACKOFF;
                spawn_attempted = false;
                let mut conn = RpcConnection::new(stream, Arc::clone(&sink));

                match initialize_session(&conn, &config, &sink, &mut bootstrapped).await {
                    Ok(()) => {}
                    Err(RpcError::Disconnected) => {
                        conn.close();
                        sink.emit(UiEvent::Disconnected);
                        if !sleep_or_shutdown(RECONNECT_DELAY, &mut cmd_rx).await {
                            return;
                        }
                        continue;
                    }
                    Err(RpcError::Proto(err)) => {
                        warn!(error = %err, "session initialization RPC failed");
                        sink.emit(UiEvent::Toast {
                            message: format!("Daemon error: {err}"),
                            is_error: true,
                        });
                        conn.close();
                        sink.emit(UiEvent::Disconnected);
                        if !sleep_or_shutdown(RECONNECT_DELAY, &mut cmd_rx).await {
                            return;
                        }
                        continue;
                    }
                }

                let should_exit = session_loop(&mut conn, &sink, &mut cmd_rx).await;
                conn.close();
                if should_exit {
                    return;
                }

                sink.emit(UiEvent::Disconnected);
                if !sleep_or_shutdown(RECONNECT_DELAY, &mut cmd_rx).await {
                    return;
                }
            }
            Err(err) => {
                debug!(
                    socket = %config.socket_path.display(),
                    error = %err,
                    "unable to connect to daedalusd"
                );
                if config.auto_spawn_daemon && !spawn_attempted {
                    spawn_attempted = true;
                    match spawn_daemon_detached(&config.socket_path) {
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

                if !sleep_or_shutdown(backoff, &mut cmd_rx).await {
                    return;
                }
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }
}

/// Sleeps for `duration` unless `UiCommand::Shutdown` arrives (or the command channel closes).
/// Returns `true` to keep running, `false` to exit the bridge.
async fn sleep_or_shutdown(
    duration: Duration,
    cmd_rx: &mut mpsc::UnboundedReceiver<UiCommand>,
) -> bool {
    let sleep = tokio::time::sleep(duration);
    tokio::pin!(sleep);
    loop {
        tokio::select! {
            _ = &mut sleep => return true,
            cmd = cmd_rx.recv() => {
                match cmd {
                    None | Some(UiCommand::Shutdown) => return false,
                    Some(_) => {
                        // Ignore commands while disconnected; UI will resync after reconnect.
                    }
                }
            }
        }
    }
}

async fn initialize_session(
    conn: &RpcConnection,
    config: &BridgeConfig,
    sink: &Arc<dyn UiEventSink>,
    bootstrapped: &mut bool,
) -> Result<(), RpcError> {
    let _ = conn.call(ClientMessage::Ping).await?;
    let _ = conn
        .call(ClientMessage::Subscribe { pane_ids: None })
        .await?;
    sink.emit(UiEvent::Connected);

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

/// Runs the connected command loop. Returns `true` if shutdown was requested,
/// or `false` if the daemon connection was lost.
async fn session_loop(
    conn: &mut RpcConnection,
    sink: &Arc<dyn UiEventSink>,
    cmd_rx: &mut mpsc::UnboundedReceiver<UiCommand>,
) -> bool {
    loop {
        tokio::select! {
            biased;
            _ = conn.disconnected() => {
                return false;
            }
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else {
                    return true;
                };
                if matches!(cmd, UiCommand::Shutdown) {
                    return true;
                }
                match handle_command(conn, sink, cmd).await {
                    Ok(()) => {}
                    Err(RpcError::Disconnected) => return false,
                    Err(RpcError::Proto(err)) => {
                        warn!(error = %err, "daemon RPC returned error");
                        sink.emit(UiEvent::Toast {
                            message: format!("Daemon error: {err}"),
                            is_error: true,
                        });
                    }
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
        UiCommand::CloseWorkspace { id } => {
            let _ = conn
                .call(ClientMessage::CloseWorkspace {
                    id,
                    compose_down: false,
                })
                .await?;
            if let ResponsePayload::Workspaces(ws) =
                conn.call(ClientMessage::ListWorkspaces).await?
            {
                sink.emit(UiEvent::WorkspacesLoaded(ws));
            }
        }
        UiCommand::ReplaceAgentPreset {
            workspace_id,
            old_pane_id,
            preset,
            cols,
            rows,
        } => {
            if let Some(command) = preset.command()
                && let Some(binary) = command.first()
                && find_binary_on_path(binary).is_none()
            {
                sink.emit(UiEvent::Toast {
                    message: format!("Agent CLI '{binary}' not found on $PATH"),
                    is_error: true,
                });
                return Ok(());
            }

            if let Some(old_id) = old_pane_id {
                let _ = conn
                    .call(ClientMessage::ClosePane { pane_id: old_id })
                    .await?;
                sink.emit(UiEvent::PaneClosed { pane_id: old_id });
            }

            let summary = match conn
                .call(ClientMessage::SpawnPane {
                    workspace_id,
                    role: PaneRole::Agent,
                    command: preset.command(),
                    shell: None,
                    cols: cols.max(1),
                    rows: rows.max(1),
                    env: HashMap::new(),
                })
                .await?
            {
                ResponsePayload::PaneSpawned(summary) => summary,
                other => {
                    warn!(response = ?other, "unexpected response to SpawnPane");
                    return Ok(());
                }
            };

            let pane_id = summary.id;
            sink.emit(UiEvent::PaneSpawned {
                summary,
                agent_preset: Some(preset),
            });

            if let ResponsePayload::Snapshot(snapshot) =
                conn.call(ClientMessage::GetSnapshot { pane_id }).await?
            {
                sink.emit(UiEvent::Snapshot(snapshot));
            }

            sink.emit(UiEvent::Toast {
                message: format!("Switched Agent pane to {}", preset.short_name()),
                is_error: false,
            });
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
                    total_scrollback,
                    rows,
                });
            }
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

async fn create_and_populate_workspace(
    conn: &RpcConnection,
    sink: &Arc<dyn UiEventSink>,
    name: String,
    root_dir: PathBuf,
    requested_agent: AgentPreset,
    cols: u16,
    rows: u16,
) -> Result<(), RpcError> {
    let ws = match conn
        .call(ClientMessage::CreateWorkspace {
            name,
            root_dir,
            env: HashMap::new(),
        })
        .await?
    {
        ResponsePayload::WorkspaceCreated(ws) => ws,
        other => {
            warn!(response = ?other, "unexpected response to CreateWorkspace");
            return Ok(());
        }
    };

    populate_workspace_panes(conn, sink, ws, requested_agent, cols, rows).await
}

async fn create_worktree_and_populate_workspace(
    conn: &RpcConnection,
    sink: &Arc<dyn UiEventSink>,
    repo_dir: PathBuf,
    branch: String,
    requested_agent: AgentPreset,
    cols: u16,
    rows: u16,
) -> Result<(), RpcError> {
    let first_attempt = conn
        .call(ClientMessage::CreateWorktreeWorkspace {
            repo_dir: repo_dir.clone(),
            branch: branch.clone(),
            create_branch: true,
            env: HashMap::new(),
        })
        .await;

    let response = match first_attempt {
        Err(RpcError::Proto(ProtoError::InvalidRequest(ref msg)))
            if msg.contains("git worktree add failed") && msg.contains("already exists") =>
        {
            conn.call(ClientMessage::CreateWorktreeWorkspace {
                repo_dir,
                branch,
                create_branch: false,
                env: HashMap::new(),
            })
            .await?
        }
        other => other?,
    };

    let ws = match response {
        ResponsePayload::WorkspaceCreated(ws) => ws,
        other => {
            warn!(response = ?other, "unexpected response to CreateWorktreeWorkspace");
            return Ok(());
        }
    };

    populate_workspace_panes(conn, sink, ws, requested_agent, cols, rows).await
}

async fn populate_workspace_panes(
    conn: &RpcConnection,
    sink: &Arc<dyn UiEventSink>,
    ws: WorkspaceInfo,
    requested_agent: AgentPreset,
    cols: u16,
    rows: u16,
) -> Result<(), RpcError> {
    let ws_id = ws.id;
    sink.emit(UiEvent::WorkspaceCreated(ws));

    let effective_agent = resolve_agent_preset(requested_agent, sink);

    let pane_specs = [
        (
            PaneRole::Agent,
            effective_agent.command(),
            Some(effective_agent),
        ),
        (PaneRole::Service, None, None),
        (PaneRole::Runner, None, None),
    ];

    for (role, command, agent_preset) in pane_specs {
        let summary = match conn
            .call(ClientMessage::SpawnPane {
                workspace_id: ws_id,
                role,
                command,
                shell: None,
                cols,
                rows,
                env: HashMap::new(),
            })
            .await?
        {
            ResponsePayload::PaneSpawned(summary) => summary,
            other => {
                warn!(role = ?role, response = ?other, "unexpected response to SpawnPane");
                continue;
            }
        };

        let pane_id = summary.id;
        sink.emit(UiEvent::PaneSpawned {
            summary,
            agent_preset,
        });

        if let ResponsePayload::Snapshot(snapshot) =
            conn.call(ClientMessage::GetSnapshot { pane_id }).await?
        {
            sink.emit(UiEvent::Snapshot(snapshot));
        }
    }

    Ok(())
}

fn resolve_agent_preset(requested: AgentPreset, sink: &Arc<dyn UiEventSink>) -> AgentPreset {
    if let Some(command) = requested.command()
        && let Some(binary) = command.first()
        && find_binary_on_path(binary).is_none()
    {
        sink.emit(UiEvent::Toast {
            message: format!("Agent CLI '{binary}' not found on $PATH; falling back to shell"),
            is_error: true,
        });
        return AgentPreset::Shell;
    }
    requested
}

fn workspace_name_from_dir(dir: &Path) -> String {
    dir.file_name()
        .and_then(|name| name.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("workspace")
        .to_string()
}

fn resolve_daemon_binary(current_exe: Option<&Path>) -> PathBuf {
    if let Some(exe) = current_exe
        && let Some(parent) = exe.parent()
    {
        let sibling = parent.join("daedalusd");
        if is_executable_file(&sibling) {
            return sibling;
        }
    }
    PathBuf::from("daedalusd")
}

fn spawn_daemon_detached(socket_path: &Path) -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe().ok();
    let bin = resolve_daemon_binary(exe.as_deref());

    let mut cmd = Command::new(&bin);
    cmd.arg("--socket")
        .arg(socket_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    // SAFETY: `setsid()` is async-signal-safe per POSIX and detaches `daedalusd`
    // into its own session so it outlives the UI process cleanly.
    unsafe {
        cmd.pre_exec(|| {
            nix::unistd::setsid()
                .map_err(|errno| std::io::Error::from_raw_os_error(errno as i32))?;
            Ok(())
        });
    }

    let _child = cmd.spawn()?;
    Ok(bin)
}

fn find_binary_on_path(binary: &str) -> Option<PathBuf> {
    if binary.contains('/') {
        let path = PathBuf::from(binary);
        return is_executable_file(&path).then_some(path);
    }
    let path_var = std::env::var_os("PATH")?;
    let dirs: Vec<PathBuf> = std::env::split_paths(&path_var).collect();
    find_binary_in_paths(binary, &dirs)
}

fn find_binary_in_paths(binary: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    for dir in dirs {
        let candidate = dir.join(binary);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && (meta.permissions().mode() & 0o111 != 0))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn workspace_name_extracts_basename_or_fallback() {
        assert_eq!(
            workspace_name_from_dir(Path::new("/home/tim/daedalus")),
            "daedalus"
        );
        assert_eq!(workspace_name_from_dir(Path::new("/")), "workspace");
        assert_eq!(workspace_name_from_dir(Path::new("")), "workspace");
    }

    #[test]
    fn find_binary_in_paths_checks_executable_bit() {
        let dir = TempDir::new().unwrap();
        let non_exec = dir.path().join("plain-file");
        fs::write(&non_exec, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&non_exec, fs::Permissions::from_mode(0o644)).unwrap();

        let exec = dir.path().join("my-agent");
        fs::write(&exec, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&exec, fs::Permissions::from_mode(0o755)).unwrap();

        let sub = dir.path().join("subdir");
        fs::create_dir(&sub).unwrap();

        let dirs = vec![dir.path().to_path_buf()];
        assert_eq!(find_binary_in_paths("plain-file", &dirs), None);
        assert_eq!(find_binary_in_paths("subdir", &dirs), None);
        assert_eq!(find_binary_in_paths("missing", &dirs), None);
        assert_eq!(find_binary_in_paths("my-agent", &dirs), Some(exec));
    }

    #[test]
    fn resolve_daemon_binary_prefers_sibling_executable() {
        let dir = TempDir::new().unwrap();
        let fake_ui = dir.path().join("daedalus-ui");
        fs::write(&fake_ui, "").unwrap();

        // Without sibling daedalusd -> falls back to PATH lookup name
        assert_eq!(
            resolve_daemon_binary(Some(&fake_ui)),
            PathBuf::from("daedalusd")
        );

        // With executable sibling daedalusd -> returns sibling path
        let sibling = dir.path().join("daedalusd");
        fs::write(&sibling, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&sibling, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(resolve_daemon_binary(Some(&fake_ui)), sibling);
    }

    struct NoopSink;
    impl UiEventSink for NoopSink {
        fn emit(&self, _event: UiEvent) {}
    }

    #[tokio::test]
    async fn rpc_call_fails_immediately_when_disconnected_before_or_during_call() {
        // Case 1: peer closes before `call()` is invoked
        let (client_stream, server_stream) = UnixStream::pair().unwrap();
        let mut conn = RpcConnection::new(client_stream, Arc::new(NoopSink));
        drop(server_stream);

        tokio::time::timeout(Duration::from_secs(2), conn.disconnected())
            .await
            .expect("expected connection to notice peer EOF");

        let res = tokio::time::timeout(Duration::from_secs(2), conn.call(ClientMessage::Ping))
            .await
            .expect("call() deadlocked after disconnect");
        assert!(matches!(res, Err(RpcError::Disconnected)));
        assert!(conn.pending.lock().unwrap().is_empty());
        conn.close();

        // Case 2: peer closes while `call()` is awaiting a response
        let (client_stream, mut server_stream) = UnixStream::pair().unwrap();
        let conn = RpcConnection::new(client_stream, Arc::new(NoopSink));

        let peer_task = tokio::spawn(async move {
            // Read the request so it's definitely in flight, then drop without replying.
            let _req: Option<Request> = read_frame(&mut server_stream).await.unwrap();
            drop(server_stream);
        });

        let res = tokio::time::timeout(Duration::from_secs(2), conn.call(ClientMessage::Ping))
            .await
            .expect("call() deadlocked when peer dropped mid-RPC");
        assert!(matches!(res, Err(RpcError::Disconnected)));
        peer_task.await.unwrap();
        assert!(conn.pending.lock().unwrap().is_empty());
        conn.close();
    }
}
