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
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent)?;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
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

    let mut event_rx = registry.subscribe_events();
    let sub_out_tx = out_tx.clone();
    let sub_filter = Arc::clone(&filter);
    let event_task = tokio::spawn(async move {
        loop {
            match event_rx.recv().await {
                Ok(evt) => {
                    if !sub_filter.read().await.matches(&evt) {
                        continue;
                    }
                    match &evt {
                        ServerEvent::GridDelta(delta) => {
                            let pane_id = delta.pane_id;
                            if sub_out_tx.try_send(ServerMessage::Event(evt)).is_err() {
                                let _ = sub_out_tx.try_send(ServerMessage::Event(
                                    ServerEvent::ResyncRequired { pane_id },
                                ));
                            }
                        }
                        _ => {
                            if sub_out_tx.send(ServerMessage::Event(evt)).await.is_err() {
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
    });

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
