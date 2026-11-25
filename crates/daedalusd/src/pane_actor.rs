use crate::agent_detector::AgentStateDetector;
use crate::supervisor::SpawnedPty;
use daedalus_proto::{
    AgentState, FailureTrace, GridRow, GridSnapshot, PaneId, PaneRole, PaneSummary, ProtoError,
    SearchMatch, ServerEvent, WorkspaceId, single_line_paste, wrap_bracketed_paste,
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
    Paste {
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
    GetScrollback {
        offset_from_bottom: usize,
        count: usize,
        reply: oneshot::Sender<(usize, Vec<GridRow>)>,
    },
    Search {
        query: String,
        case_sensitive: bool,
        reply: oneshot::Sender<(usize, Vec<SearchMatch>)>,
    },
    GetFailureTrace {
        reply: oneshot::Sender<Option<FailureTrace>>,
    },
    GetSummary {
        reply: oneshot::Sender<PaneSummary>,
    },
    SetAgentState {
        state: AgentState,
        reply: oneshot::Sender<Result<(), ProtoError>>,
    },
    Close {
        reply: oneshot::Sender<()>,
    },
}

enum PtyOutput {
    Bytes(Vec<u8>),
    Closed,
}

fn read_pty_output(reader: &mut impl Read, tx: mpsc::Sender<PtyOutput>) {
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf) {
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Ok(0) | Err(_) => {
                let _ = tx.blocking_send(PtyOutput::Closed);
                return;
            }
            Ok(n) if !tx.is_closed() => {
                let _ = tx.blocking_send(PtyOutput::Bytes(buf[..n].to_vec()));
            }
            // ClosePseudoConsole can wait for pending output to be read. Keep
            // draining the pipe even after the actor has stopped accepting bytes.
            Ok(_) => {}
        }
    }
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
    root_pid: Option<u32>,
    assignment_id: Option<std::sync::Arc<str>>,
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
        Self::spawn_journaled(
            workspace_id,
            pane_id,
            role,
            cols,
            rows,
            spawned,
            event_tx,
            Default::default(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn spawn_journaled(
        workspace_id: WorkspaceId,
        pane_id: PaneId,
        role: PaneRole,
        cols: u16,
        rows: u16,
        spawned: SpawnedPty,
        event_tx: broadcast::Sender<ServerEvent>,
        journal: crate::journal::Journal,
    ) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel(128);
        let (pty_tx, pty_rx) = mpsc::channel(256);
        let (write_tx, mut write_rx) = mpsc::channel::<WriteRequest>(128);

        let SpawnedPty {
            direct,
            launch_preset,
            master,
            mut reader,
            mut writer,
            handle: proc_handle,
        } = spawned;
        let root_pid = proc_handle.root_pid();

        std::thread::Builder::new()
            .name(format!("pty-reader-{}", pane_id.short()))
            .spawn(move || read_pty_output(&mut reader, pty_tx))
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

        let size = PtySize {
            cols: cols.max(1),
            rows: rows.max(1),
            pixel_width: 0,
            pixel_height: 0,
        };
        let output = PaneOutput::new(
            workspace_id,
            pane_id,
            role,
            size.cols,
            size.rows,
            event_tx,
            journal,
        );
        tokio::spawn(run_pane_actor(
            direct,
            output,
            PaneCommands {
                master,
                write_tx,
                launch_preset,
                size,
            },
            writer_handle,
            proc_handle,
            pty_rx,
            cmd_rx,
        ));

        Self {
            id: pane_id,
            workspace_id,
            role,
            root_pid,
            assignment_id: None,
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

    pub(crate) fn with_assignment_id(mut self, assignment_id: Option<String>) -> Self {
        self.assignment_id = assignment_id.map(Into::into);
        self
    }

    pub(crate) fn assignment_id(&self) -> Option<&str> {
        self.assignment_id.as_deref()
    }

    pub fn root_pid(&self) -> Option<u32> {
        self.root_pid
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

    pub async fn paste(&self, data: Vec<u8>) -> Result<(), ProtoError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(PaneCmd::Paste { data, reply: tx })
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

    pub async fn scrollback(
        &self,
        offset_from_bottom: usize,
        count: usize,
    ) -> Result<(usize, Vec<GridRow>), ProtoError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(PaneCmd::GetScrollback {
                offset_from_bottom,
                count,
                reply: tx,
            })
            .await
            .map_err(|_| ProtoError::PaneNotFound(self.id))?;
        rx.await.map_err(|_| ProtoError::PaneNotFound(self.id))
    }

    pub async fn search(
        &self,
        query: String,
        case_sensitive: bool,
    ) -> Result<(usize, Vec<SearchMatch>), ProtoError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(PaneCmd::Search {
                query,
                case_sensitive,
                reply: tx,
            })
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

    pub async fn set_agent_state(&self, state: AgentState) -> Result<(), ProtoError> {
        if self.role != PaneRole::Agent {
            return Err(ProtoError::InvalidRequest(format!(
                "pane {} is not an agent pane",
                self.id
            )));
        }
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(PaneCmd::SetAgentState { state, reply: tx })
            .await
            .map_err(|_| ProtoError::PaneNotFound(self.id))?;
        rx.await.map_err(|_| ProtoError::PaneNotFound(self.id))?
    }

    pub async fn close(&self) {
        let (tx, rx) = oneshot::channel();
        if self.cmd_tx.send(PaneCmd::Close { reply: tx }).await.is_ok() {
            let _ = rx.await;
        }
    }
}

struct PaneOutput {
    last_frame: Option<daedalus_proto::GridDelta>,
    received_output: bool,
    journal: crate::journal::Journal,
    role: PaneRole,
    workspace_id: WorkspaceId,
    pane_id: PaneId,
    tracker: CommandTracker,
    detector: Option<AgentStateDetector>,
    seq: u64,
    dirty: bool,
    events: broadcast::Sender<ServerEvent>,
}

impl PaneOutput {
    fn new(
        workspace_id: WorkspaceId,
        pane_id: PaneId,
        role: PaneRole,
        cols: u16,
        rows: u16,
        events: broadcast::Sender<ServerEvent>,
        journal: crate::journal::Journal,
    ) -> Self {
        let engine = create_vt_engine(VtConfig {
            cols,
            rows,
            scrollback_limit: 10_000,
        });
        Self {
            workspace_id,
            pane_id,
            journal,
            role,
            last_frame: None,
            received_output: false,
            tracker: {
                let mut tracker = CommandTracker::new(pane_id, engine);
                if role == PaneRole::Runner {
                    tracker.enable_runner();
                }
                tracker
            },
            detector: (role == PaneRole::Agent).then(AgentStateDetector::new),
            seq: 0,
            dirty: false,
            events,
        }
    }

    fn emit_agent_state(&self, state: Option<AgentState>) {
        if let Some(state) = state {
            self.journal.record(daedalus_proto::ActivityKind::Agent {
                pane_id: self.pane_id,
                state,
            });
            let _ = self.events.send(ServerEvent::AgentStateChanged {
                workspace_id: self.workspace_id,
                pane_id: self.pane_id,
                state,
            });
        }
    }

    fn feed(&mut self, bytes: &[u8], writer: &mpsc::Sender<WriteRequest>) {
        let outcome = self.tracker.feed(bytes);
        if !outcome.terminal_replies.is_empty() {
            let (reply, _) = oneshot::channel();
            if let Err(error) = writer.try_send(WriteRequest {
                data: outcome.terminal_replies,
                reply,
            }) {
                tracing::warn!(pane_id = %self.pane_id, %error, "could not queue terminal reply");
            }
        }
        self.dirty = true;
        self.received_output |= !bytes.is_empty();
        for (status, cwd) in outcome.status_changed {
            let _ = self.events.send(ServerEvent::PaneStatusChanged {
                pane_id: self.pane_id,
                status,
                cwd,
            });
        }
        for (command, exit_code, has_failure_trace) in outcome.command_finished {
            self.journal.record(daedalus_proto::ActivityKind::Command {
                pane_id: self.pane_id,
                role: self.role,
                exit_code,
            });
            let _ = self.events.send(ServerEvent::CommandFinished {
                pane_id: self.pane_id,
                command,
                exit_code,
                has_failure_trace,
            });
        }
        if let Some(detector) = self.detector.as_mut() {
            let state = detector.observe_output(bytes, Instant::now());
            self.emit_agent_state(state);
        }
    }

    fn flush_delta(&mut self) {
        if self.dirty {
            self.dirty = false;
            let received_output = std::mem::take(&mut self.received_output);
            let insight = self.tracker.runner_insight();
            let activity = self.tracker.activity().cloned();
            let seq = self.seq + 1;
            let grid = self
                .tracker
                .engine_mut()
                .take_dirty_delta(self.pane_id, seq);
            let update = grid.or_else(|| {
                self.last_frame
                    .as_ref()
                    .filter(|old| old.insight != insight || old.activity != activity)
                    .cloned()
            });
            if let Some(mut delta) = update {
                self.seq = seq;
                delta.seq = seq;
                delta.insight = insight;
                delta.activity = activity;
                {
                    let rows = std::mem::take(&mut delta.dirty_rows);
                    self.last_frame = Some(delta.clone());
                    delta.dirty_rows = rows;
                }
                if received_output
                    && self.role == PaneRole::Agent
                    && let Some(row) = delta
                        .dirty_rows
                        .iter()
                        .rev()
                        .find(|row| {
                            row.cells.iter().any(|cell| {
                                cell.width > 0 && cell.text.chars().any(char::is_alphanumeric)
                            })
                        })
                        .or_else(|| delta.dirty_rows.last())
                {
                    self.journal
                        .record(daedalus_proto::ActivityKind::AgentOutput {
                            pane_id: self.pane_id,
                            abs_row: row.abs_row,
                            pane_seq: delta.seq,
                            history_generation: delta.history_generation,
                        });
                }
                let _ = self.events.send(ServerEvent::GridDelta(delta));
            }
        }
    }

    fn finish(&mut self, exit_code: Option<i32>) {
        self.tracker.mark_exited(exit_code);
        self.dirty |= self.role == PaneRole::Runner;
        self.flush_delta();
        if let Some(detector) = self.detector.as_mut() {
            let state = detector.observe_exit(exit_code);
            self.emit_agent_state(state);
        }
        self.journal.record(daedalus_proto::ActivityKind::Exit {
            pane_id: self.pane_id,
            role: self.role,
            exit_code,
        });
        let _ = self.events.send(ServerEvent::PaneExited {
            pane_id: self.pane_id,
            exit_code,
        });
    }
}

// ConPTY does not guarantee EOF on child exit. Allow the reader to deliver trailing
// output, then stop admitting bytes and process everything already in its queue.
const CONPTY_EXIT_DRAIN: Duration = Duration::from_millis(250);

async fn next_pty_output(
    receiver: &mut mpsc::Receiver<PtyOutput>,
    deadline: Option<tokio::time::Instant>,
) -> Option<PtyOutput> {
    if let Some(deadline) = deadline {
        if tokio::time::Instant::now() < deadline
            && let Ok(output) = tokio::time::timeout_at(deadline, receiver.recv()).await
        {
            return output;
        }
        receiver.close();
    }
    receiver.recv().await
}

struct PaneCommands {
    master: Box<dyn portable_pty::MasterPty + Send>,
    write_tx: mpsc::Sender<WriteRequest>,
    launch_preset: Option<String>,
    size: PtySize,
}

impl PaneCommands {
    fn queue_input(&self, data: Vec<u8>, reply: oneshot::Sender<Result<(), ProtoError>>) {
        if let Err(err) = self.write_tx.try_send(WriteRequest { data, reply }) {
            let (msg, request) = match err {
                mpsc::error::TrySendError::Full(req) => ("PTY input queue full", req),
                mpsc::error::TrySendError::Closed(req) => ("PTY writer closed", req),
            };
            let _ = request.reply.send(Err(ProtoError::IoError(msg.into())));
        }
    }

    fn handle(&mut self, output: &mut PaneOutput, command: PaneCmd) -> Option<oneshot::Sender<()>> {
        match command {
            PaneCmd::SendInput { data, reply } => self.queue_input(data, reply),
            PaneCmd::Paste { data, reply } => {
                let data = if output.tracker.engine().terminal_modes().bracketed_paste {
                    wrap_bracketed_paste(&data)
                } else {
                    single_line_paste(&data)
                };
                self.queue_input(data, reply);
            }
            PaneCmd::Resize { cols, rows, reply } => {
                self.size.cols = cols.max(1);
                self.size.rows = rows.max(1);
                let result = self
                    .master
                    .resize(self.size)
                    .map_err(|err| ProtoError::IoError(err.to_string()));
                output
                    .tracker
                    .engine_mut()
                    .resize(self.size.cols, self.size.rows);
                output.dirty = true;
                let _ = reply.send(result);
            }
            PaneCmd::GetSnapshot { reply } => {
                // A snapshot and journal anchor with the same sequence must describe the same frame.
                output.flush_delta();
                let mut snapshot = output.tracker.engine().snapshot(output.pane_id, output.seq);
                snapshot.activity = output.tracker.activity().cloned();
                snapshot.insight = output.tracker.runner_insight();
                let _ = reply.send(snapshot);
            }
            PaneCmd::GetScrollback {
                offset_from_bottom,
                count,
                reply,
            } => {
                let engine = output.tracker.engine();
                let _ = reply.send((
                    engine.scrollback_len(),
                    engine.scrollback_window(offset_from_bottom, count),
                ));
            }
            PaneCmd::Search {
                query,
                case_sensitive,
                reply,
            } => {
                let engine = output.tracker.engine();
                let _ = reply.send((
                    engine.scrollback_len(),
                    engine.search(&query, case_sensitive),
                ));
            }
            PaneCmd::GetFailureTrace { reply } => {
                let _ = reply.send(output.tracker.last_failure_trace().cloned());
            }
            PaneCmd::GetSummary { reply } => {
                let _ = reply.send(PaneSummary {
                    id: output.pane_id,
                    workspace_id: output.workspace_id,
                    role: output.role,
                    status: output.tracker.status(),
                    cols: self.size.cols,
                    rows: self.size.rows,
                    cwd: output.tracker.cwd().cloned(),
                    last_exit_code: output.tracker.last_exit_code(),
                    has_failure_trace: output.tracker.last_failure_trace().is_some(),
                    launch_preset: self.launch_preset.clone(),
                    agent_state: output.detector.as_ref().map(AgentStateDetector::state),
                });
            }
            PaneCmd::SetAgentState { state, reply } => {
                let result = if let Some(detector) = output.detector.as_mut() {
                    let update =
                        detector.set_override(state, Instant::now(), Duration::from_secs(30));
                    output.emit_agent_state(update);
                    Ok(())
                } else {
                    Err(ProtoError::InvalidRequest(format!(
                        "pane {} is not an agent pane",
                        output.pane_id
                    )))
                };
                let _ = reply.send(result);
            }
            PaneCmd::Close { reply } => return Some(reply),
        }
        None
    }
}

async fn finish_pty_output(
    output: &mut PaneOutput,
    process: &mut crate::supervisor::ProcessHandle,
    observed_exit: Option<i32>,
) {
    let mut exit_code = observed_exit.or_else(|| process.try_wait_exit());
    for _ in 0..20 {
        if exit_code.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        exit_code = process.try_wait_exit();
    }
    output.finish(exit_code);
}

async fn run_pane_actor(
    direct: bool,
    mut output: PaneOutput,
    mut commands: PaneCommands,
    writer_handle: std::thread::JoinHandle<()>,
    mut proc_handle: crate::supervisor::ProcessHandle,
    mut pty_rx: mpsc::Receiver<PtyOutput>,
    mut cmd_rx: mpsc::Receiver<PaneCmd>,
) {
    let mut child_exit: Option<(i32, tokio::time::Instant)> = None;
    let role = output.role;
    if direct && role != PaneRole::Agent {
        output.tracker.start_direct_command();
    }
    let mut pty_open = true;
    let mut delta_tick = tokio::time::interval(Duration::from_millis(16));
    delta_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut agent_tick = tokio::time::interval(Duration::from_millis(250));
    agent_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    #[cfg(windows)]
    let mut exit_tick = {
        let mut t = tokio::time::interval(Duration::from_millis(50));
        t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        t
    };

    let close_reply = loop {
        tokio::select! {
            maybe_out = next_pty_output(&mut pty_rx, child_exit.map(|(_, deadline)| deadline)), if pty_open => {
                match maybe_out {
                    Some(PtyOutput::Bytes(bytes)) => output.feed(&bytes, &commands.write_tx),
                    Some(PtyOutput::Closed) | None => {
                        pty_open = false;
                        finish_pty_output(&mut output, &mut proc_handle, child_exit.map(|(code, _)| code)).await;
                    }
                }
            }
            _ = delta_tick.tick(), if output.dirty => {
                output.flush_delta();
            }
            _ = agent_tick.tick(), if output.detector.is_some() || (role==PaneRole::Runner && pty_open) => {
                if role==PaneRole::Runner && output.last_frame.as_ref().is_some_and(|old|old.insight!=output.tracker.runner_insight()) {
                    output.dirty=true;
                }
                if let Some(detector) = output.detector.as_mut() {
                    let state = detector.observe_tick(Instant::now());
                    output.emit_agent_state(state);
                }
            }
            _ = async {
                #[cfg(windows)]
                {
                    exit_tick.tick().await;
                }
                #[cfg(not(windows))]
                {
                    std::future::pending::<()>().await;
                }
            }, if pty_open && child_exit.is_none() => {
                if let Some(code) = proc_handle.try_wait_exit() {
                    child_exit = Some((code, tokio::time::Instant::now() + CONPTY_EXIT_DRAIN));
                }
            }
            maybe_cmd = cmd_rx.recv() => {
                let Some(command) = maybe_cmd else {
                    break None;
                };
                if let Some(reply) = commands.handle(&mut output, command) {
                    break Some(reply);
                }
            }
        }
    };

    pty_rx.close();
    teardown_pane(
        commands.write_tx,
        commands.master,
        writer_handle,
        proc_handle,
    )
    .await;
    if let Some(reply) = close_reply {
        let _ = reply.send(());
    }
}

async fn teardown_pane(
    write_tx: mpsc::Sender<WriteRequest>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    writer_handle: std::thread::JoinHandle<()>,
    mut proc_handle: crate::supervisor::ProcessHandle,
) {
    drop(write_tx);
    let _ = tokio::task::spawn_blocking(move || {
        // Give the writer thread a brief window to observe channel closure and drop the writer fd
        // before process teardown; if it is blocked on a full PTY, teardown kills the child to unblock it.
        let deadline = Instant::now() + Duration::from_millis(50);
        while !writer_handle.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        proc_handle.teardown();
        drop(master);
        let _ = writer_handle.join();
    })
    .await;
}

#[cfg(test)]
mod output_tests {
    use super::*;

    #[test]
    fn closed_receiver_keeps_reader_draining_through_eof() {
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        let mut pipe = std::io::Cursor::new(vec![0; 16_384]);
        read_pty_output(&mut pipe, tx);
        assert_eq!(pipe.position(), 16_384);
    }

    #[tokio::test]
    async fn timed_out_exit_drains_buffered_output_without_waiting_for_eof() {
        let (reader, mut receiver) = mpsc::channel(2);
        reader.send(PtyOutput::Bytes(vec![42])).await.unwrap();
        let elapsed = Some(tokio::time::Instant::now());
        assert!(
            matches!(next_pty_output(&mut receiver, elapsed).await, Some(PtyOutput::Bytes(bytes)) if bytes == [42])
        );
        assert!(next_pty_output(&mut receiver, elapsed).await.is_none());
        assert!(reader.is_closed());
    }

    #[test]
    fn activity_flushes_without_cells_and_coalesces_duplicate_titles() {
        let (events, mut receiver) = broadcast::channel(16);
        let (writer, _) = mpsc::channel(1);
        let mut output = PaneOutput::new(
            WorkspaceId::new(),
            PaneId::new(),
            PaneRole::Service,
            8,
            2,
            events,
            Default::default(),
        );
        output.feed(b"seed", &writer);
        output.flush_delta();
        assert!(matches!(receiver.try_recv(), Ok(ServerEvent::GridDelta(_))));
        output.feed(b"\x1b]0;codex\x07", &writer);
        output.flush_delta();
        let Ok(ServerEvent::GridDelta(delta)) = receiver.try_recv() else {
            panic!("missing title delta")
        };
        assert!(delta.dirty_rows.is_empty());
        assert_eq!(delta.activity.unwrap().title, "codex");
        output.feed(b"\x1b]0;codex\x07", &writer);
        output.flush_delta();
        assert!(receiver.try_recv().is_err());
        output.feed(b"\x07\x07", &writer);
        output.flush_delta();
        let Ok(ServerEvent::GridDelta(delta)) = receiver.try_recv() else {
            panic!("missing attention delta")
        };
        assert_eq!(delta.activity.unwrap().attention_seq, 2);
    }

    #[test]
    fn terminal_mode_changes_publish_a_grid_event_without_cells() {
        let (events, mut receiver) = broadcast::channel(16);
        let (writer, _) = mpsc::channel(1);
        let mut output = PaneOutput::new(
            WorkspaceId::new(),
            PaneId::new(),
            PaneRole::Service,
            8,
            2,
            events,
            Default::default(),
        );
        output.feed(b"seed", &writer);
        output.flush_delta();
        let _ = receiver.try_recv().unwrap();

        output.feed(b"\x1b[?1000;1006h", &writer);
        output.flush_delta();
        let Ok(ServerEvent::GridDelta(delta)) = receiver.try_recv() else {
            panic!("missing mode delta");
        };
        assert_eq!(delta.seq, 2);
        assert!(delta.dirty_rows.is_empty());
        assert_eq!(
            delta.terminal_modes.mouse,
            daedalus_proto::MouseTracking::Button
        );
        assert!(delta.terminal_modes.sgr_mouse);
    }

    #[tokio::test]
    async fn exits_after_queued_and_late_output_and_flushes_once() {
        let pane_id = PaneId::new();
        let (events, mut subscribed) = broadcast::channel(32);
        let (write_tx, mut write_rx) = mpsc::channel::<WriteRequest>(8);
        let journal = crate::journal::Journal::default();
        let workspace_id = WorkspaceId::new();
        let epoch = WorkspaceId::new();
        let start = journal.read(workspace_id, epoch, None).cursor;
        let mut output = PaneOutput::new(
            workspace_id,
            pane_id,
            PaneRole::Runner,
            80,
            24,
            events,
            journal.clone(),
        );
        let (tx, mut rx) = mpsc::channel(2);
        tx.send(PtyOutput::Bytes(b"abc\x1b[6n\x1b]133;C\x07".to_vec()))
            .await
            .unwrap();
        let reader = async {
            let reply = write_rx.recv().await.unwrap();
            assert_eq!(reply.data, b"\x1b[1;4R");
            tx.send(PtyOutput::Bytes(b"late\x1b]133;D;7\x07".to_vec()))
                .await
                .unwrap();
            tx.send(PtyOutput::Closed).await.unwrap();
        };
        let drain = async {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            while let Some(PtyOutput::Bytes(bytes)) = next_pty_output(&mut rx, Some(deadline)).await
            {
                output.feed(&bytes, &write_tx);
            }
            output.finish(Some(7));
        };
        tokio::join!(drain, reader);
        let history = journal.read(workspace_id, epoch, Some(start));
        assert_eq!(
            history
                .entries
                .iter()
                .map(|entry| &entry.kind)
                .collect::<Vec<_>>(),
            [
                &daedalus_proto::ActivityKind::Command {
                    pane_id,
                    role: PaneRole::Runner,
                    exit_code: 7
                },
                &daedalus_proto::ActivityKind::Exit {
                    pane_id,
                    role: PaneRole::Runner,
                    exit_code: Some(7)
                },
            ]
        );
        let mut observed = Vec::new();
        while let Ok(event) = subscribed.try_recv() {
            observed.push(event);
        }
        assert!(
            matches!(
                &observed[..],
                [
                    ServerEvent::PaneStatusChanged { .. },
                    ServerEvent::CommandFinished {
                        exit_code: 7,
                        has_failure_trace: true,
                        ..
                    },
                    ServerEvent::GridDelta(_),
                    ServerEvent::PaneExited {
                        exit_code: Some(7),
                        ..
                    },
                ]
            ),
            "{observed:?}"
        );
        assert!(
            output
                .tracker
                .last_failure_trace()
                .unwrap()
                .output
                .contains("late")
        );
    }
}
