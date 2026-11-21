use crate::supervisor::SpawnedPty;
use daedalus_proto::{
    FailureTrace, GridSnapshot, PaneId, PaneRole, PaneSummary, ProtoError, ServerEvent, WorkspaceId,
};
use daedalus_vt::{CommandTracker, VtConfig, create_vt_engine};
use portable_pty::PtySize;
use std::io::{Read, Write};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc, oneshot};

#[derive(Debug)]
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

struct WriteRequest {
    data: Vec<u8>,
    reply: oneshot::Sender<Result<(), ProtoError>>,
}

#[derive(Debug, Clone)]
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
        let (write_tx, mut write_rx) = mpsc::channel::<WriteRequest>(128);

        let SpawnedPty {
            master,
            mut reader,
            mut writer,
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

        let writer_handle = std::thread::Builder::new()
            .name(format!("pty-writer-{}", pane_id.short()))
            .spawn(move || {
                while let Some(WriteRequest { data, reply }) = write_rx.blocking_recv() {
                    let res = writer
                        .write_all(&data)
                        .and_then(|_| writer.flush())
                        .map_err(|e| ProtoError::IoError(e.to_string()));
                    let _ = reply.send(res);
                }
                drop(writer);
            })
            .expect("failed to spawn PTY writer thread");

        tokio::spawn(run_pane_actor(
            workspace_id,
            pane_id,
            role,
            cols,
            rows,
            master,
            write_tx,
            writer_handle,
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
    cols: u16,
    rows: u16,
    master: Box<dyn portable_pty::MasterPty + Send>,
    write_tx: mpsc::Sender<WriteRequest>,
    writer_handle: std::thread::JoinHandle<()>,
    mut proc_handle: crate::supervisor::ProcessHandle,
    mut pty_rx: mpsc::Receiver<PtyOutput>,
    mut cmd_rx: mpsc::Receiver<PaneCmd>,
    event_tx: broadcast::Sender<ServerEvent>,
) {
    let mut cols = cols.max(1);
    let mut rows = rows.max(1);
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
    delta_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            maybe_out = pty_rx.recv(), if pty_open => {
                match maybe_out {
                    Some(PtyOutput::Bytes(bytes)) => {
                        let outcome = tracker.feed(&bytes);
                        has_pending_delta = true;
                        for (status, cwd) in outcome.status_changed {
                            let _ = event_tx.send(ServerEvent::PaneStatusChanged {
                                pane_id,
                                status,
                                cwd,
                            });
                        }
                        for (command, exit_code, has_failure_trace) in outcome.command_finished {
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
                        let mut exit_code = proc_handle.try_wait_exit();
                        if exit_code.is_none() {
                            for _ in 0..20 {
                                tokio::time::sleep(Duration::from_millis(10)).await;
                                exit_code = proc_handle.try_wait_exit();
                                if exit_code.is_some() {
                                    break;
                                }
                            }
                        }
                        tracker.mark_exited(exit_code);
                        let _ = event_tx.send(ServerEvent::PaneExited { pane_id, exit_code });
                    }
                }
            }
            _ = delta_tick.tick(), if has_pending_delta => {
                has_pending_delta = false;
                seq += 1;
                if let Some(delta) = tracker.engine_mut().take_dirty_delta(pane_id, seq) {
                    let _ = event_tx.send(ServerEvent::GridDelta(delta));
                }
            }
            maybe_cmd = cmd_rx.recv() => {
                match maybe_cmd {
                    Some(PaneCmd::SendInput { data, reply }) => {
                        if let Err(err) = write_tx.try_send(WriteRequest { data, reply }) {
                            let (msg, reply) = match err {
                                mpsc::error::TrySendError::Full(req) => {
                                    ("PTY input queue full".to_string(), req.reply)
                                }
                                mpsc::error::TrySendError::Closed(req) => {
                                    ("PTY writer closed".to_string(), req.reply)
                                }
                            };
                            let _ = reply.send(Err(ProtoError::IoError(msg)));
                        }
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
                        teardown_pane(write_tx, master, writer_handle, proc_handle).await;
                        let _ = reply.send(());
                        break;
                    }
                    None => {
                        teardown_pane(write_tx, master, writer_handle, proc_handle).await;
                        break;
                    }
                }
            }
        }
    }
}

async fn teardown_pane(
    write_tx: mpsc::Sender<WriteRequest>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    writer_handle: std::thread::JoinHandle<()>,
    mut proc_handle: crate::supervisor::ProcessHandle,
) {
    drop(write_tx);
    drop(master);
    let _ = tokio::task::spawn_blocking(move || {
        // Give the writer thread a brief window to observe channel closure and drop the writer fd
        // before process teardown; if it is blocked on a full PTY, teardown kills the child to unblock it.
        let deadline = Instant::now() + Duration::from_millis(50);
        while !writer_handle.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        proc_handle.teardown();
        let _ = writer_handle.join();
    })
    .await;
}
