use super::{DaedalusApp, PointerState, ViewportMetrics};
use daedalus_ui::selection::{CellPos, Selection, select_word_at};
use daedalus_ui::state::PaneViewState;
use std::time::{Duration, Instant};

const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(400);

impl PointerState {
    pub(super) fn select_at(&mut self, pane: &mut PaneViewState, col: u16, row: u16, now: Instant) {
        let role = pane.summary.role;
        let pane_id = pane.summary.id;
        let grid_row = pane.visible_rows().get(row as usize);
        if self
            .last_click
            .is_some_and(|(ts, prev_pane, prev_col, prev_row)| {
                now.duration_since(ts) <= DOUBLE_CLICK_WINDOW
                    && (prev_pane, prev_col, prev_row) == (pane_id, col, row)
            })
        {
            self.cancel_gestures();
            pane.selection = grid_row.and_then(|grid_row| select_word_at(grid_row, col));
        } else {
            self.last_click = Some((now, pane_id, col, row));
            self.drag_anchor =
                grid_row.map(|grid_row| (role, pane_id, CellPos::new(col, grid_row.abs_row)));
            pane.selection = None;
        }
    }
}

impl DaedalusApp {
    pub(crate) fn drag_selection(&mut self, metrics: ViewportMetrics) {
        let Some((drag_role, drag_pane_id, anchor)) = self.pointer.drag_anchor else {
            return;
        };
        let (x, y) = self.pointer.position;
        let (cell_w, cell_h) = metrics.cell;
        let layout = self.layout_for(metrics);
        let Some(pane_rect) = layout.pane(drag_role) else {
            return;
        };

        let col = if cell_w > 0.0 {
            (((x - pane_rect.viewport.x) / cell_w).floor().max(0.0) as u16)
                .min(pane_rect.cols.saturating_sub(1))
        } else {
            0
        };
        let row = if cell_h > 0.0 {
            (((y - pane_rect.viewport.y) / cell_h).floor().max(0.0) as u16)
                .min(pane_rect.rows.saturating_sub(1))
        } else {
            0
        };

        if let Some(pane) = self.state.panes.get_mut(&drag_pane_id)
            && extend_selection(pane, anchor, col, row)
        {
            self.request_redraw();
        }
    }
}

fn extend_selection(pane: &mut PaneViewState, anchor: CellPos, col: u16, row: u16) -> bool {
    let Some(grid_row) = pane.visible_rows().get(row as usize) else {
        return false;
    };
    let head = CellPos::new(col, grid_row.abs_row);
    if head == anchor && pane.selection.is_none() {
        return false;
    }
    pane.selection = Some(Selection::new(anchor, head));
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use daedalus_proto::{PaneId, PaneRole};

    #[test]
    fn a_second_click_selects_a_word_and_releases_the_drag_anchor() {
        use daedalus_proto::{Cell, GridRow, PaneStatus, PaneSummary, WorkspaceId};
        let id = PaneId::new();
        let role = PaneRole::Runner;
        let mut pane = PaneViewState::from_summary(PaneSummary {
            id,
            role,
            workspace_id: WorkspaceId::new(),
            status: PaneStatus::Idle,
            cols: 5,
            rows: 1,
            cwd: None,
            last_exit_code: None,
            has_failure_trace: false,
            launch_preset: None,
            agent_state: None,
        });
        pane.viewport_rows.push(GridRow {
            links: Vec::new(),
            wraps: false,
            row_index: 0,
            abs_row: 42,
            cells: "a cat"
                .chars()
                .map(|c| Cell {
                    text: c.to_string(),
                    ..Cell::default()
                })
                .collect(),
        });
        let mut pointer = PointerState::default();
        let first = Instant::now();
        pointer.select_at(&mut pane, 3, 0, first);
        assert_eq!(pointer.drag_anchor, Some((role, id, CellPos::new(3, 42))));
        let (_, _, anchor) = pointer.drag_anchor.unwrap();
        assert!(!extend_selection(&mut pane, anchor, 3, 0));
        assert!(extend_selection(&mut pane, anchor, 4, 0));
        assert_eq!(
            pane.selection,
            Some(Selection::new(anchor, CellPos::new(4, 42)))
        );
        pointer.end_drag();
        pointer.select_at(&mut pane, 3, 0, first + DOUBLE_CLICK_WINDOW);
        assert_eq!(pointer.drag_anchor, None);
        assert_eq!(
            pane.selection,
            Some(Selection::new(CellPos::new(2, 42), CellPos::new(4, 42)))
        );
        pointer.select_at(&mut pane, 3, 0, first + Duration::from_secs(2));
        assert!(pane.selection.is_none());
        assert!(pointer.drag_anchor.is_some());

        pointer.end_drag();
        let mut sibling = pane.clone();
        sibling.summary.id = PaneId::new();
        pointer.select_at(
            &mut sibling,
            3,
            0,
            first + Duration::from_secs(2) + DOUBLE_CLICK_WINDOW,
        );
        assert!(sibling.selection.is_none());
        assert_eq!(
            pointer.drag_anchor,
            Some((role, sibling.summary.id, CellPos::new(3, 42)))
        );
    }
}
