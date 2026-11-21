pub mod builtin;

use daedalus_proto::{CursorState, GridDelta, GridSnapshot, PaneId};
use std::path::PathBuf;

pub use builtin::BuiltinVt;
pub use builtin::grid::MAX_TRACE_BYTES;

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
    fn take_dirty_delta(&mut self, pane_id: PaneId, seq: u64) -> Option<GridDelta>;
    fn slice_plain_text(&self, start_abs_row: u64, end_abs_row: u64) -> String;
    fn row_text_from(&self, abs_row: u64, start_col: u16) -> String;
    fn cursor(&self) -> CursorState;
    fn current_abs_row(&self) -> u64;
    fn backend_name(&self) -> &'static str;
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
}
