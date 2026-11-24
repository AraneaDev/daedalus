use super::{UiEvent, UiEventSink};
use daedalus_proto::transport::IpcStream;
use daedalus_proto::{
    ClientMessage, ProtoError, Request, ResponsePayload, ServerEvent, ServerMessage, read_frame,
    write_frame,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle as TokioJoinHandle;
use tracing::{debug, warn};

#[derive(Debug)]
pub(super) enum RpcError {
    Disconnected,
    Proto(ProtoError),
}

type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<ResponsePayload, ProtoError>>>>>;

pub(super) struct RpcConnection {
    pub(super) inspect_setup_supported: bool,
    write_tx: mpsc::UnboundedSender<Request>,
    pending: PendingMap,
    next_id: AtomicU64,
    disconnect_rx: watch::Receiver<bool>,
    reader_task: TokioJoinHandle<()>,
    writer_task: TokioJoinHandle<()>,
}

impl RpcConnection {
    pub(super) fn new(stream: IpcStream, sink: Arc<dyn UiEventSink>) -> Self {
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
                    Ok(Some(ServerMessage::Event(event))) => match event {
                        ServerEvent::AgentStateChanged {
                            workspace_id,
                            pane_id,
                            state,
                        } => {
                            sink.emit(UiEvent::AgentStateChanged {
                                workspace_id,
                                pane_id,
                                state,
                            });
                        }
                        ServerEvent::WorkspaceRuntimeUpdated {
                            workspace_id,
                            bound_ports,
                            containers,
                            git_status,
                            git_branch,
                        } => {
                            sink.emit(UiEvent::WorkspaceRuntimeUpdated {
                                workspace_id,
                                bound_ports,
                                containers,
                                git_status,
                                git_branch,
                            });
                        }
                        other => {
                            sink.emit(UiEvent::ServerEvent(other));
                        }
                    },
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
            inspect_setup_supported: false,
            write_tx,
            pending,
            next_id: AtomicU64::new(1),
            disconnect_rx,
            reader_task,
            writer_task,
        }
    }

    pub(super) async fn call(&self, payload: ClientMessage) -> Result<ResponsePayload, RpcError> {
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

    pub(super) async fn disconnected(&mut self) {
        if *self.disconnect_rx.borrow() {
            return;
        }
        while self.disconnect_rx.changed().await.is_ok() {
            if *self.disconnect_rx.borrow() {
                return;
            }
        }
    }

    pub(super) async fn close(self) {
        drop(self.write_tx);
        self.reader_task.abort();
        self.writer_task.abort();
        let _ = self.reader_task.await;
        let _ = self.writer_task.await;
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{NoopSink, ipc_pair};
    use super::*;
    use std::time::Duration;
    #[tokio::test]
    async fn rpc_call_fails_immediately_when_disconnected_before_or_during_call() {
        // Case 1: peer closes before `call()` is invoked
        let (client_stream, server_stream) = ipc_pair().await;
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
        conn.close().await;

        // Case 2: peer closes while `call()` is awaiting a response
        let (client_stream, mut server_stream) = ipc_pair().await;
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
        conn.close().await;
    }
}
