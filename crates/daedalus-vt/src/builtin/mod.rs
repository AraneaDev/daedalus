pub mod grid;

use crate::{SemanticEvent, VtConfig, VtEngine};
use daedalus_proto::{
    CellFlags, Color, CursorState, GridDelta, GridRow, GridSnapshot, PaneId, SearchMatch,
};
use grid::Grid;
use std::path::PathBuf;
use vte::{Params, Parser, Perform};

pub struct BuiltinVt {
    parser: Parser,
    state: PerformerState,
}

struct PerformerState {
    grid: Grid,
    pending_events: Vec<SemanticEvent>,
}

impl BuiltinVt {
    pub fn new(config: VtConfig) -> Self {
        Self {
            parser: Parser::new(),
            state: PerformerState {
                grid: Grid::new(config.cols, config.rows, config.scrollback_limit),
                pending_events: Vec::new(),
            },
        }
    }

    pub fn row_text_range(&self, abs_row: u64, start_col: u16, end_col: Option<u16>) -> String {
        self.state.grid.row_text_range(abs_row, start_col, end_col)
    }
}

impl VtEngine for BuiltinVt {
    fn feed(&mut self, bytes: &[u8]) -> Vec<SemanticEvent> {
        for &byte in bytes {
            self.parser.advance(&mut self.state, &[byte]);
        }
        std::mem::take(&mut self.state.pending_events)
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        self.state.grid.resize(cols, rows);
    }

    fn snapshot(&self, pane_id: PaneId, seq: u64) -> GridSnapshot {
        self.state.grid.snapshot(pane_id, seq)
    }

    fn scrollback_len(&self) -> usize {
        self.state.grid.scrollback_len()
    }

    fn scrollback_window(&self, offset_from_bottom: usize, count: usize) -> Vec<GridRow> {
        self.state.grid.scrollback_window(offset_from_bottom, count)
    }

    fn search(&self, query: &str, case_sensitive: bool) -> Vec<SearchMatch> {
        self.state.grid.search(query, case_sensitive)
    }

    fn take_dirty_delta(&mut self, pane_id: PaneId, seq: u64) -> Option<GridDelta> {
        self.state.grid.take_dirty_delta(pane_id, seq)
    }

    fn slice_plain_text(&self, start_abs_row: u64, end_abs_row: u64) -> String {
        self.state.grid.slice_plain_text(start_abs_row, end_abs_row)
    }

    fn row_text_from(&self, abs_row: u64, start_col: u16) -> String {
        self.state.grid.row_text_range(abs_row, start_col, None)
    }

    fn cursor(&self) -> CursorState {
        self.state.grid.cursor()
    }

    fn current_abs_row(&self) -> u64 {
        self.state.grid.current_abs_row()
    }

    fn backend_name(&self) -> &'static str {
        "builtin"
    }
}

impl Perform for PerformerState {
    fn print(&mut self, c: char) {
        self.grid.put_char(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\r' => self.grid.carriage_return(),
            b'\n' | 0x0b | 0x0c => self.grid.linefeed(),
            0x08 => self.grid.backspace(),
            b'\t' => self.grid.tab(),
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if !ignore && intermediates.is_empty() {
            let mut args = params.iter();
            let first = args.next();
            if args.next().is_none() {
                match (action, first) {
                    ('n', Some([6])) => {
                        let cursor = self.grid.cursor();
                        let reply = format!("\x1b[{};{}R", cursor.row + 1, cursor.col + 1);
                        self.pending_events
                            .push(SemanticEvent::TerminalReply(reply.into_bytes()));
                        return;
                    }
                    ('c', None | Some([0])) => {
                        self.pending_events
                            .push(SemanticEvent::TerminalReply(b"\x1b[?1;2c".to_vec()));
                        return;
                    }
                    _ => {}
                }
            }
        }
        let mut flat: Vec<u16> = params.iter().map(|p| p[0]).collect();
        let is_private = intermediates == b"?";

        match (is_private, action) {
            (false, 'm') => self.apply_sgr(&flat),
            (false, 'H' | 'f') => {
                let row = flat.first().copied().unwrap_or(1).max(1) - 1;
                let col = flat.get(1).copied().unwrap_or(1).max(1) - 1;
                self.grid.move_cursor(col, row);
            }
            (false, 'A') => self.grid.move_up(flat.first().copied().unwrap_or(1).max(1)),
            (false, 'B') => self
                .grid
                .move_down(flat.first().copied().unwrap_or(1).max(1)),
            (false, 'C') => self
                .grid
                .move_forward(flat.first().copied().unwrap_or(1).max(1)),
            (false, 'D') => self
                .grid
                .move_backward(flat.first().copied().unwrap_or(1).max(1)),
            (false, 'G') => {
                let col = flat.first().copied().unwrap_or(1).max(1) - 1;
                let row = self.grid.cursor().row;
                self.grid.move_cursor(col, row);
            }
            (false, 'K') => self.grid.erase_in_line(flat.first().copied().unwrap_or(0)),
            (false, 'J') => self
                .grid
                .erase_in_display(flat.first().copied().unwrap_or(0)),
            (false, 'r') => {
                let top = flat.first().copied().unwrap_or(1).max(1) - 1;
                let bottom = flat
                    .get(1)
                    .copied()
                    .filter(|&v| v > 0)
                    .unwrap_or(self.grid.rows())
                    .saturating_sub(1);
                self.grid.set_scroll_region(top, bottom);
            }
            (true, 'h') => {
                for &p in &flat {
                    match p {
                        25 => self.grid.set_cursor_visible(true),
                        1049 => self.grid.enter_alt_screen(),
                        _ => {}
                    }
                }
            }
            (true, 'l') => {
                for &p in &flat {
                    match p {
                        25 => self.grid.set_cursor_visible(false),
                        1049 => self.grid.leave_alt_screen(),
                        _ => {}
                    }
                }
            }
            _ => {
                flat.clear();
            }
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        if params.is_empty() {
            return;
        }
        match params[0] {
            b"7" => {
                if let Some(uri_bytes) = params.get(1)
                    && let Ok(uri) = std::str::from_utf8(uri_bytes)
                    && let Some(path) = parse_osc7_file_uri(uri)
                {
                    self.pending_events.push(SemanticEvent::CwdChanged(path));
                }
            }
            b"133" => {
                if let Some(code) = params.get(1) {
                    match *code {
                        b"A" => self.pending_events.push(SemanticEvent::PromptStart),
                        b"B" => self.pending_events.push(SemanticEvent::CommandInputStart),
                        b"C" => self.pending_events.push(SemanticEvent::CommandExecuted),
                        b"D" => {
                            let exit_code = params
                                .get(2)
                                .and_then(|b| std::str::from_utf8(b).ok())
                                .and_then(|s| s.split(';').next())
                                .and_then(|s| s.parse::<i32>().ok())
                                .unwrap_or(0);
                            self.pending_events
                                .push(SemanticEvent::CommandFinished { exit_code });
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

impl PerformerState {
    fn apply_sgr(&mut self, params: &[u16]) {
        if params.is_empty() {
            self.grid.reset_pen();
            return;
        }
        let mut i = 0;
        while i < params.len() {
            match params[i] {
                0 => self.grid.reset_pen(),
                1 => self.grid.pen_flags_mut().insert(CellFlags::BOLD),
                2 => self.grid.pen_flags_mut().insert(CellFlags::DIM),
                3 => self.grid.pen_flags_mut().insert(CellFlags::ITALIC),
                4 => self.grid.pen_flags_mut().insert(CellFlags::UNDERLINE),
                7 => self.grid.pen_flags_mut().insert(CellFlags::INVERSE),
                9 => self.grid.pen_flags_mut().insert(CellFlags::STRIKETHROUGH),
                22 => self
                    .grid
                    .pen_flags_mut()
                    .remove(CellFlags::BOLD | CellFlags::DIM),
                23 => self.grid.pen_flags_mut().remove(CellFlags::ITALIC),
                24 => self.grid.pen_flags_mut().remove(CellFlags::UNDERLINE),
                27 => self.grid.pen_flags_mut().remove(CellFlags::INVERSE),
                29 => self.grid.pen_flags_mut().remove(CellFlags::STRIKETHROUGH),
                30..=37 => self.grid.set_pen_fg(Color::Indexed((params[i] - 30) as u8)),
                39 => self.grid.set_pen_fg(Color::Default),
                40..=47 => self.grid.set_pen_bg(Color::Indexed((params[i] - 40) as u8)),
                49 => self.grid.set_pen_bg(Color::Default),
                90..=97 => self
                    .grid
                    .set_pen_fg(Color::Indexed((params[i] - 90 + 8) as u8)),
                100..=107 => self
                    .grid
                    .set_pen_bg(Color::Indexed((params[i] - 100 + 8) as u8)),
                38 | 48 => {
                    let is_fg = params[i] == 38;
                    if let Some(&mode) = params.get(i + 1) {
                        if mode == 5 && i + 2 < params.len() {
                            let color = Color::Indexed(params[i + 2] as u8);
                            if is_fg {
                                self.grid.set_pen_fg(color);
                            } else {
                                self.grid.set_pen_bg(color);
                            }
                            i += 2;
                        } else if mode == 2 && i + 4 < params.len() {
                            let color = Color::Rgb(
                                params[i + 2] as u8,
                                params[i + 3] as u8,
                                params[i + 4] as u8,
                            );
                            if is_fg {
                                self.grid.set_pen_fg(color);
                            } else {
                                self.grid.set_pen_bg(color);
                            }
                            i += 4;
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }
}

pub fn parse_osc7_file_uri(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let slash_idx = rest.find('/')?;
    let raw_path = &rest[slash_idx..];
    let decoded = percent_decode(raw_path);
    let bytes = decoded.as_bytes();
    let normalized = if bytes.len() >= 3
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b':'
        && (bytes.len() == 3 || bytes[3] == b'/')
    {
        &decoded[1..]
    } else {
        decoded.as_str()
    };
    Some(PathBuf::from(normalized))
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3])
            && let Ok(val) = u8::from_str_radix(hex, 16)
        {
            out.push(val);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod query_tests {
    use super::*;
    use crate::CommandTracker;

    #[test]
    fn replies_use_query_position_across_every_read_boundary() {
        let stream = b"a\x1b[6nbc\x1b[6nZ\x1b[c\x1b[0c";
        for split in 0..=stream.len() {
            let mut tracker =
                CommandTracker::new(PaneId::new(), Box::new(BuiltinVt::new(VtConfig::default())));
            let mut replies = tracker.feed(&stream[..split]).terminal_replies;
            replies.extend(tracker.feed(&stream[split..]).terminal_replies);
            assert_eq!(
                replies, b"\x1b[1;2R\x1b[1;4R\x1b[?1;2c\x1b[?1;2c",
                "split {split}"
            );
        }
    }

    #[test]
    fn plain_output_and_unsupported_queries_do_not_generate_replies() {
        let mut tracker =
            CommandTracker::new(PaneId::new(), Box::new(BuiltinVt::new(VtConfig::default())));
        assert!(
            tracker
                .feed(b"text\x1b[?6n\x1b[>c\x1b[6;7n\x1b[6:0n")
                .terminal_replies
                .is_empty()
        );
    }
}
