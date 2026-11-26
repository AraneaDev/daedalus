pub mod builtin;
#[cfg(feature = "ghostty-vt")]
pub mod ghostty;
mod runner;
pub mod semantic;
mod terminal_modes;

use daedalus_proto::{
    CursorState, GridDelta, GridRow, GridSnapshot, PaneId, SearchMatch, TerminalModes,
};
use std::path::PathBuf;

pub use builtin::BuiltinVt;
pub use builtin::grid::{MAX_SEARCH_MATCHES, MAX_TRACE_BYTES};
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
    Title(String),
    Attention,
    TerminalReply(Vec<u8>),
    CwdChanged(PathBuf),
    PromptStart,
    CommandInputStart,
    CommandExecuted,
    CommandFinished { exit_code: i32 },
}

fn terminal_title(fields: &[&[u8]]) -> String {
    let mut bytes = Vec::with_capacity(256);
    for (i, field) in fields.iter().enumerate() {
        if bytes.len() == 256 {
            break;
        }
        if i != 0 {
            bytes.push(b';');
        }
        bytes.extend_from_slice(&field[..field.len().min(256 - bytes.len())]);
    }
    let mut result = String::new();
    for ch in String::from_utf8_lossy(&bytes)
        .chars()
        .filter(|ch| !ch.is_control())
    {
        if result.len() + ch.len_utf8() > 256 {
            break;
        }
        result.push(ch);
    }
    result
}

pub trait VtEngine: Send {
    fn feed(&mut self, bytes: &[u8]) -> Vec<SemanticEvent>;
    fn resize(&mut self, cols: u16, rows: u16);
    fn snapshot(&self, pane_id: PaneId, seq: u64) -> GridSnapshot;
    fn terminal_modes(&self) -> TerminalModes;
    fn scrollback_len(&self) -> usize;
    fn scrollback_window(&self, offset_from_bottom: usize, count: usize) -> Vec<GridRow>;
    fn search(&self, query: &str, case_sensitive: bool) -> Vec<SearchMatch>;
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
    fn hyperlink_limits_and_resize_do_not_leave_stale_targets() {
        use daedalus_proto::types::LinkSpan;
        let pane = PaneId::new();
        let mut vt = BuiltinVt::new(VtConfig {
            cols: 80,
            rows: 3,
            scrollback_limit: 3,
        });
        for n in 0..30 {
            vt.feed(format!("\x1b]8;;https://host/{n}\x07x").as_bytes());
        }
        let row = &vt.snapshot(pane, 0).rows_data[0];
        assert_eq!(row.links.len(), LinkSpan::MAX_PER_ROW);
        vt.feed(
            format!(
                "\r\n\x1b]8;;https://host/{}\x07x",
                "a".repeat(LinkSpan::MAX_URI_BYTES)
            )
            .as_bytes(),
        );
        assert!(vt.snapshot(pane, 1).rows_data[1].links.is_empty());
        vt.resize(4, 3);
        assert!(
            vt.snapshot(pane, 2).rows_data[0]
                .links
                .iter()
                .all(|s| s.end <= 4)
        );
        vt.feed(b"\x1b[1;1H\x1b[2K");
        assert!(vt.snapshot(pane, 3).rows_data[0].links.is_empty());
    }

    #[test]
    fn hyperlink_wrap_scrollback_and_overwrite() {
        let config = VtConfig {
            cols: 8,
            rows: 2,
            scrollback_limit: 8,
        };
        fn exercise(mut vt: Box<dyn VtEngine>) {
            let pane = PaneId::new();
            vt.feed(b"\x1b]8;id=42;https://host/a;b\x1b\\123456789\x1b]8;;\x07!");
            let before = vt.snapshot(pane, 0);
            assert!(before.rows_data[0].wraps);
            assert_eq!(before.rows_data[0].links[0].url, "https://host/a;b");
            assert_eq!(before.rows_data[1].links[0].end, 1);
            vt.feed(b"\x1b[1;4Hx");
            let edited = vt.take_dirty_delta(pane, 1).unwrap();
            let links = &edited.dirty_rows[0].links;
            assert!(links.iter().any(|span| (span.start, span.end) == (0, 3)));
            assert!(links.iter().any(|span| (span.start, span.end) == (4, 8)));
            vt.feed(b"\x1b[2;1H\r\n");
            let saved = vt.scrollback_window(1, 2);
            assert_eq!(saved[0].links, *links);
            vt.feed(b"\x1b[2J");
            assert!(
                vt.snapshot(pane, 2)
                    .rows_data
                    .iter()
                    .all(|r| r.links.is_empty())
            );
        }
        exercise(Box::new(BuiltinVt::new(config)));
        #[cfg(feature = "ghostty-vt")]
        exercise(Box::new(ghostty::GhosttyVt::new(config)));
    }

    #[test]
    fn erase_line_modes_include_the_cursor_and_preserve_unaffected_cells() {
        for (sequence, expected) in [
            (b"\x1b[0K".as_slice(), "AB"),
            (b"\x1b[1K", "   DE"),
            (b"\x1b[2K", ""),
            (b"\x1b[9K", "ABCDE"),
        ] {
            let mut vt = BuiltinVt::new(VtConfig::default());
            vt.feed(b"ABCDE\x1b[1;3H");
            vt.feed(sequence);
            assert_eq!(vt.row_text_from(0, 0).trim_end(), expected);
        }
    }

    #[test]
    fn erase_uses_the_physical_cursor_while_wrap_is_pending() {
        for erase in [b"\x1b[K", b"\x1b[J"] {
            let mut vt = BuiltinVt::new(VtConfig {
                cols: 4,
                rows: 2,
                ..Default::default()
            });
            vt.feed(b"ABCD");
            vt.feed(erase);
            assert_eq!(vt.row_text_from(0, 0).trim_end(), "ABC");
            assert_eq!(vt.cursor().col, 3);
            vt.feed(b"E");
            assert_eq!(vt.row_text_from(1, 0).trim_end(), "E");

            vt.feed(b"\x1b[1;2H");
            vt.feed(erase);
            assert_eq!(vt.row_text_from(0, 0).trim_end(), "A");
        }
    }

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
    fn parses_osc7_windows_drive_and_unix_uris() {
        assert_eq!(
            builtin::parse_osc7_file_uri("file://WINHOST/C:/Users/tim/project"),
            Some(PathBuf::from("C:/Users/tim/project"))
        );
        assert_eq!(
            builtin::parse_osc7_file_uri("file://WINHOST/C:"),
            Some(PathBuf::from("C:"))
        );
        assert_eq!(
            builtin::parse_osc7_file_uri("file://WINHOST/C%3A/Users/tim/my%20project"),
            Some(PathBuf::from("C:/Users/tim/my project"))
        );
        assert_eq!(
            builtin::parse_osc7_file_uri("file://host/home/tim"),
            Some(PathBuf::from("/home/tim"))
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
    fn alternate_screen_restores_cursor_and_pending_wrap() {
        let mut vt = BuiltinVt::new(VtConfig {
            cols: 5,
            rows: 3,
            scrollback_limit: 10,
        });
        vt.feed(b"abc\x1b[?1049h\x1b[3;2H\x1b[?1049l");
        assert_eq!((vt.cursor().col, vt.cursor().row), (3, 0));
        vt.feed(b"DE\x1b[?1049h\x1b[?1049lX");
        assert_eq!(vt.slice_plain_text(0, 1), "abcDE\nX");
        vt.feed(b"\x1b[3;5H\x1b[?1049h");
        vt.resize(2, 2);
        vt.feed(b"\x1b[?1049l");
        assert_eq!((vt.cursor().col, vt.cursor().row), (1, 1));
    }

    #[test]
    fn wide_cells_and_combining_marks_follow_terminal_columns() {
        let mut vt = BuiltinVt::new(VtConfig {
            cols: 5,
            rows: 3,
            scrollback_limit: 10,
        });
        vt.feed("A界e\u{301}".as_bytes());
        assert_eq!(vt.cursor().col, 4);
        let row = &vt.snapshot(PaneId::new(), 0).rows_data[0];
        assert_eq!(row.cells[1].width, 2);
        assert_eq!(row.cells[2].width, 0);
        assert!(row.cells[2].text.is_empty());
        assert_eq!(row.cells[3].text, "e\u{301}");
        let hit = &vt.search("界", true)[0];
        assert_eq!((hit.start_col, hit.end_col), (1, 3));
        vt.feed("界".as_bytes());
        assert_eq!((vt.cursor().col, vt.cursor().row), (2, 1));
        vt.feed(b"\x1b[1;3HX");
        assert_eq!(vt.slice_plain_text(0, 0), "A Xe\u{301}");
        vt.feed(b"\x1b[2;1H\x1b[1K");
        assert!(vt.slice_plain_text(1, 1).is_empty());
        vt.feed("界".as_bytes());
        vt.resize(1, 3);
        let snapshot = vt.snapshot(PaneId::new(), 0);
        assert_eq!(
            snapshot.rows_data[1].cells[0],
            daedalus_proto::Cell::default()
        );
    }

    #[test]
    fn single_column_panes_keep_wide_and_combining_output_in_bounds() {
        let mut vt = BuiltinVt::new(VtConfig {
            cols: 1,
            rows: 2,
            scrollback_limit: 0,
        });
        vt.feed("界".as_bytes());
        vt.feed("\u{301}".repeat(4096).as_bytes());
        let snapshot = vt.snapshot(PaneId::new(), 0);
        let cell = &snapshot.rows_data[0].cells[0];
        assert_eq!(cell.width, 1);
        assert!(cell.text.starts_with("�\u{301}"));
        assert!(cell.text.len() <= 256);
        vt.feed(b"z");
        assert_eq!(
            vt.snapshot(PaneId::new(), 1).rows_data[1].cells[0].text,
            "z"
        );
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

    #[test]
    fn search_scans_scrollback_and_viewport_with_case_and_unicode() {
        let mut vt = BuiltinVt::new(VtConfig {
            cols: 40,
            rows: 3,
            scrollback_limit: 50,
        });
        vt.feed(b"Alpha hit0\r\nnoise\r\n-> HIT1 and hit2\r\nmore\r\nviewport Hit3");

        assert!(vt.search("", false).is_empty());
        assert!(vt.search("", true).is_empty());

        let ci = vt.search("hit", false);
        assert_eq!(
            ci,
            vec![
                SearchMatch {
                    abs_row: 0,
                    start_col: 6,
                    end_col: 9,
                },
                SearchMatch {
                    abs_row: 2,
                    start_col: 3,
                    end_col: 6,
                },
                SearchMatch {
                    abs_row: 2,
                    start_col: 12,
                    end_col: 15,
                },
                SearchMatch {
                    abs_row: 4,
                    start_col: 9,
                    end_col: 12,
                },
            ]
        );

        let cs = vt.search("HIT", true);
        assert_eq!(
            cs,
            vec![SearchMatch {
                abs_row: 2,
                start_col: 3,
                end_col: 6,
            }]
        );

        // Multi-byte UTF-8 prefix shifts byte offset relative to cell column index.
        let mut utf8_vt = BuiltinVt::new(VtConfig {
            cols: 20,
            rows: 2,
            scrollback_limit: 10,
        });
        utf8_vt.feed("→ Café foo".as_bytes());
        assert_eq!(
            utf8_vt.search("foo", true),
            vec![SearchMatch {
                abs_row: 0,
                start_col: 7,
                end_col: 10,
            }]
        );
        assert_eq!(
            utf8_vt.search("café", false),
            vec![SearchMatch {
                abs_row: 0,
                start_col: 2,
                end_col: 6,
            }]
        );
    }

    #[test]
    fn search_folds_greek_sigma_and_preserves_terminal_columns() {
        let mut vt = BuiltinVt::new(VtConfig {
            cols: 20,
            rows: 2,
            scrollback_limit: 10,
        });
        vt.feed("界 ΟΣ".as_bytes());
        for query in ["ΟΣ", "ος", "οσ"] {
            assert_eq!(
                vt.search(query, false),
                vec![SearchMatch {
                    abs_row: 0,
                    start_col: 3,
                    end_col: 5,
                }],
                "{query}"
            );
        }
        assert!(vt.search("ος", true).is_empty());
    }

    #[test]
    fn search_caps_results_at_max_search_matches() {
        let mut vt = BuiltinVt::new(VtConfig {
            cols: 80,
            rows: 10,
            scrollback_limit: 3_000,
        });
        for i in 0..2_500 {
            vt.feed(format!("match-{i:04}\r\n").as_bytes());
        }
        let matches = vt.search("match-", true);
        assert_eq!(matches.len(), MAX_SEARCH_MATCHES);
        assert_eq!(matches[0].start_col, 0);
        assert_eq!(matches[0].end_col, 6);
    }
}

#[cfg(test)]
mod input_mode_tests {
    use super::*;
    use daedalus_proto::{MouseTracking, TerminalModes};

    #[test]
    fn mode_changes_emit_empty_deltas_and_survive_chunk_boundaries() {
        fn exercise(mut vt: Box<dyn VtEngine>) {
            let pane = PaneId::new();
            vt.take_dirty_delta(pane, 0);
            vt.feed(b"\x1b[?100");
            assert!(vt.take_dirty_delta(pane, 1).is_none());
            vt.feed(b"0;1006;2004h");
            let delta = vt.take_dirty_delta(pane, 1).unwrap();
            assert!(delta.dirty_rows.is_empty());
            assert_eq!(
                delta.terminal_modes,
                TerminalModes {
                    mouse: MouseTracking::Button,
                    sgr_mouse: true,
                    bracketed_paste: true,
                }
            );
            assert_eq!(vt.snapshot(pane, 1).terminal_modes, delta.terminal_modes);
            assert!(vt.take_dirty_delta(pane, 2).is_none());
            vt.feed(b"\x1b[?1000l\x1b[?1002h");
            assert_eq!(
                vt.take_dirty_delta(pane, 2).unwrap().terminal_modes.mouse,
                MouseTracking::Drag
            );
            vt.feed(b"\x1b[?1002;1006;2004l");
            assert_eq!(
                vt.take_dirty_delta(pane, 3).unwrap().terminal_modes,
                TerminalModes::default()
            );
        }
        exercise(Box::new(BuiltinVt::new(VtConfig::default())));
        #[cfg(feature = "ghostty-vt")]
        exercise(Box::new(ghostty::GhosttyVt::new(VtConfig::default())));
    }
}
