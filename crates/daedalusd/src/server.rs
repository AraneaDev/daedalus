use crate::external_hook::ExternalHookServer;
use crate::persistence::StateStore;
use crate::registry::Registry;
use crate::runtime_monitor;
use crate::shell_integration::ShellIntegration;
use crate::supervisor::Supervisor;
use daedalus_proto::transport::{IpcListener, IpcStream};
use daedalus_proto::{
    ClientMessage, PaneId, ProtoError, Request, ResponsePayload, ServerEvent, ServerMessage,
    default_events_socket_path, default_state_file_path, read_frame, write_frame,
};
use daedalus_vt::{VtConfig, create_vt_engine};
use std::collections::HashSet;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{RwLock, broadcast, mpsc, watch};
use tokio::task::JoinSet;
use tracing::{debug, error, info, warn};

const CLIENT_OUTBOUND_CAPACITY: usize = 256;

pub struct DaemonServer {
    listener: IpcListener,
    registry: Arc<Registry>,
    external_hook: Option<ExternalHookServer>,
    auth_token: Option<String>,
}

impl DaemonServer {
    pub async fn bind(socket_path: PathBuf, runtime_dir: PathBuf) -> std::io::Result<Self> {
        let supervisor = Supervisor::detect();
        Self::bind_with_options(
            socket_path,
            runtime_dir,
            Some(default_events_socket_path()),
            Some(default_state_file_path()),
            supervisor,
        )
        .await
    }

    pub async fn bind_with_supervisor(
        socket_path: PathBuf,
        runtime_dir: PathBuf,
        supervisor: Supervisor,
    ) -> std::io::Result<Self> {
        Self::bind_with_options(socket_path, runtime_dir, None, None, supervisor).await
    }

    pub async fn bind_with_events_socket(
        socket_path: PathBuf,
        runtime_dir: PathBuf,
        events_socket_path: Option<PathBuf>,
        supervisor: Supervisor,
    ) -> std::io::Result<Self> {
        Self::bind_with_options(
            socket_path,
            runtime_dir,
            events_socket_path,
            None,
            supervisor,
        )
        .await
    }

    pub async fn bind_with_options(
        socket_path: PathBuf,
        runtime_dir: PathBuf,
        events_socket_path: Option<PathBuf>,
        state_path: Option<PathBuf>,
        supervisor: Supervisor,
    ) -> std::io::Result<Self> {
        std::fs::create_dir_all(&runtime_dir)?;

        let shell_integration = ShellIntegration::prepare(&runtime_dir)?;
        let (event_tx, _) = broadcast::channel(1024);
        let registry = Arc::new(Registry::new_with_state_store(
            supervisor,
            shell_integration,
            event_tx,
            state_path.map(StateStore::new),
        ));
        let listener = IpcListener::bind(&socket_path).await?;
        let external_hook = match events_socket_path {
            Some(events_path) => {
                match ExternalHookServer::bind(events_path, Arc::clone(&registry)).await {
                    Ok(hook) => Some(hook),
                    Err(err) => {
                        listener.cleanup();
                        return Err(err);
                    }
                }
            }
            None => None,
        };
        registry.restore_persisted_workspaces().await;
        let bound_endpoint = listener.local_endpoint();
        info!(socket = %bound_endpoint.display(), "daedalusd listening");

        Ok(Self {
            listener,
            registry,
            external_hook,
            auth_token: None,
        })
    }

    pub fn with_auth_token(mut self, token: Option<String>) -> Self {
        self.auth_token = token.filter(|t| !t.is_empty());
        self
    }

    pub fn socket_path(&self) -> PathBuf {
        self.listener.local_endpoint()
    }

    pub fn set_worktrees_dir(&self, dir: PathBuf) {
        self.registry.set_worktrees_dir(dir);
    }

    pub async fn serve(mut self, shutdown: impl Future<Output = ()>) -> std::io::Result<()> {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let monitor_handle =
            runtime_monitor::spawn_runtime_monitor(Arc::clone(&self.registry), shutdown_rx.clone());
        let hook_handle = self.external_hook.map(|hook| {
            let mut hook_shutdown = shutdown_rx.clone();
            tokio::spawn(async move {
                if let Err(e) = hook
                    .serve(async move {
                        while !*hook_shutdown.borrow() {
                            if hook_shutdown.changed().await.is_err() {
                                break;
                            }
                        }
                    })
                    .await
                {
                    error!(error = %e, "external hook server failed");
                }
            })
        });

        tokio::pin!(shutdown);
        let mut client_tasks = JoinSet::new();
        loop {
            tokio::select! {
                accept_res = self.listener.accept() => {
                    match accept_res {
                        Ok(stream) => {
                            let reg = Arc::clone(&self.registry);
                            let auth_token = self.auth_token.clone();
                            client_tasks.spawn(handle_client(stream, reg, auth_token));
                        }
                        Err(e) => {
                            error!(error = %e, "accept failed");
                        }
                    }
                    while client_tasks.try_join_next().is_some() {}
                }
                _ = &mut shutdown => {
                    info!("shutting down daedalusd");
                    break;
                }
            }
        }

        let _ = shutdown_tx.send(true);
        client_tasks.abort_all();
        while client_tasks.join_next().await.is_some() {}
        let _ = monitor_handle.await;
        if let Some(handle) = hook_handle {
            let _ = handle.await;
        }
        self.registry.close_all().await;
        self.listener.cleanup();
        Ok(())
    }
}

#[derive(Clone, Default)]
struct SubscriptionFilter {
    active: bool,
    pane_ids: Option<HashSet<PaneId>>,
}

impl SubscriptionFilter {
    fn matches(&self, event: &ServerEvent) -> bool {
        if !self.active {
            return false;
        }
        let Some(allowed) = &self.pane_ids else {
            return true;
        };
        match event {
            ServerEvent::GridDelta(d) => allowed.contains(&d.pane_id),
            ServerEvent::PaneStatusChanged { pane_id, .. }
            | ServerEvent::AgentStateChanged { pane_id, .. }
            | ServerEvent::CommandFinished { pane_id, .. }
            | ServerEvent::PaneExited { pane_id, .. }
            | ServerEvent::ResyncRequired { pane_id } => allowed.contains(pane_id),
            ServerEvent::WorkspaceRuntimeUpdated { .. }
            | ServerEvent::ExternalStatusUpdated { .. } => false,
        }
    }
}

async fn wait_for_done(done_rx: &mut watch::Receiver<bool>) {
    while !*done_rx.borrow() {
        if done_rx.changed().await.is_err() {
            break;
        }
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let max_len = a.len().max(b.len());
    let mut diff = a.len() ^ b.len();
    for i in 0..max_len {
        let byte_a = a.get(i).copied().unwrap_or(0);
        let byte_b = b.get(i).copied().unwrap_or(0);
        diff |= usize::from(byte_a ^ byte_b);
    }
    diff == 0
}

async fn handle_client(stream: IpcStream, registry: Arc<Registry>, auth_token: Option<String>) {
    let (mut reader, mut writer) = stream.into_split();
    let (out_tx, mut out_rx) = mpsc::channel::<ServerMessage>(CLIENT_OUTBOUND_CAPACITY);
    let filter = Arc::new(RwLock::new(SubscriptionFilter::default()));
    let (done_tx, mut done_rx) = watch::channel(false);

    let writer_fut = async move {
        while let Some(msg) = out_rx.recv().await {
            if let Err(e) = write_frame(&mut writer, &msg).await {
                debug!(error = %e, "client write failed");
                break;
            }
        }
    };

    let event_rx = registry.subscribe_events();
    let sub_out_tx = out_tx.clone();
    let sub_filter = Arc::clone(&filter);
    let event_fut = async move {
        tokio::select! {
            _ = forward_subscriber_events(event_rx, sub_out_tx, sub_filter) => {}
            _ = wait_for_done(&mut done_rx) => {}
        }
    };

    let reader_fut = async move {
        let mut authenticated = auth_token.is_none();
        loop {
            match read_frame::<_, Request>(&mut reader).await {
                Ok(Some(req)) => {
                    let result = match req.payload {
                        ClientMessage::Authenticate { token } => match &auth_token {
                            None => Ok(ResponsePayload::Ok),
                            Some(expected)
                                if constant_time_eq(token.as_bytes(), expected.as_bytes()) =>
                            {
                                authenticated = true;
                                Ok(ResponsePayload::Ok)
                            }
                            Some(_) => Err(ProtoError::Unauthorized(
                                "invalid authentication token".to_string(),
                            )),
                        },
                        _ if !authenticated => Err(ProtoError::Unauthorized(
                            "authentication required".to_string(),
                        )),
                        other => dispatch_request(&registry, &filter, other).await,
                    };
                    let resp = ServerMessage::Response { id: req.id, result };
                    if out_tx.send(resp).await.is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    warn!(error = %e, "client protocol error; disconnecting");
                    break;
                }
            }
        }
        let _ = done_tx.send(true);
        drop(out_tx);
    };

    tokio::join!(reader_fut, writer_fut, event_fut);
}

async fn dispatch_request(
    registry: &Registry,
    filter: &RwLock<SubscriptionFilter>,
    msg: ClientMessage,
) -> Result<ResponsePayload, ProtoError> {
    match msg {
        ClientMessage::Ping => {
            let vt_backend = create_vt_engine(VtConfig::default())
                .backend_name()
                .to_string();
            Ok(ResponsePayload::Pong {
                version: env!("CARGO_PKG_VERSION").to_string(),
                vt_backend,
                isolation_mode: registry.isolation_mode().as_str().to_string(),
            })
        }
        ClientMessage::Authenticate { .. } => Ok(ResponsePayload::Ok),
        ClientMessage::CreateWorkspace {
            name,
            root_dir,
            env,
        } => Ok(ResponsePayload::WorkspaceCreated(
            registry.create_workspace(name, root_dir, env).await?,
        )),
        ClientMessage::ResolveWorkspaceDirectory { root_dir } => Ok(
            ResponsePayload::WorkspaceResolved(registry.workspace_for_directory(root_dir).await?),
        ),
        ClientMessage::ListWorkspaces => Ok(ResponsePayload::Workspaces(
            registry.list_workspaces().await,
        )),
        ClientMessage::CloseWorkspace {
            id,
            compose_down,
            remove_worktree,
            force_remove_worktree,
        } => {
            registry
                .close_workspace(id, compose_down, remove_worktree, force_remove_worktree)
                .await?;
            Ok(ResponsePayload::Ok)
        }
        ClientMessage::SetAgentState {
            pane_id,
            workspace_id,
            root_dir,
            state,
        } => {
            registry
                .set_agent_state(pane_id, workspace_id, root_dir, state)
                .await?;
            Ok(ResponsePayload::Ok)
        }
        ClientMessage::ClearExternalStatus {
            workspace_id,
            root_dir,
            source,
        } => {
            registry
                .clear_external_status(workspace_id, root_dir, source)
                .await?;
            Ok(ResponsePayload::Ok)
        }
        ClientMessage::DiscoverWorktrees { repo_dir } => Ok(ResponsePayload::Worktrees(
            registry.discover_worktrees(repo_dir).await?,
        )),
        ClientMessage::CreateWorktreeWorkspace {
            repo_dir,
            branch,
            create_branch,
            env,
        } => Ok(ResponsePayload::WorkspaceCreated(
            registry
                .create_worktree_workspace(repo_dir, branch, create_branch, env)
                .await?,
        )),
        ClientMessage::PublishExternalStatus {
            workspace_id,
            root_dir,
            source,
            level,
            label,
            detail,
        } => {
            registry
                .publish_external_status(workspace_id, root_dir, source, level, label, detail)
                .await?;
            Ok(ResponsePayload::Ok)
        }
        ClientMessage::SpawnPane {
            workspace_id,
            role,
            command,
            shell,
            cols,
            rows,
            env,
        } => Ok(ResponsePayload::PaneSpawned(
            registry
                .spawn_pane(workspace_id, role, command, shell, cols, rows, env)
                .await?,
        )),
        ClientMessage::EnsureDefaultPane {
            workspace_id,
            role,
            command,
            cols,
            rows,
        } => {
            registry
                .ensure_default_pane(workspace_id, role, command, cols, rows)
                .await
        }
        ClientMessage::ReplaceAgentPane {
            workspace_id,
            old_pane_id,
            command,
            cols,
            rows,
        } => Ok(ResponsePayload::PaneSpawned(
            registry
                .replace_agent_pane(workspace_id, old_pane_id, command, cols, rows)
                .await?,
        )),
        ClientMessage::ClosePane { pane_id } => {
            registry.close_pane(pane_id).await?;
            Ok(ResponsePayload::Ok)
        }
        ClientMessage::SendInput { pane_id, data } => {
            registry.get_pane(pane_id).await?.send_input(data).await?;
            Ok(ResponsePayload::Ok)
        }
        ClientMessage::ResizePane {
            pane_id,
            cols,
            rows,
        } => {
            registry.get_pane(pane_id).await?.resize(cols, rows).await?;
            Ok(ResponsePayload::Ok)
        }
        ClientMessage::GetSnapshot { pane_id } => Ok(ResponsePayload::Snapshot(
            registry.get_pane(pane_id).await?.snapshot().await?,
        )),
        ClientMessage::GetScrollback {
            pane_id,
            offset_from_bottom,
            count,
        } => {
            let (total_scrollback, rows) = registry
                .get_pane(pane_id)
                .await?
                .scrollback(offset_from_bottom, count)
                .await?;
            Ok(ResponsePayload::ScrollbackRows {
                pane_id,
                total_scrollback,
                rows,
            })
        }
        ClientMessage::SearchPane {
            pane_id,
            query,
            case_sensitive,
        } => {
            let (total_scrollback, matches) = registry
                .get_pane(pane_id)
                .await?
                .search(query, case_sensitive)
                .await?;
            Ok(ResponsePayload::SearchMatches {
                pane_id,
                total_scrollback,
                matches,
            })
        }
        ClientMessage::GetFailureTrace { pane_id } => Ok(ResponsePayload::FailureTrace(
            registry.get_failure_trace(pane_id).await?,
        )),
        ClientMessage::Subscribe { pane_ids } => {
            let mut f = filter.write().await;
            f.active = true;
            f.pane_ids = pane_ids.map(|ids| ids.into_iter().collect());
            Ok(ResponsePayload::Ok)
        }
        ClientMessage::Unsubscribe => {
            let mut f = filter.write().await;
            f.active = false;
            f.pane_ids = None;
            Ok(ResponsePayload::Ok)
        }
        ClientMessage::RunWorkspaceAction {
            workspace_id,
            action_id,
        } => {
            registry
                .run_workspace_action(workspace_id, &action_id)
                .await?;
            Ok(ResponsePayload::Ok)
        }
    }
}

enum DrainResult {
    Drained,
    StillFull,
    Closed,
}

fn drain_pending_resync(
    pending_resync: &mut HashSet<PaneId>,
    out_tx: &mpsc::Sender<ServerMessage>,
) -> DrainResult {
    let pane_ids: Vec<PaneId> = pending_resync.drain().collect();
    for (idx, pane_id) in pane_ids.iter().enumerate() {
        let msg = ServerMessage::Event(ServerEvent::ResyncRequired { pane_id: *pane_id });
        match out_tx.try_send(msg) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                pending_resync.extend(pane_ids[idx..].iter().copied());
                return DrainResult::StillFull;
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                return DrainResult::Closed;
            }
        }
    }
    DrainResult::Drained
}

async fn forward_subscriber_events(
    mut event_rx: broadcast::Receiver<ServerEvent>,
    sub_out_tx: mpsc::Sender<ServerMessage>,
    sub_filter: Arc<RwLock<SubscriptionFilter>>,
) {
    let mut pending_resync: HashSet<PaneId> = HashSet::new();

    loop {
        let recv_res = if pending_resync.is_empty() {
            event_rx.recv().await
        } else {
            tokio::select! {
                permit = sub_out_tx.reserve() => {
                    match permit {
                        Ok(permit) => {
                            if let Some(&pane_id) = pending_resync.iter().next() {
                                pending_resync.remove(&pane_id);
                                permit.send(ServerMessage::Event(ServerEvent::ResyncRequired {
                                    pane_id,
                                }));
                            }
                            if let DrainResult::Closed =
                                drain_pending_resync(&mut pending_resync, &sub_out_tx)
                            {
                                break;
                            }
                            continue;
                        }
                        Err(_) => break,
                    }
                }
                res = event_rx.recv() => res,
            }
        };

        match recv_res {
            Ok(evt) => {
                if !sub_filter.read().await.matches(&evt) {
                    continue;
                }
                match drain_pending_resync(&mut pending_resync, &sub_out_tx) {
                    DrainResult::Drained => {}
                    DrainResult::StillFull => {
                        if let ServerEvent::GridDelta(delta) = &evt {
                            pending_resync.insert(delta.pane_id);
                            continue;
                        }
                    }
                    DrainResult::Closed => break,
                }
                match &evt {
                    ServerEvent::GridDelta(delta) => {
                        let pane_id = delta.pane_id;
                        match sub_out_tx.try_send(ServerMessage::Event(evt)) {
                            Ok(()) => {}
                            Err(mpsc::error::TrySendError::Full(_)) => {
                                pending_resync.insert(pane_id);
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => break,
                        }
                    }
                    _ => {
                        if sub_out_tx.send(ServerMessage::Event(evt)).await.is_err() {
                            break;
                        }
                        if let DrainResult::Closed =
                            drain_pending_resync(&mut pending_resync, &sub_out_tx)
                        {
                            break;
                        }
                    }
                }
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                warn!(skipped, "event subscriber lagged");
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::supervisor::IsolationMode;
    use daedalus_proto::{CursorState, GridDelta};
    use std::time::Duration;
    #[cfg(unix)]
    use tempfile::TempDir;

    fn dummy_delta(pane_id: PaneId, seq: u64) -> ServerEvent {
        ServerEvent::GridDelta(GridDelta {
            pane_id,
            seq,
            cols: 80,
            rows: 24,
            cursor: CursorState::default(),
            history_generation: 0,
            scrollback_len: None,
            dirty_rows: Vec::new(),
        })
    }

    #[tokio::test]
    async fn resync_required_is_delivered_after_backpressure_drains() {
        let (event_tx, _) = broadcast::channel(16);
        let event_rx = event_tx.subscribe();
        let (out_tx, mut out_rx) = mpsc::channel(2);
        let filter = Arc::new(RwLock::new(SubscriptionFilter {
            active: true,
            pane_ids: None,
        }));

        let handle = tokio::spawn(forward_subscriber_events(event_rx, out_tx, filter));

        let pane_id = PaneId::new();
        for seq in 1..=4 {
            event_tx.send(dummy_delta(pane_id, seq)).unwrap();
        }

        // Yield so forwarder fills the 2-slot mpsc and queues pending_resync
        tokio::time::sleep(Duration::from_millis(20)).await;

        let m1 = out_rx.recv().await.unwrap();
        let m2 = out_rx.recv().await.unwrap();
        assert!(matches!(
            m1,
            ServerMessage::Event(ServerEvent::GridDelta(_))
        ));
        assert!(matches!(
            m2,
            ServerMessage::Event(ServerEvent::GridDelta(_))
        ));

        // Even with no further events on event_tx, ResyncRequired must arrive
        let m3 = tokio::time::timeout(Duration::from_secs(1), out_rx.recv())
            .await
            .expect("timed out waiting for ResyncRequired")
            .unwrap();
        assert_eq!(
            m3,
            ServerMessage::Event(ServerEvent::ResyncRequired { pane_id })
        );

        drop(event_tx);
        tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("forwarder did not exit on broadcast close")
            .unwrap();
    }

    #[tokio::test]
    async fn event_forwarder_exits_when_outbound_receiver_closes() {
        let (event_tx, _) = broadcast::channel(16);
        let event_rx = event_tx.subscribe();
        let (out_tx, out_rx) = mpsc::channel(1);
        let filter = Arc::new(RwLock::new(SubscriptionFilter {
            active: true,
            pane_ids: None,
        }));

        let handle = tokio::spawn(forward_subscriber_events(event_rx, out_tx, filter));
        drop(out_rx);

        event_tx.send(dummy_delta(PaneId::new(), 1)).unwrap();

        tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("forwarder did not exit when outbound channel closed")
            .unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bind_preserves_existing_parent_permissions_and_sets_0700_on_created_parent() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let existing_parent = dir.path().join("existing");
        std::fs::create_dir(&existing_parent).unwrap();
        std::fs::set_permissions(&existing_parent, std::fs::Permissions::from_mode(0o755)).unwrap();

        let runtime = dir.path().join("runtime");
        let sock1 = existing_parent.join("daedalusd.sock");
        let srv1 = DaemonServer::bind_with_supervisor(
            sock1,
            runtime.clone(),
            Supervisor::with_mode(IsolationMode::PgidFallback),
        )
        .await
        .unwrap();

        let mode1 = std::fs::metadata(&existing_parent)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode1, 0o755);
        drop(srv1);

        let created_parent = dir.path().join("newly_created");
        let sock2 = created_parent.join("daedalusd.sock");
        let srv2 = DaemonServer::bind_with_supervisor(
            sock2,
            runtime,
            Supervisor::with_mode(IsolationMode::PgidFallback),
        )
        .await
        .unwrap();

        let mode2 = std::fs::metadata(&created_parent)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode2, 0o700);
        drop(srv2);
    }

    #[test]
    fn constant_time_eq_checks_bytes_and_lengths() {
        assert!(constant_time_eq(b"", b""));
        assert!(constant_time_eq(b"secret-token-123", b"secret-token-123"));
        assert!(!constant_time_eq(b"secret-token-123", b"secret-token-124"));
        assert!(!constant_time_eq(b"secret", b"secret-longer"));
        assert!(!constant_time_eq(b"secret-longer", b"secret"));
        assert!(!constant_time_eq(b"", b"nonempty"));
    }
}
