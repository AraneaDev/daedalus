#![allow(
    non_upper_case_globals,
    non_camel_case_types,
    non_snake_case,
    dead_code
)]

use crate::{BuiltinVt, SemanticEvent, VtConfig, VtEngine};
use daedalus_proto::{CursorState, GridDelta, GridRow, GridSnapshot, PaneId, SearchMatch};

pub mod ffi {
    include!(concat!(env!("OUT_DIR"), "/ghostty_vt_bindings.rs"));
}

/// Feature-gated `libghostty-vt` backend wrapper.
/// Currently generates and links the `libghostty-vt` C ABI bindings via [`ffi`]
/// while delegating `VtEngine` state to `shadow: BuiltinVt` until `libghostty-vt`
/// C ABI calls are wired against an installed 1.0 header.
pub struct GhosttyVt {
    shadow: BuiltinVt,
}

impl GhosttyVt {
    pub fn new(config: VtConfig) -> Self {
        Self {
            shadow: BuiltinVt::new(config),
        }
    }
}

impl VtEngine for GhosttyVt {
    fn feed(&mut self, bytes: &[u8]) -> Vec<SemanticEvent> {
        self.shadow.feed(bytes)
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        self.shadow.resize(cols, rows);
    }

    fn snapshot(&self, pane_id: PaneId, seq: u64) -> GridSnapshot {
        self.shadow.snapshot(pane_id, seq)
    }

    fn scrollback_len(&self) -> usize {
        self.shadow.scrollback_len()
    }

    fn scrollback_window(&self, offset_from_bottom: usize, count: usize) -> Vec<GridRow> {
        self.shadow.scrollback_window(offset_from_bottom, count)
    }

    fn search(&self, query: &str, case_sensitive: bool) -> Vec<SearchMatch> {
        self.shadow.search(query, case_sensitive)
    }

    fn take_dirty_delta(&mut self, pane_id: PaneId, seq: u64) -> Option<GridDelta> {
        self.shadow.take_dirty_delta(pane_id, seq)
    }

    fn slice_plain_text(&self, start_abs_row: u64, end_abs_row: u64) -> String {
        self.shadow.slice_plain_text(start_abs_row, end_abs_row)
    }

    fn row_text_from(&self, abs_row: u64, start_col: u16) -> String {
        self.shadow.row_text_from(abs_row, start_col)
    }

    fn cursor(&self) -> CursorState {
        self.shadow.cursor()
    }

    fn current_abs_row(&self) -> u64 {
        self.shadow.current_abs_row()
    }

    fn backend_name(&self) -> &'static str {
        "ghostty-vt"
    }
}
