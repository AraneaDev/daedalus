use super::AgentPreset;
use crate::selection::Selection;
use daedalus_proto::{CursorState, FailureTrace, GridDelta, GridRow, GridSnapshot, PaneSummary};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_SCROLLBACK_GENERATION: AtomicU64 = AtomicU64::new(1);

/// Client-side cached terminal state for a single daemon pane.
#[derive(Debug, Clone)]
pub struct PaneViewState {
    pub summary: PaneSummary,
    pub(super) grid_received: bool,
    pub cursor: CursorState,
    pub viewport_rows: Vec<GridRow>,
    pub scrollback_len: usize,
    /// Offset of the rows currently displayed.
    pub scroll_offset: usize,
    /// Latest scroll target, including requests awaiting a daemon reply.
    pub requested_scroll_offset: usize,
    pending_scrollback: Option<u64>,
    pub(crate) history_generation: u64,
    pub scrolled_rows: Option<Vec<GridRow>>,
    pub dirty_row_indices: Vec<usize>,
    pub selection: Option<Selection>,
    pub agent_preset: Option<AgentPreset>,
    pub last_failure_trace: Option<FailureTrace>,
    pub show_diagnostic: bool,
    pub activity: Option<daedalus_proto::types::TerminalActivity>,
    pub unread_attention: bool,
    pub insight: Option<daedalus_proto::RunnerInsight>,
    pub last_seq: u64,
}

impl PaneViewState {
    pub fn from_summary(summary: PaneSummary) -> Self {
        let preset = summary
            .launch_preset
            .as_deref()
            .and_then(AgentPreset::from_cli_name);
        Self {
            summary,
            grid_received: false,
            cursor: CursorState::default(),
            viewport_rows: Vec::new(),
            scrollback_len: 0,
            scroll_offset: 0,
            requested_scroll_offset: 0,
            pending_scrollback: None,
            history_generation: 0,
            scrolled_rows: None,
            dirty_row_indices: Vec::new(),
            selection: None,
            agent_preset: preset,
            last_failure_trace: None,
            show_diagnostic: false,
            unread_attention: false,
            activity: None,
            insight: None,
            last_seq: 0,
        }
    }

    pub fn tab_indicator(&self, reduced_motion: bool) -> Option<char> {
        if self.unread_attention {
            return Some('●');
        }
        if matches!(
            self.summary.status,
            daedalus_proto::PaneStatus::Exited { .. }
        ) {
            return None;
        }
        let title = self
            .activity
            .as_ref()
            .map_or("", |activity| activity.title.as_str());
        // Both CLIs replace their spinner title when they return to the prompt.
        if title == "codex" || title.starts_with("✳ ") {
            return None;
        }
        let frame = title.chars().next().filter(|c| "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏◐◑".contains(*c));
        if frame.is_some() || self.summary.agent_state == Some(daedalus_proto::AgentState::Thinking)
        {
            Some(if reduced_motion {
                '•'
            } else {
                frame.unwrap_or('•')
            })
        } else {
            None
        }
    }

    fn update_history_generation(&mut self, generation: u64) -> bool {
        if generation == self.history_generation {
            return false;
        }
        self.history_generation = generation;
        if self.scroll_offset > 0 {
            self.selection = None;
        }
        self.requested_scroll_offset = 0;
        self.scroll_offset = 0;
        self.pending_scrollback = None;
        self.scrolled_rows = None;
        self.mark_all_visible_dirty();
        true
    }

    /// Replaces the cached viewport with a full `GridSnapshot` and marks all rows dirty.
    /// Returns true when saved history was erased since the last update.
    pub fn apply_snapshot(&mut self, snapshot: GridSnapshot) -> bool {
        let erased = self.update_history_generation(snapshot.history_generation);
        self.grid_received = true;
        self.activity = snapshot.activity;
        self.insight = snapshot.insight;
        self.summary.cols = snapshot.cols;
        self.summary.rows = snapshot.rows;
        self.cursor = snapshot.cursor;
        self.last_seq = snapshot.seq;
        self.scrollback_len = snapshot.scrollback_len;
        self.viewport_rows = snapshot.rows_data;
        self.requested_scroll_offset = self.requested_scroll_offset.min(self.scrollback_len);
        self.scroll_offset = 0;
        self.pending_scrollback = None;
        self.scrolled_rows = None;
        self.mark_all_visible_dirty();
        erased
    }

    /// Patches only the rows present in `delta` and records their indices for incremental shaping.
    /// Returns true when saved history was erased since the last update.
    pub fn apply_delta(&mut self, delta: GridDelta) -> bool {
        if (self.last_seq != 0 && delta.seq <= self.last_seq)
            || delta.history_generation < self.history_generation
        {
            return false;
        }
        if delta.insight.as_ref().is_none_or(|i| i.phase != "Failed") {
            self.show_diagnostic = false;
        }
        if delta.activity.as_ref().is_some_and(|next| {
            self.grid_received
                && next.attention_seq > self.activity.as_ref().map_or(0, |old| old.attention_seq)
        }) {
            self.unread_attention = true;
        }
        self.activity = delta.activity;
        self.insight = delta.insight;
        let erased = self.update_history_generation(delta.history_generation);
        if let Some(len) = delta.scrollback_len {
            self.scrollback_len = len;
        }
        let resized = self.summary.cols != delta.cols || self.summary.rows != delta.rows;
        self.grid_received = true;
        self.summary.cols = delta.cols;
        self.summary.rows = delta.rows;
        self.cursor = delta.cursor;
        self.last_seq = delta.seq;

        let target_rows = delta.rows as usize;
        if self.viewport_rows.len() > target_rows {
            self.viewport_rows.truncate(target_rows);
        }

        for row in delta.dirty_rows {
            let idx = row.row_index as usize;
            if idx >= self.viewport_rows.len() {
                for fill_idx in self.viewport_rows.len()..=idx {
                    self.viewport_rows.push(GridRow {
                        links: Vec::new(),
                        wraps: false,
                        row_index: fill_idx as u16,
                        abs_row: 0,
                        cells: Vec::new(),
                    });
                }
            }
            self.viewport_rows[idx] = row;
            if !self.dirty_row_indices.contains(&idx) {
                self.dirty_row_indices.push(idx);
            }
        }

        if resized {
            self.scroll_offset = 0;
            self.pending_scrollback = None;
            self.scrolled_rows = None;
            self.mark_all_visible_dirty();
        }
        erased
    }

    /// Claims a request for the desired history viewport, unless already loaded or pending.
    pub fn begin_scrollback_request(&mut self) -> Option<u64> {
        if self.requested_scroll_offset == self.scroll_offset || self.pending_scrollback.is_some() {
            return None;
        }
        let generation = NEXT_SCROLLBACK_GENERATION.fetch_add(1, Ordering::Relaxed);
        self.pending_scrollback = Some(generation);
        Some(generation)
    }

    pub fn apply_scrollback(
        &mut self,
        generation: u64,
        total_scrollback: usize,
        rows: Vec<GridRow>,
    ) {
        if self.pending_scrollback != Some(generation) {
            return;
        }
        self.pending_scrollback = None;
        self.scrollback_len = total_scrollback;
        self.requested_scroll_offset = self.requested_scroll_offset.min(total_scrollback);
        self.scroll_offset = self.requested_scroll_offset;
        if self.scroll_offset == 0 {
            self.scrolled_rows = None;
        } else {
            self.scrolled_rows = Some(rows);
        }
        self.mark_all_visible_dirty();
    }

    /// Returns the rows currently visible in the pane viewport (scrolled history or live viewport).
    pub fn visible_rows(&self) -> &[GridRow] {
        if self.scroll_offset > 0 {
            self.scrolled_rows.as_deref().unwrap_or_default()
        } else {
            &self.viewport_rows
        }
    }

    pub fn set_scroll_offset(&mut self, offset: usize) {
        if self.requested_scroll_offset != offset {
            self.requested_scroll_offset = offset;
            self.pending_scrollback = None;
            if offset == 0 {
                self.scroll_offset = 0;
                self.scrolled_rows = None;
                self.mark_all_visible_dirty();
            }
        }
    }

    /// Adjusts the requested offset by `delta` (positive = scroll up into history,
    /// negative = scroll down toward live output), clamped to `0..=scrollback_len`.
    pub fn adjust_scroll_offset(&mut self, delta: isize) -> usize {
        let candidate = if delta >= 0 {
            self.requested_scroll_offset.saturating_add(delta as usize)
        } else {
            self.requested_scroll_offset
                .saturating_sub(delta.unsigned_abs())
        };
        let clamped = candidate.min(self.scrollback_len);
        self.set_scroll_offset(clamped);
        self.requested_scroll_offset
    }

    /// Returns the pane to the live bottom viewport (`scroll_offset = 0`).
    pub fn reset_scroll(&mut self) {
        self.set_scroll_offset(0);
    }

    /// Drains and returns dirty row indices that need `glyphon` re-shaping.
    pub fn take_dirty_rows(&mut self) -> Vec<usize> {
        std::mem::take(&mut self.dirty_row_indices)
    }

    pub fn mark_all_visible_dirty(&mut self) {
        let len = self.visible_rows().len();
        self.dirty_row_indices = (0..len).collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_support::{make_pane_summary, make_row, row_text};
    use daedalus_proto::{PaneRole, WorkspaceId};

    #[test]
    fn apply_delta_ignores_stale_or_duplicate_seq_after_snapshot() {
        let ws_id = WorkspaceId::new();
        let summary = make_pane_summary(ws_id, PaneRole::Agent);
        let mut pane = PaneViewState::from_summary(summary);

        // Initial delta when last_seq == 0 should still apply
        pane.apply_delta(GridDelta {
            activity: None,
            insight: None,
            pane_id: pane.summary.id,
            seq: 1,
            cols: 10,
            rows: 2,
            cursor: CursorState::default(),
            history_generation: 0,
            scrollback_len: None,
            dirty_rows: vec![make_row(0, 0, "initial")],
        });
        assert_eq!(pane.last_seq, 1);
        assert_eq!(row_text(&pane.visible_rows()[0]), "initial");

        // Newer snapshot advances last_seq to 5
        pane.apply_snapshot(GridSnapshot {
            activity: None,
            insight: None,
            pane_id: pane.summary.id,
            seq: 5,
            cols: 10,
            rows: 2,
            cursor: CursorState {
                col: 1,
                row: 0,
                visible: true,
            },
            rows_data: vec![make_row(0, 10, "fresh-row"), make_row(1, 11, "row-one")],
            history_generation: 0,
            scrollback_len: 0,
        });
        let _ = pane.take_dirty_rows();

        // Older (seq < last_seq) and duplicate (seq == last_seq) deltas must be ignored
        for stale_seq in [4, 5] {
            pane.apply_delta(GridDelta {
                activity: None,
                insight: None,
                pane_id: pane.summary.id,
                seq: stale_seq,
                cols: 10,
                rows: 2,
                cursor: CursorState {
                    col: 9,
                    row: 1,
                    visible: true,
                },
                history_generation: 0,
                scrollback_len: None,
                dirty_rows: vec![make_row(0, 10, "STALE")],
            });
            assert_eq!(pane.last_seq, 5);
            assert_eq!(row_text(&pane.visible_rows()[0]), "fresh-row");
            assert_eq!(pane.cursor.col, 1);
            assert!(pane.dirty_row_indices.is_empty());
        }

        // Newer delta (seq > last_seq) applies normally
        pane.apply_delta(GridDelta {
            activity: None,
            insight: None,
            pane_id: pane.summary.id,
            seq: 6,
            cols: 10,
            rows: 2,
            cursor: CursorState {
                col: 4,
                row: 0,
                visible: true,
            },
            history_generation: 0,
            scrollback_len: None,
            dirty_rows: vec![make_row(0, 10, "newer-row")],
        });
        assert_eq!(pane.last_seq, 6);
        assert_eq!(row_text(&pane.visible_rows()[0]), "newer-row");
        assert_eq!(pane.cursor.col, 4);
        assert_eq!(pane.dirty_row_indices, vec![0]);
    }

    #[test]
    fn scrollback_offset_clamps_and_switches_visible_rows() {
        let ws_id = WorkspaceId::new();
        let summary = make_pane_summary(ws_id, PaneRole::Runner);
        let mut pane = PaneViewState::from_summary(summary);
        pane.apply_snapshot(GridSnapshot {
            activity: None,
            insight: None,
            pane_id: pane.summary.id,
            seq: 1,
            cols: 20,
            rows: 2,
            cursor: CursorState::default(),
            rows_data: vec![make_row(0, 5, "live-0"), make_row(1, 6, "live-1")],
            history_generation: 0,
            scrollback_len: 5,
        });

        // Clamps above scrollback_len
        assert_eq!(pane.adjust_scroll_offset(100), 5);
        // Clamps below 0
        assert_eq!(pane.adjust_scroll_offset(-100), 0);

        // Scroll up 3 lines and load scrollback rows
        assert_eq!(pane.adjust_scroll_offset(3), 3);
        let generation = pane.begin_scrollback_request().unwrap();
        pane.apply_scrollback(
            generation,
            5,
            vec![make_row(0, 2, "hist-2"), make_row(1, 3, "hist-3")],
        );
        assert_eq!(row_text(&pane.visible_rows()[0]), "hist-2");
        assert_eq!(row_text(&pane.visible_rows()[1]), "hist-3");

        // Reset scroll returns to live viewport
        pane.reset_scroll();
        assert_eq!(pane.scroll_offset, 0);
        assert!(pane.scrolled_rows.is_none());
        assert_eq!(row_text(&pane.visible_rows()[0]), "live-0");
    }

    #[test]
    fn stale_scrollback_cannot_replace_a_newer_offset_or_live_viewport() {
        let summary = make_pane_summary(WorkspaceId::new(), PaneRole::Runner);
        let mut pane = PaneViewState::from_summary(summary);
        pane.set_scroll_offset(5);
        let older = pane.begin_scrollback_request().unwrap();
        pane.set_scroll_offset(10);
        let newer = pane.begin_scrollback_request().unwrap();
        pane.apply_scrollback(older, 3, vec![make_row(0, 1, "stale")]);
        assert_eq!(pane.requested_scroll_offset, 10);
        assert_eq!(pane.scroll_offset, 0);
        assert_eq!(pane.scrollback_len, 0);
        assert!(pane.visible_rows().is_empty());
        assert!(pane.begin_scrollback_request().is_none());
        pane.apply_scrollback(newer, 30, vec![make_row(0, 20, "current")]);
        assert_eq!(row_text(&pane.visible_rows()[0]), "current");

        pane.set_scroll_offset(15);
        let pending = pane.begin_scrollback_request().unwrap();
        assert_eq!(pane.scroll_offset, 10);
        assert_eq!(row_text(&pane.visible_rows()[0]), "current");
        pane.reset_scroll();
        pane.apply_scrollback(pending, 40, vec![make_row(0, 25, "late")]);
        assert_eq!(pane.scroll_offset, 0);
        assert_eq!(pane.scrollback_len, 30);
        assert!(pane.scrolled_rows.is_none());
    }

    #[test]
    fn resized_scrollback_requires_fresh_rows_even_at_the_same_offset() {
        let summary = make_pane_summary(WorkspaceId::new(), PaneRole::Runner);
        let mut pane = PaneViewState::from_summary(summary);
        pane.set_scroll_offset(5);
        let original = pane.begin_scrollback_request().unwrap();
        pane.apply_delta(GridDelta {
            activity: None,
            insight: None,
            pane_id: pane.summary.id,
            seq: 1,
            cols: pane.summary.cols,
            rows: pane.summary.rows + 1,
            history_generation: 0,
            scrollback_len: None,
            dirty_rows: Vec::new(),
            cursor: CursorState::default(),
        });
        let resized = pane.begin_scrollback_request().unwrap();
        pane.apply_scrollback(original, 20, vec![make_row(0, 15, "wrong size")]);
        assert!(pane.scrolled_rows.is_none());
        pane.apply_scrollback(resized, 20, vec![make_row(0, 15, "resized")]);
        assert_eq!(row_text(&pane.visible_rows()[0]), "resized");
    }
}
