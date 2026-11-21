use crate::{SemanticEvent, VtEngine};
use daedalus_proto::{FailureTrace, PaneId, PaneStatus};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Default)]
pub struct TrackedOutcome {
    pub status_changed: Vec<(PaneStatus, Option<PathBuf>)>,
    pub command_finished: Vec<(Option<String>, i32, bool)>,
}

pub struct CommandTracker {
    pane_id: PaneId,
    engine: Box<dyn VtEngine>,
    status: PaneStatus,
    cwd: Option<PathBuf>,
    last_exit_code: Option<i32>,
    last_failure_trace: Option<FailureTrace>,
    prompt_input_start: Option<(u64, u16)>,
    active_command: Option<String>,
    command_start_abs_row: Option<u64>,
}

impl CommandTracker {
    pub fn new(pane_id: PaneId, engine: Box<dyn VtEngine>) -> Self {
        Self {
            pane_id,
            engine,
            status: PaneStatus::Starting,
            cwd: None,
            last_exit_code: None,
            last_failure_trace: None,
            prompt_input_start: None,
            active_command: None,
            command_start_abs_row: None,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> TrackedOutcome {
        let mut outcome = TrackedOutcome::default();
        let mut start = 0;

        // Split right after potential OSC terminators (BEL, ST '\', or 8-bit ST)
        // so cursor() and current_abs_row() reflect the exact stream position
        // where each semantic event was emitted.
        for (i, &byte) in bytes.iter().enumerate() {
            if matches!(byte, 0x07 | 0x5c | 0x9c) {
                let events = self.engine.feed(&bytes[start..=i]);
                self.process_events(events, &mut outcome);
                start = i + 1;
            }
        }

        if start < bytes.len() {
            let events = self.engine.feed(&bytes[start..]);
            self.process_events(events, &mut outcome);
        }

        outcome
    }

    fn process_events(&mut self, events: Vec<SemanticEvent>, outcome: &mut TrackedOutcome) {
        for event in events {
            match event {
                SemanticEvent::CwdChanged(path) => {
                    self.cwd = Some(path);
                    outcome.status_changed.push((self.status, self.cwd.clone()));
                }
                SemanticEvent::PromptStart => {
                    if !matches!(self.status, PaneStatus::Exited { .. }) {
                        self.status = PaneStatus::Idle;
                        outcome.status_changed.push((self.status, self.cwd.clone()));
                    }
                }
                SemanticEvent::CommandInputStart => {
                    let cursor = self.engine.cursor();
                    self.prompt_input_start = Some((self.engine.current_abs_row(), cursor.col));
                }
                SemanticEvent::CommandExecuted => {
                    self.status = PaneStatus::Busy;
                    if let Some((row, col)) = self.prompt_input_start.take() {
                        let cmd = self.engine.row_text_from(row, col);
                        self.active_command = if cmd.is_empty() { None } else { Some(cmd) };
                    }
                    self.command_start_abs_row = Some(self.engine.current_abs_row());
                    outcome.status_changed.push((self.status, self.cwd.clone()));
                }
                SemanticEvent::CommandFinished { exit_code } => {
                    self.last_exit_code = Some(exit_code);
                    let end_row = self.engine.current_abs_row();
                    let command = self.active_command.take();
                    let start_row = self.command_start_abs_row.take().unwrap_or(end_row);
                    let has_failure_trace = if exit_code != 0 {
                        let output = self.engine.slice_plain_text(start_row, end_row);
                        let captured_at_ms = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map(|d| d.as_millis() as u64)
                            .unwrap_or(0);
                        self.last_failure_trace = Some(FailureTrace {
                            pane_id: self.pane_id,
                            command: command.clone(),
                            exit_code,
                            cwd: self.cwd.clone(),
                            output,
                            captured_at_ms,
                        });
                        true
                    } else {
                        false
                    };
                    outcome
                        .command_finished
                        .push((command, exit_code, has_failure_trace));
                }
            }
        }
    }

    pub fn mark_exited(&mut self, code: Option<i32>) {
        self.status = PaneStatus::Exited { code };
    }

    pub fn status(&self) -> PaneStatus {
        self.status
    }

    pub fn cwd(&self) -> Option<&PathBuf> {
        self.cwd.as_ref()
    }

    pub fn last_exit_code(&self) -> Option<i32> {
        self.last_exit_code
    }

    pub fn last_failure_trace(&self) -> Option<&FailureTrace> {
        self.last_failure_trace.as_ref()
    }

    pub fn engine(&self) -> &dyn VtEngine {
        self.engine.as_ref()
    }

    pub fn engine_mut(&mut self) -> &mut dyn VtEngine {
        self.engine.as_mut()
    }
}
