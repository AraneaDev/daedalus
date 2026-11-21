use crate::registry::Registry;
use crate::shell_integration::ShellIntegration;
use crate::supervisor::Supervisor;
use daedalus_proto::{
    ClientMessage, PaneId, ProtoError, Request, ResponsePayload, ServerEvent, ServerMessage,
    read_frame, write_frame,
};
use daedalus_vt::{VtConfig, create_vt_engine};
use std::collections::HashSet;
use std::future::Future;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{RwLock, broadcast, mpsc};
use tracing::{debug, error, info, warn};

const CLIENT_OUTBOUND_CAPACITY: usize = 256;

pub struct DaemonServer {
    socket_path: PathBuf,
    listener: UnixListener,
    registry: Arc<Registry>,
}

impl DaemonServer {
    pub async fn bind(socket_path: PathBuf, runtime_dir: PathBuf) -> std::io::Result<Self> {
        let supervisor = Supervisor::detect();
        Self::bind_with_supervisor(socket_path, runtime_dir, supervisor).await
    }

    pub async fn bind_with_supervisor(
        socket_path: PathBuf,
        runtime_dir: PathBuf,
        supervisor: Supervisor,
    ) -> std::io::Result<Self> {
        if let Some(parent) = socket_path.parent().filter(|p| !p.as_os_str().is_empty()) {
            let created = !parent.exists();
            std::fs::create_dir_all(parent)?;
            if created {
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
            }
        }
        std::fs::create_dir_all(&runtime_dir)?;

        if socket_path.exists() {
            match UnixStream::connect(&socket_path).await {
                Ok(_) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AddrInUse,
                        format!("daedalusd is already running on {}", socket_path.display()),
                    ));
                }
                Err(_) => {
                    let _ = std::fs::remove_file(&socket_path);
                }
            }
        }

        let shell_integration = ShellIntegration::prepare(&runtime_dir)?;
        let (event_tx, _) = broadcast::channel(1024);
        let registry = Arc::new(Registry::new(supervisor, shell_integration, event_tx));
        let listener = UnixListener::bind(&socket_path)?;
        info!(socket = %socket_path.display(), "daedalusd listening");

        Ok(Self {
            socket_path,
            listener,
            registry,
        })
    }

    pub async fn serve(self, shutdown: impl Future<Output = ()>) -> std::io::Result<()> {
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                accept_res = self.listener.accept() => {
                    match accept_res {
                        Ok((stream, _)) => {
                            let reg = Arc::clone(&self.registry);
                            tokio::spawn(handle_client(stream, reg));
                        }
                        Err(e) => {
                            error!(error = %e, "accept failed");
                        }
                    }
                }
                _ = &mut shutdown => {
                    info!("shutting down daedalusd");
                    break;
                }
            }
        }

        self.registry.close_all().await;
        let _ = std::fs::remove_file(&self.socket_path);
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
        let pid = match event {
            ServerEvent::GridDelta(d) => d.pane_id,
            ServerEvent::PaneStatusChanged { pane_id, .. }
            | ServerEvent::CommandFinished { pane_id, .. }
            | ServerEvent::PaneExited { pane_id, .. }
            | ServerEvent::ResyncRequired { pane_id } => *pane_id,
        };
        allowed.contains(&pid)
    }
}

async fn handle_client(stream: UnixStream, registry: Arc<Registry>) {
    let (mut reader, mut writer) = stream.into_split();
    let (out_tx, mut out_rx) = mpsc::channel::<ServerMessage>(CLIENT_OUTBOUND_CAPACITY);
    let filter = Arc::new(RwLock::new(SubscriptionFilter::default()));

    let writer_task = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            if let Err(e) = write_frame(&mut writer, &msg).await {
                debug!(error = %e, "client write failed");
                break;
            }
        }
    });

    let event_rx = registry.subscribe_events();
    let sub_out_tx = out_tx.clone();
    let sub_filter = Arc::clone(&filter);
    let event_task = tokio::spawn(forward_subscriber_events(event_rx, sub_out_tx, sub_filter));

    loop {
        match read_frame::<_, Request>(&mut reader).await {
            Ok(Some(req)) => {
                let result = dispatch_request(&registry, &filter, req.payload).await;
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

    event_task.abort();
    drop(out_tx);
    let _ = writer_task.await;
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
        ClientMessage::CreateWorkspace {
            name,
            root_dir,
            env,
        } => Ok(ResponsePayload::WorkspaceCreated(
            registry.create_workspace(name, root_dir, env).await?,
        )),
        ClientMessage::ListWorkspaces => Ok(ResponsePayload::Workspaces(
            registry.list_workspaces().await,
        )),
        ClientMessage::CloseWorkspace { id } => {
            registry.close_workspace(id).await?;
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
        ClientMessage::GetFailureTrace { pane_id } => Ok(ResponsePayload::FailureTrace(
            registry.get_pane(pane_id).await?.failure_trace().await?,
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
    use crate::supervisor::IsolationMode;
    use daedalus_proto::{CursorState, GridDelta};
    use std::time::Duration;
    use tempfile::TempDir;

    fn dummy_delta(pane_id: PaneId, seq: u64) -> ServerEvent {
        ServerEvent::GridDelta(GridDelta {
            pane_id,
            seq,
            cols: 80,
            rows: 24,
            cursor: CursorState::default(),
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

    #[tokio::test]
    async fn bind_preserves_existing_parent_permissions_and_sets_0700_on_created_parent() {
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
}
