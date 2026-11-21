use crate::supervisor::SpawnedPty;
use daedalus_proto::{
    FailureTrace, GridSnapshot, PaneId, PaneRole, PaneSummary, ProtoError, ServerEvent, WorkspaceId,
};
use daedalus_vt::{CommandTracker, VtConfig, create_vt_engine};
use portable_pty::PtySize;
use std::io::{Read, Write};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, oneshot};

enum PaneCmd {
    SendInput {
        data: Vec<u8>,
        reply: oneshot::Sender<Result<(), ProtoError>>,
    },
    Resize {
        cols: u16,
        rows: u16,
        reply: oneshot::Sender<Result<(), ProtoError>>,
    },
    GetSnapshot {
        reply: oneshot::Sender<GridSnapshot>,
    },
    GetFailureTrace {
        reply: oneshot::Sender<Option<FailureTrace>>,
    },
    GetSummary {
        reply: oneshot::Sender<PaneSummary>,
    },
    Close {
        reply: oneshot::Sender<()>,
    },
}

enum PtyOutput {
    Bytes(Vec<u8>),
    Closed,
}

#[derive(Clone)]
pub struct PaneHandle {
    id: PaneId,
    workspace_id: WorkspaceId,
    role: PaneRole,
    cmd_tx: mpsc::Sender<PaneCmd>,
}

impl PaneHandle {
    pub fn spawn(
        workspace_id: WorkspaceId,
        pane_id: PaneId,
        role: PaneRole,
        cols: u16,
        rows: u16,
        spawned: SpawnedPty,
        event_tx: broadcast::Sender<ServerEvent>,
    ) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel(128);
        let (pty_tx, pty_rx) = mpsc::channel(256);

        let SpawnedPty {
            master,
            mut reader,
            writer,
            handle: proc_handle,
        } = spawned;

        std::thread::Builder::new()
            .name(format!("pty-reader-{}", pane_id.short()))
            .spawn(move || {
                let mut buf = [0u8; 4096];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => {
                            let _ = pty_tx.blocking_send(PtyOutput::Closed);
                            break;
                        }
                        Ok(n) => {
                            if pty_tx
                                .blocking_send(PtyOutput::Bytes(buf[..n].to_vec()))
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                }
            })
            .expect("failed to spawn PTY reader thread");

        tokio::spawn(run_pane_actor(
            workspace_id,
            pane_id,
            role,
            cols,
            rows,
            master,
            writer,
            proc_handle,
            pty_rx,
            cmd_rx,
            event_tx,
        ));

        Self {
            id: pane_id,
            workspace_id,
            role,
            cmd_tx,
        }
    }

    pub fn id(&self) -> PaneId {
        self.id
    }

    pub fn workspace_id(&self) -> WorkspaceId {
        self.workspace_id
    }

    pub fn role(&self) -> PaneRole {
        self.role
    }

    pub async fn summary(&self) -> Result<PaneSummary, ProtoError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(PaneCmd::GetSummary { reply: tx })
            .await
            .map_err(|_| ProtoError::PaneNotFound(self.id))?;
        rx.await.map_err(|_| ProtoError::PaneNotFound(self.id))
    }

    pub async fn send_input(&self, data: Vec<u8>) -> Result<(), ProtoError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(PaneCmd::SendInput { data, reply: tx })
            .await
            .map_err(|_| ProtoError::PaneNotFound(self.id))?;
        rx.await.map_err(|_| ProtoError::PaneNotFound(self.id))?
    }

    pub async fn resize(&self, cols: u16, rows: u16) -> Result<(), ProtoError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(PaneCmd::Resize {
                cols,
                rows,
                reply: tx,
            })
            .await
            .map_err(|_| ProtoError::PaneNotFound(self.id))?;
        rx.await.map_err(|_| ProtoError::PaneNotFound(self.id))?
    }

    pub async fn snapshot(&self) -> Result<GridSnapshot, ProtoError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(PaneCmd::GetSnapshot { reply: tx })
            .await
            .map_err(|_| ProtoError::PaneNotFound(self.id))?;
        rx.await.map_err(|_| ProtoError::PaneNotFound(self.id))
    }

    pub async fn failure_trace(&self) -> Result<Option<FailureTrace>, ProtoError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(PaneCmd::GetFailureTrace { reply: tx })
            .await
            .map_err(|_| ProtoError::PaneNotFound(self.id))?;
        rx.await.map_err(|_| ProtoError::PaneNotFound(self.id))
    }

    pub async fn close(&self) {
        let (tx, rx) = oneshot::channel();
        if self.cmd_tx.send(PaneCmd::Close { reply: tx }).await.is_ok() {
            let _ = rx.await;
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_pane_actor(
    workspace_id: WorkspaceId,
    pane_id: PaneId,
    role: PaneRole,
    mut cols: u16,
    mut rows: u16,
    master: Box<dyn portable_pty::MasterPty + Send>,
    mut writer: Box<dyn Write + Send>,
    mut proc_handle: crate::supervisor::ProcessHandle,
    mut pty_rx: mpsc::Receiver<PtyOutput>,
    mut cmd_rx: mpsc::Receiver<PaneCmd>,
    event_tx: broadcast::Sender<ServerEvent>,
) {
    let engine = create_vt_engine(VtConfig {
        cols,
        rows,
        scrollback_limit: 10_000,
    });
    let mut tracker = CommandTracker::new(pane_id, engine);
    let mut seq: u64 = 0;
    let mut has_pending_delta = false;
    let mut pty_open = true;
    let mut delta_tick = tokio::time::interval(Duration::from_millis(16));

    loop {
        tokio::select! {
            maybe_out = pty_rx.recv(), if pty_open => {
                match maybe_out {
                    Some(PtyOutput::Bytes(bytes)) => {
                        let outcome = tracker.feed(&bytes);
                        has_pending_delta = true;
                        if let Some((status, cwd)) = outcome.status_changed {
                            let _ = event_tx.send(ServerEvent::PaneStatusChanged {
                                pane_id,
                                status,
                                cwd,
                            });
                        }
                        if let Some((command, exit_code, has_failure_trace)) = outcome.command_finished {
                            let _ = event_tx.send(ServerEvent::CommandFinished {
                                pane_id,
                                command,
                                exit_code,
                                has_failure_trace,
                            });
                        }
                    }
                    Some(PtyOutput::Closed) | None => {
                        pty_open = false;
                        if has_pending_delta {
                            has_pending_delta = false;
                            seq += 1;
                            if let Some(delta) = tracker.engine_mut().take_dirty_delta(pane_id, seq) {
                                let _ = event_tx.send(ServerEvent::GridDelta(delta));
                            }
                        }
                        let exit_code = proc_handle.try_wait_exit();
                        tracker.mark_exited(exit_code);
                        let _ = event_tx.send(ServerEvent::PaneExited { pane_id, exit_code });
                    }
                }
            }
            _ = delta_tick.tick() => {
                if has_pending_delta {
                    has_pending_delta = false;
                    seq += 1;
                    if let Some(delta) = tracker.engine_mut().take_dirty_delta(pane_id, seq) {
                        let _ = event_tx.send(ServerEvent::GridDelta(delta));
                    }
                }
            }
            maybe_cmd = cmd_rx.recv() => {
                match maybe_cmd {
                    Some(PaneCmd::SendInput { data, reply }) => {
                        let res = writer
                            .write_all(&data)
                            .and_then(|_| writer.flush())
                            .map_err(|e| ProtoError::IoError(e.to_string()));
                        let _ = reply.send(res);
                    }
                    Some(PaneCmd::Resize { cols: new_cols, rows: new_rows, reply }) => {
                        cols = new_cols.max(1);
                        rows = new_rows.max(1);
                        let res = master
                            .resize(PtySize {
                                rows,
                                cols,
                                pixel_width: 0,
                                pixel_height: 0,
                            })
                            .map_err(|e| ProtoError::IoError(e.to_string()));
                        tracker.engine_mut().resize(cols, rows);
                        has_pending_delta = true;
                        let _ = reply.send(res);
                    }
                    Some(PaneCmd::GetSnapshot { reply }) => {
                        let _ = reply.send(tracker.engine().snapshot(pane_id, seq));
                    }
                    Some(PaneCmd::GetFailureTrace { reply }) => {
                        let _ = reply.send(tracker.last_failure_trace().cloned());
                    }
                    Some(PaneCmd::GetSummary { reply }) => {
                        let summary = PaneSummary {
                            id: pane_id,
                            workspace_id,
                            role,
                            status: tracker.status(),
                            cols,
                            rows,
                            cwd: tracker.cwd().cloned(),
                            last_exit_code: tracker.last_exit_code(),
                            has_failure_trace: tracker.last_failure_trace().is_some(),
                        };
                        let _ = reply.send(summary);
                    }
                    Some(PaneCmd::Close { reply }) => {
                        drop(writer);
                        drop(master);
                        let _ = tokio::task::spawn_blocking(move || {
                            proc_handle.teardown();
                        }).await;
                        let _ = reply.send(());
                        break;
                    }
                    None => {
                        drop(writer);
                        drop(master);
                        let _ = tokio::task::spawn_blocking(move || {
                            proc_handle.teardown();
                        }).await;
                        break;
                    }
                }
            }
        }
    }
}
