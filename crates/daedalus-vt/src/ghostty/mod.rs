mod native;

use crate::{
    MAX_TRACE_BYTES, SemanticEvent, VtConfig, VtEngine,
    builtin::grid::{search_rows, truncate_utf8_tail},
};
use daedalus_proto::{CursorState, GridDelta, GridRow, GridSnapshot, PaneId, SearchMatch};
use native::{Info, Terminal};

pub struct GhosttyVt {
    terminal: Terminal,
    modes: crate::terminal_modes::ModeObserver,
    info: Info,
    limit: usize,
    first_primary: u64,
    generation: u64,
    emitted: Option<GridSnapshot>,
    changed: bool,
}

impl GhosttyVt {
    pub fn new(config: VtConfig) -> Self {
        // A single internal history anchor preserves live row IDs even when the
        // public scrollback budget is zero. All reads enforce the configured limit.
        let terminal = Terminal::new(config.cols, config.rows, config.scrollback_limit.max(1));
        let info = terminal.info();
        Self {
            terminal,
            modes: crate::terminal_modes::ModeObserver::new(),
            info,
            limit: config.scrollback_limit,
            first_primary: 0,
            generation: 0,
            emitted: None,
            changed: true,
        }
    }

    fn base(&self) -> u64 {
        if self.info.alternate {
            0
        } else {
            self.first_primary
        }
    }

    fn total(&self) -> usize {
        self.info.history + usize::from(self.info.rows)
    }

    fn first_visible_history(&self) -> usize {
        self.info.history.saturating_sub(self.limit)
    }

    fn row(&self, y: usize, index: usize) -> GridRow {
        let (wraps, links) = self.terminal.row_links(y as u32, self.info.cols);
        GridRow {
            links,
            wraps,
            row_index: index as u16,
            abs_row: self.base() + y as u64,
            cells: (0..self.info.cols)
                .map(|x| self.terminal.cell(x, y as u32))
                .collect(),
        }
    }

    fn row_text(&self, y: usize, start_col: u16) -> String {
        let mut text = String::new();
        for x in start_col..self.info.cols {
            text.push_str(&self.terminal.cell(x, y as u32).text);
        }
        text.truncate(text.trim_end().len());
        text
    }

    fn feed_chunk(&mut self, bytes: &[u8]) {
        // A tracked point moves upward in screen coordinates when Ghostty prunes
        // complete history pages. Preserve absolute row IDs across those evictions.
        let previous = self.info;
        // Use historical text when possible. Editing lines in the live screen
        // must not renumber prior commands or search results.
        let anchor = previous.history.saturating_sub(1) as u32;
        let tracking = !previous.alternate && self.terminal.set_anchor(anchor);
        self.terminal.write(bytes);
        self.info = self.terminal.info();
        let mut cleared = false;
        if tracking {
            match self.terminal.anchor() {
                Some(row) => self.first_primary += u64::from(anchor.saturating_sub(row)),
                None if !self.info.alternate && previous.history > 0 => {
                    // Erasing all saved lines invalidates even the newest history row.
                    self.first_primary += previous.history as u64;
                    cleared = true;
                }
                _ => {}
            }
        }
        if cleared
            || previous.alternate != self.info.alternate
            || previous.history > self.info.history
        {
            self.generation = self.generation.wrapping_add(1);
        }
        self.changed = true;
    }
}

impl VtEngine for GhosttyVt {
    fn feed(&mut self, bytes: &[u8]) -> Vec<SemanticEvent> {
        self.modes.feed(bytes);
        for chunk in bytes.chunks(256) {
            self.feed_chunk(chunk);
        }
        self.terminal.take_events()
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        if self.terminal.resize(cols, rows) {
            self.info = self.terminal.info();
            self.generation = self.generation.wrapping_add(1);
            self.changed = true;
        }
    }

    fn snapshot(&self, pane_id: PaneId, seq: u64) -> GridSnapshot {
        GridSnapshot {
            terminal_modes: self.terminal_modes(),
            activity: None,
            insight: None,
            pane_id,
            seq,
            cols: self.info.cols,
            rows: self.info.rows,
            cursor: self.cursor(),
            rows_data: self.scrollback_window(0, usize::from(self.info.rows)),
            scrollback_len: self.scrollback_len(),
            history_generation: self.generation,
        }
    }

    fn terminal_modes(&self) -> daedalus_proto::TerminalModes {
        let mut modes = self.modes.modes();
        modes.bracketed_paste = self.info.bracketed_paste;
        modes
    }

    fn scrollback_len(&self) -> usize {
        self.info.history.min(self.limit)
    }

    fn scrollback_window(&self, offset_from_bottom: usize, count: usize) -> Vec<GridRow> {
        let end = self.total() - offset_from_bottom.min(self.scrollback_len());
        let start = end.saturating_sub(count).max(self.first_visible_history());
        (start..end)
            .enumerate()
            .map(|(index, y)| self.row(y, index))
            .collect()
    }

    fn search(&self, query: &str, case_sensitive: bool) -> Vec<SearchMatch> {
        let rows = (self.first_visible_history()..self.total()).map(|y| {
            let row = self.row(y, 0);
            (row.abs_row, row.cells)
        });
        search_rows(query, case_sensitive, rows)
    }

    fn take_dirty_delta(&mut self, pane_id: PaneId, seq: u64) -> Option<GridDelta> {
        if !std::mem::take(&mut self.changed) {
            return None;
        }
        let snapshot = self.snapshot(pane_id, seq);
        let old = self.emitted.replace(snapshot);
        let new = self.emitted.as_ref()?;
        let dirty_rows: Vec<_> = new
            .rows_data
            .iter()
            .filter(|row| {
                old.as_ref()
                    .and_then(|s| s.rows_data.get(usize::from(row.row_index)))
                    != Some(row)
            })
            .cloned()
            .collect();
        if dirty_rows.is_empty()
            && old.as_ref().is_some_and(|old| {
                old.terminal_modes == new.terminal_modes
                    && old.cursor == new.cursor
                    && old.history_generation == new.history_generation
                    && old.scrollback_len == new.scrollback_len
                    && old.rows == new.rows
                    && old.cols == new.cols
            })
        {
            return None;
        }
        Some(GridDelta {
            terminal_modes: new.terminal_modes,
            activity: None,
            insight: None,
            pane_id,
            seq,
            cols: new.cols,
            rows: new.rows,
            cursor: new.cursor,
            dirty_rows,
            history_generation: new.history_generation,
            scrollback_len: Some(new.scrollback_len),
        })
    }

    fn slice_plain_text(&self, start_abs_row: u64, end_abs_row: u64) -> String {
        let base = self.base();
        let start = start_abs_row
            .saturating_sub(base)
            .max(self.first_visible_history() as u64);
        let end = end_abs_row
            .saturating_add(1)
            .saturating_sub(base)
            .min(self.total() as u64);
        // Gather from the end to bound allocations on noisy, long-running commands.
        let mut lines = Vec::new();
        let mut len = 0;
        for y in (start..end).rev() {
            let line = self.row_text(y as usize, 0);
            if lines.is_empty() && line.is_empty() {
                continue;
            }
            len += line.len() + 1;
            lines.push(line);
            if len > MAX_TRACE_BYTES {
                break;
            }
        }
        lines.reverse();
        truncate_utf8_tail(lines.join("\n"), MAX_TRACE_BYTES)
    }

    fn row_text_from(&self, abs_row: u64, start_col: u16) -> String {
        if let Some(y) = abs_row.checked_sub(self.base())
            && (self.first_visible_history() as u64..self.total() as u64).contains(&y)
        {
            return self.row_text(y as usize, start_col);
        }
        String::new()
    }

    fn cursor(&self) -> CursorState {
        CursorState {
            col: self.info.x,
            row: self.info.y,
            visible: self.info.visible,
        }
    }
    fn current_abs_row(&self) -> u64 {
        self.base() + self.info.history as u64 + u64::from(self.info.y)
    }
    fn backend_name(&self) -> &'static str {
        "ghostty-vt"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BuiltinVt, CommandTracker};
    use daedalus_proto::{CellFlags, Color};

    #[test]
    fn both_backends_preserve_style_history_split_osc_and_alternate_screen() {
        let config = VtConfig {
            cols: 18,
            rows: 3,
            scrollback_limit: 20,
        };
        let engines: [Box<dyn VtEngine>; 2] = [
            Box::new(BuiltinVt::new(config)),
            Box::new(GhosttyVt::new(config)),
        ];
        for mut vt in engines {
            let pane = PaneId::new();
            vt.feed("one\r\ntwo\r\nthree\r\n\x1b[1;38;2;10;20;30m界e\u{301}\x1b[0m".as_bytes());
            assert_eq!(vt.scrollback_len(), 1);
            let hit = vt.search("界", true)[0];
            assert_eq!((hit.abs_row, hit.start_col, hit.end_col), (3, 0, 2));
            let grid = vt.snapshot(pane, 1);
            let cells = &grid.rows_data[2].cells;
            assert_eq!(cells[0].fg, Color::Rgb(10, 20, 30));
            assert!(cells[0].flags.contains(CellFlags::BOLD));
            assert_eq!(cells[0].width, 2);
            assert_eq!(cells[1].width, 0);
            assert_eq!(cells[2].text, "e\u{301}");
            assert!(vt.take_dirty_delta(pane, 2).is_some());
            assert!(vt.take_dirty_delta(pane, 3).is_none());
            vt.feed(b"\x1b[?1049hALT\x1b[?1049l");
            assert_eq!(vt.snapshot(pane, 4).rows_data, grid.rows_data);
            vt.resize(24, 3);
            assert_eq!(vt.snapshot(pane, 5).cols, 24);
            let mut tracker = CommandTracker::new(pane, vt);
            tracker.feed(b"\r\n\x1b]7;file://localhost/tmp/work\x07\x1b]133;");
            tracker.feed(
                b"A\x07$ \x1b]133;B\x07cargo test\x1b]133;C\x07\r\nassertion failed\r\n\x1b]133;D;",
            );
            let done = tracker.feed(b"101\x07");
            assert_eq!(
                done.command_finished,
                vec![(Some("cargo test".into()), 101, true)]
            );
            let trace = tracker.last_failure_trace().unwrap();
            assert_eq!(trace.cwd, Some("/tmp/work".into()));
            assert!(trace.output.contains("assertion failed"));
        }
    }

    #[test]
    fn history_rows_stay_addressable_after_pruning_and_clear() {
        let mut vt = GhosttyVt::new(VtConfig {
            cols: 30,
            rows: 4,
            scrollback_limit: 12,
        });
        for n in 0..2_000 {
            vt.feed(format!("line {n}\r\n").as_bytes());
        }
        assert_eq!(vt.scrollback_len(), 12);
        assert_eq!(vt.current_abs_row(), 2000);
        assert_eq!(vt.row_text_from(1999, 0), "line 1999");
        let pane = PaneId::new();
        let before = vt.snapshot(pane, 0);
        vt.feed(b"\x1b[3J");
        assert_eq!(vt.scrollback_len(), 0);
        assert!(vt.snapshot(pane, 1).history_generation > before.history_generation);
        assert_eq!(vt.row_text_from(1999, 0), "line 1999");
    }

    #[test]
    fn zero_saved_lines_retains_only_the_live_absolute_rows() {
        let mut vt = GhosttyVt::new(VtConfig {
            cols: 10,
            rows: 2,
            scrollback_limit: 0,
        });
        for n in 0..20 {
            vt.feed(format!("{n}\r\n").as_bytes());
        }
        assert_eq!(vt.scrollback_len(), 0);
        assert_eq!(vt.current_abs_row(), 20);
        assert_eq!(vt.row_text_from(19, 0), "19");
        assert!(vt.search("12", true).is_empty());
    }

    #[test]
    fn native_keeps_background_only_cells() {
        let mut vt = GhosttyVt::new(VtConfig::default());
        vt.feed(b"\x1b[48;2;9;8;7m\x1b[K");
        let snapshot = vt.snapshot(PaneId::new(), 0);
        assert_eq!(snapshot.rows_data[0].cells[1].bg, Color::Rgb(9, 8, 7));
    }

    #[test]
    fn native_reflows_text_and_handles_insert_characters() {
        let mut vt = GhosttyVt::new(VtConfig {
            cols: 8,
            rows: 3,
            scrollback_limit: 10,
        });
        vt.feed(b"abcdefghij\x1b[1;2H\x1b[@");
        assert_eq!(vt.row_text_from(0, 0), "a bcdefg");
        vt.resize(5, 3);
        assert_eq!(vt.search("a bcd", true).len(), 1);
    }
}
