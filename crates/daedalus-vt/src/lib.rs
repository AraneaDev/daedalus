pub mod builtin;
#[cfg(feature = "ghostty-vt")]
pub mod ghostty;
pub mod semantic;

use daedalus_proto::{CursorState, GridDelta, GridRow, GridSnapshot, PaneId};
use std::path::PathBuf;

pub use builtin::BuiltinVt;
pub use builtin::grid::MAX_TRACE_BYTES;
pub use semantic::{CommandTracker, TrackedOutcome};

#[derive(Debug, Clone, Copy)]
pub struct VtConfig {
    pub cols: u16,
    pub rows: u16,
    pub scrollback_limit: usize,
}

impl Default for VtConfig {
    fn default() -> Self {
        Self {
            cols: 120,
            rows: 40,
            scrollback_limit: 10_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticEvent {
    CwdChanged(PathBuf),
    PromptStart,
    CommandInputStart,
    CommandExecuted,
    CommandFinished { exit_code: i32 },
}

pub trait VtEngine: Send {
    fn feed(&mut self, bytes: &[u8]) -> Vec<SemanticEvent>;
    fn resize(&mut self, cols: u16, rows: u16);
    fn snapshot(&self, pane_id: PaneId, seq: u64) -> GridSnapshot;
    fn scrollback_len(&self) -> usize;
    fn scrollback_window(&self, offset_from_bottom: usize, count: usize) -> Vec<GridRow>;
    fn take_dirty_delta(&mut self, pane_id: PaneId, seq: u64) -> Option<GridDelta>;
    fn slice_plain_text(&self, start_abs_row: u64, end_abs_row: u64) -> String;
    fn row_text_from(&self, abs_row: u64, start_col: u16) -> String;
    fn cursor(&self) -> CursorState;
    fn current_abs_row(&self) -> u64;
    fn backend_name(&self) -> &'static str;
}

pub fn create_vt_engine(config: VtConfig) -> Box<dyn VtEngine> {
    #[cfg(feature = "ghostty-vt")]
    {
        Box::new(ghostty::GhosttyVt::new(config))
    }
    #[cfg(not(feature = "ghostty-vt"))]
    {
        Box::new(BuiltinVt::new(config))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daedalus_proto::{CellFlags, Color};

    #[test]
    fn renders_sgr_colors_and_flags_and_clears_dirty_delta() {
        let pane_id = PaneId::new();
        let mut vt = BuiltinVt::new(VtConfig {
            cols: 20,
            rows: 5,
            scrollback_limit: 100,
        });
        vt.feed(b"\x1b[1;38;2;255;10;20mHi\x1b[0m");
        let snap = vt.snapshot(pane_id, 1);
        assert_eq!(snap.rows_data[0].cells[0].text, "H");
        assert_eq!(snap.rows_data[0].cells[0].fg, Color::Rgb(255, 10, 20));
        assert!(snap.rows_data[0].cells[0].flags.contains(CellFlags::BOLD));

        let delta = vt
            .take_dirty_delta(pane_id, 1)
            .expect("expected dirty delta");
        assert!(!delta.dirty_rows.is_empty());
        assert!(vt.take_dirty_delta(pane_id, 2).is_none());
    }

    #[test]
    fn handles_scrollback_and_carriage_return_overwrites() {
        let mut vt = BuiltinVt::new(VtConfig {
            cols: 20,
            rows: 3,
            scrollback_limit: 10,
        });
        vt.feed(b"line0\r\nline1\r\nloading...\rDONE!     \r\nline3\r\nline4");
        let sliced = vt.slice_plain_text(0, vt.current_abs_row());
        assert_eq!(sliced, "line0\nline1\nDONE!\nline3\nline4");
    }

    #[test]
    fn parses_osc7_percent_encoded_and_osc133_events() {
        let mut vt = BuiltinVt::new(VtConfig::default());
        let events = vt.feed(
            b"\x1b]7;file://host/home/tim/my%20repo\x07\x1b]133;A\x07$ \x1b]133;B\x07ls\x1b]133;C\x07\r\nout\r\n\x1b]133;D;42\x07",
        );
        assert_eq!(
            events,
            vec![
                SemanticEvent::CwdChanged(PathBuf::from("/home/tim/my repo")),
                SemanticEvent::PromptStart,
                SemanticEvent::CommandInputStart,
                SemanticEvent::CommandExecuted,
                SemanticEvent::CommandFinished { exit_code: 42 },
            ]
        );
    }

    #[test]
    fn truncates_plain_text_slice_to_64kib_tail() {
        let mut vt = BuiltinVt::new(VtConfig {
            cols: 100,
            rows: 10,
            scrollback_limit: 2_000,
        });
        for i in 0..900 {
            let line = format!("row-{i:04}-{}", "x".repeat(80));
            vt.feed(format!("{line}\r\n").as_bytes());
        }
        let sliced = vt.slice_plain_text(0, vt.current_abs_row());
        assert!(sliced.len() <= MAX_TRACE_BYTES);
        assert!(sliced.contains("row-0899-"));
    }

    #[test]
    fn resizes_safely_during_alt_screen_and_conforms_restored_main() {
        let pane_id = PaneId::new();
        let mut vt = BuiltinVt::new(VtConfig {
            cols: 20,
            rows: 5,
            scrollback_limit: 50,
        });
        vt.feed(b"main0\r\nmain1\r\nmain2");

        // Enter alt screen and resize larger
        vt.feed(b"\x1b[?1049halt");
        vt.resize(30, 8);
        vt.feed(b"\x1b[?1049l");

        let delta = vt
            .take_dirty_delta(pane_id, 1)
            .expect("expected dirty delta after leaving alt screen");
        assert_eq!(delta.cols, 30);
        assert_eq!(delta.rows, 8);
        assert_eq!(delta.dirty_rows.len(), 8);
        assert!(delta.dirty_rows.iter().all(|r| r.cells.len() == 30));

        let snap = vt.snapshot(pane_id, 2);
        assert_eq!(snap.cols, 30);
        assert_eq!(snap.rows, 8);
        assert_eq!(snap.rows_data.len(), 8);
        assert!(snap.rows_data.iter().all(|r| r.cells.len() == 30));
        assert_eq!(vt.row_text_from(snap.rows_data[0].abs_row, 0), "main0");

        // Enter alt screen again and resize smaller
        vt.feed(b"\x1b[?1049h");
        vt.resize(10, 3);
        vt.feed(b"\x1b[?1049l");

        let delta = vt
            .take_dirty_delta(pane_id, 3)
            .expect("expected dirty delta after shrinking in alt screen");
        assert_eq!(delta.cols, 10);
        assert_eq!(delta.rows, 3);
        assert_eq!(delta.dirty_rows.len(), 3);
        assert!(delta.dirty_rows.iter().all(|r| r.cells.len() == 10));

        let snap = vt.snapshot(pane_id, 4);
        assert_eq!(snap.cols, 10);
        assert_eq!(snap.rows, 3);
        assert_eq!(snap.rows_data.len(), 3);
        assert!(snap.rows_data.iter().all(|r| r.cells.len() == 10));
    }

    #[test]
    fn command_tracker_captures_failure_trace_on_nonzero_exit_only() {
        let pane_id = PaneId::new();
        let mut tracker = CommandTracker::new(pane_id, create_vt_engine(VtConfig::default()));

        let ok = tracker.feed(
            b"\x1b]7;file://localhost/tmp/work\x07\x1b]133;A\x07$ \x1b]133;B\x07echo ok\x1b]133;C\x07\r\nok\r\n\x1b]133;D;0\x07",
        );
        assert_eq!(
            ok.command_finished,
            vec![(Some("echo ok".to_string()), 0, false)]
        );
        assert!(tracker.last_failure_trace().is_none());

        let fail = tracker.feed(
            b"\x1b]133;A\x07$ \x1b]133;B\x07cargo test\x1b]133;C\x07\r\n\x1b[31massertion failed\x1b[0m\r\n\x1b]133;D;101\x07",
        );
        assert_eq!(
            fail.command_finished,
            vec![(Some("cargo test".to_string()), 101, true)]
        );
        let trace = tracker
            .last_failure_trace()
            .expect("expected failure trace");
        assert_eq!(trace.exit_code, 101);
        assert_eq!(trace.command.as_deref(), Some("cargo test"));
        assert_eq!(trace.cwd, Some(PathBuf::from("/tmp/work")));
        assert!(trace.output.contains("assertion failed"));
        assert!(!trace.output.contains("\x1b[31m"));
    }

    #[test]
    fn command_tracker_preserves_multiple_events_in_single_feed() {
        use daedalus_proto::PaneStatus;

        let pane_id = PaneId::new();
        let mut tracker = CommandTracker::new(pane_id, create_vt_engine(VtConfig::default()));

        let chunk = concat!(
            "\x1b]133;A\x07$ \x1b]133;B\x07true\x1b]133;C\x07\r\n\x1b]133;D;0\x07",
            "\x1b]133;A\x07$ \x1b]133;B\x07false\x1b]133;C\x07\r\nboom\r\n\x1b]133;D;1\x07",
            "\x1b]133;A\x07$ \x1b]133;B\x07",
        );
        let outcome = tracker.feed(chunk.as_bytes());

        assert_eq!(
            outcome.status_changed,
            vec![
                (PaneStatus::Idle, None),
                (PaneStatus::Busy, None),
                (PaneStatus::Idle, None),
                (PaneStatus::Busy, None),
                (PaneStatus::Idle, None),
            ]
        );
        assert_eq!(
            outcome.command_finished,
            vec![
                (Some("true".to_string()), 0, false),
                (Some("false".to_string()), 1, true),
            ]
        );
        assert_eq!(tracker.status(), PaneStatus::Idle);
        let trace = tracker
            .last_failure_trace()
            .expect("expected failure trace");
        assert_eq!(trace.command.as_deref(), Some("false"));
        assert_eq!(trace.exit_code, 1);
        assert!(trace.output.contains("boom"));
    }

    #[test]
    fn scrollback_window_returns_clamped_historical_rows() {
        let mut vt = BuiltinVt::new(VtConfig {
            cols: 20,
            rows: 3,
            scrollback_limit: 50,
        });
        vt.feed(b"line0\r\nline1\r\nline2\r\nline3\r\nline4\r\nline5\r\nline6\r\nline7");

        assert_eq!(vt.scrollback_len(), 5);
        assert_eq!(vt.snapshot(PaneId::new(), 1).scrollback_len, 5);

        let row_texts = |rows: Vec<GridRow>| -> Vec<String> {
            rows.into_iter()
                .map(|r| {
                    r.cells
                        .into_iter()
                        .map(|c| c.text)
                        .collect::<String>()
                        .trim_end()
                        .to_string()
                })
                .collect()
        };

        assert_eq!(
            row_texts(vt.scrollback_window(0, 3)),
            vec!["line5", "line6", "line7"]
        );
        assert_eq!(
            row_texts(vt.scrollback_window(2, 3)),
            vec!["line3", "line4", "line5"]
        );
        assert_eq!(
            row_texts(vt.scrollback_window(999, 3)),
            vec!["line0", "line1", "line2"]
        );
    }
}
