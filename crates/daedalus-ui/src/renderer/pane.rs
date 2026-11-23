use super::quads::QuadInstance;
use super::{
    agent_state_badge, emit_cell_run_quad, pane_agent_state_pill_rect, push_chrome_badge,
    push_rect_border, status_dot_color,
};
use crate::layout::{PaneRect, Rect};
use crate::state::{AppState, PaneViewState, SearchState};
use crate::theme::{Rgba, Theme};
use daedalus_proto::{CellFlags, GridRow, PaneRole};
use std::collections::HashMap;

pub(super) fn collect_pane_quads(
    quads: &mut Vec<QuadInstance>,
    pane_rect: &PaneRect,
    state: &AppState,
    theme: &Theme,
    scale_factor: f32,
    border_w: f32,
    cell_size: (f32, f32),
) {
    let is_focused = pane_rect.role == state.focused_role;
    let pane_state = state.pane_for_role(pane_rect.role);
    collect_pane_chrome(
        quads,
        pane_rect,
        pane_state,
        theme,
        scale_factor,
        border_w,
        state.zoomed,
    );
    if let Some(pane) = pane_state {
        let visible = pane.visible_rows();
        let grid = PaneGrid {
            rows: &visible[..visible.len().min(pane_rect.rows as usize)],
            pane_rect,
            theme,
            cell_size,
            border_w,
        };
        grid.backgrounds(quads);
        if let Some(search) = &state.search
            && search.pane_id == pane.summary.id
            && !search.matches.is_empty()
        {
            grid.search_highlights(quads, search);
        }
        grid.selection(quads, pane);
        grid.decorations(quads);
        grid.cursor(quads, pane, is_focused);
    }
    // Pane outer border (focused pane highlighted in accent blue)
    let border_color = if is_focused {
        theme.border_focused
    } else {
        theme.border_unfocused
    };
    push_rect_border(quads, pane_rect.outer, border_w, border_color);
}

fn collect_pane_chrome(
    quads: &mut Vec<QuadInstance>,
    pane_rect: &PaneRect,
    pane_state: Option<&PaneViewState>,
    theme: &Theme,
    scale_factor: f32,
    border_w: f32,
    zoomed: bool,
) {
    // Pane body & header backgrounds
    quads.push(QuadInstance::new(pane_rect.outer, theme.bg_pane));
    quads.push(QuadInstance::new(pane_rect.header, theme.bg_header));

    // Role / status dot in pane header
    let dot_color = pane_state.map_or_else(
        || theme.role_accent(pane_rect.role),
        |p| status_dot_color(p.summary.status, theme),
    );
    let dot_size = 7.0 * scale_factor;
    if !zoomed {
        quads.push(QuadInstance::from_xywh(
            pane_rect.header.x + 9.0 * scale_factor,
            pane_rect.header.y + (pane_rect.header.height - dot_size) * 0.5,
            dot_size,
            dot_size,
            dot_color,
        ));
    }

    // Agent state badge pill (left of action_pill in Agent pane header)
    if !zoomed
        && pane_rect.role == PaneRole::Agent
        && let Some(agent_state) = pane_state.and_then(|p| p.summary.agent_state)
    {
        let (label, color) = agent_state_badge(agent_state, theme);
        if let Some(pill) = pane_agent_state_pill_rect(pane_rect, label, scale_factor) {
            push_chrome_badge(quads, pill, theme, border_w, color.with_alpha(0.70));
        }
    }

    // Action pill background + border
    if let Some(pill) = pane_rect.action_pill {
        let border = match pane_rect.role {
            PaneRole::Agent => theme.border_focused.with_alpha(0.55),
            PaneRole::Runner => theme.status_err.with_alpha(0.75),
            PaneRole::Service => theme.border_unfocused,
        };
        push_chrome_badge(quads, pill, theme, border_w, border);
    }

    // Header separator
    quads.push(QuadInstance::from_xywh(
        pane_rect.header.x,
        pane_rect.header.bottom() - border_w,
        pane_rect.header.width,
        border_w,
        theme.border_unfocused,
    ));
}

struct PaneGrid<'a> {
    rows: &'a [GridRow],
    pane_rect: &'a PaneRect,
    theme: &'a Theme,
    cell_size: (f32, f32),
    border_w: f32,
}

impl PaneGrid<'_> {
    fn backgrounds(&self, quads: &mut Vec<QuadInstance>) {
        let vp = self.pane_rect.viewport;
        let (cell_w, cell_h) = self.cell_size;
        // Run-length merged cell backgrounds
        for (row_idx, grid_row) in self.rows.iter().enumerate() {
            let row_y = vp.y + (row_idx as f32) * cell_h;
            let mut active_run: Option<(usize, usize, Rgba)> = None;

            for (col_idx, cell) in grid_row.cells.iter().enumerate() {
                if col_idx >= self.pane_rect.cols as usize {
                    break;
                }
                if cell.width == 0 {
                    continue;
                }
                let span = (cell.width as usize).min((self.pane_rect.cols as usize) - col_idx);
                let (_fg, bg_opt) = self.theme.resolve_cell_colors(cell.fg, cell.bg, cell.flags);

                match (active_run, bg_opt) {
                    (Some((start, end, prev_bg)), Some(bg)) if end == col_idx && prev_bg == bg => {
                        active_run = Some((start, col_idx + span, prev_bg));
                    }
                    (Some((start, end, prev_bg)), next_bg) => {
                        emit_cell_run_quad(quads, vp, row_y, start, end, cell_w, cell_h, prev_bg);
                        active_run = next_bg.map(|bg| (col_idx, col_idx + span, bg));
                    }
                    (None, Some(bg)) => {
                        active_run = Some((col_idx, col_idx + span, bg));
                    }
                    (None, None) => {}
                }
            }
            if let Some((start, end, bg)) = active_run {
                emit_cell_run_quad(quads, vp, row_y, start, end, cell_w, cell_h, bg);
            }
        }
    }

    fn search_highlights(&self, quads: &mut Vec<QuadInstance>, search: &SearchState) {
        let vp = self.pane_rect.viewport;
        let (cell_w, cell_h) = self.cell_size;
        // Index visible IDs but emit in visual row order, including repeated absolute IDs.
        let mut visible_matches: HashMap<_, Vec<_>> =
            self.rows.iter().map(|r| (r.abs_row, Vec::new())).collect();
        for (index, hit) in search.matches.iter().enumerate() {
            if let Some(matches) = visible_matches.get_mut(&hit.abs_row) {
                matches.push((index, hit));
            }
        }
        for (row_idx, row) in self.rows.iter().enumerate() {
            let row_y = vp.y + (row_idx as f32) * cell_h;
            for &(match_idx, m) in &visible_matches[&row.abs_row] {
                let start_col = (m.start_col as usize).min(self.pane_rect.cols as usize);
                let end_col = (m.end_col as usize).min(self.pane_rect.cols as usize);
                if end_col <= start_col {
                    continue;
                }
                let is_active = match_idx == search.current_index;
                let fill = if is_active {
                    self.theme.status_busy.with_alpha(0.55)
                } else {
                    self.theme.status_busy.with_alpha(0.24)
                };
                emit_cell_run_quad(quads, vp, row_y, start_col, end_col, cell_w, cell_h, fill);
                if is_active {
                    let x = vp.x + (start_col as f32) * cell_w;
                    let w =
                        (((end_col - start_col) as f32) * cell_w).min((vp.right() - x).max(0.0));
                    if w > 0.0 {
                        push_rect_border(
                            quads,
                            Rect::new(x, row_y, w, cell_h),
                            self.border_w,
                            self.theme.status_busy,
                        );
                    }
                }
            }
        }
    }

    fn selection(&self, quads: &mut Vec<QuadInstance>, pane: &PaneViewState) {
        let vp = self.pane_rect.viewport;
        let (cell_w, cell_h) = self.cell_size;
        // Selection highlight quads
        if let Some(sel) = pane.selection {
            let (start, end) = sel.normalized();
            let max_col = self.pane_rect.cols.saturating_sub(1);
            for (row_idx, grid_row) in self.rows.iter().enumerate() {
                if grid_row.abs_row < start.abs_row || grid_row.abs_row > end.abs_row {
                    continue;
                }
                let col_start = if grid_row.abs_row == start.abs_row {
                    start.col
                } else {
                    0
                };
                let col_end = if grid_row.abs_row == end.abs_row {
                    end.col.min(max_col)
                } else {
                    max_col
                };
                if col_start <= col_end {
                    let row_y = vp.y + (row_idx as f32) * cell_h;
                    emit_cell_run_quad(
                        quads,
                        vp,
                        row_y,
                        col_start as usize,
                        col_end as usize + 1,
                        cell_w,
                        cell_h,
                        self.theme.selection_bg,
                    );
                }
            }
        }
    }

    fn decorations(&self, quads: &mut Vec<QuadInstance>) {
        let vp = self.pane_rect.viewport;
        let (cell_w, cell_h) = self.cell_size;
        // Underline and strikethrough 1px bars
        let bar_h = self.border_w;
        for (row_idx, grid_row) in self.rows.iter().enumerate() {
            let row_y = vp.y + (row_idx as f32) * cell_h;
            for (col_idx, cell) in grid_row.cells.iter().enumerate() {
                if col_idx >= self.pane_rect.cols as usize {
                    break;
                }
                if cell.width == 0 {
                    continue;
                }
                let has_underline = cell.flags.contains(CellFlags::UNDERLINE);
                let has_strike = cell.flags.contains(CellFlags::STRIKETHROUGH);
                if !has_underline && !has_strike {
                    continue;
                }
                let (fg, _bg) = self.theme.resolve_cell_colors(cell.fg, cell.bg, cell.flags);
                let span = (cell.width as usize).min((self.pane_rect.cols as usize) - col_idx);
                let x = vp.x + (col_idx as f32) * cell_w;
                let w = ((span as f32) * cell_w).min((vp.right() - x).max(0.0));
                if w <= 0.0 {
                    continue;
                }
                if has_underline {
                    quads.push(QuadInstance::from_xywh(
                        x,
                        row_y + cell_h - bar_h,
                        w,
                        bar_h,
                        fg,
                    ));
                }
                if has_strike {
                    quads.push(QuadInstance::from_xywh(
                        x,
                        row_y + (cell_h * 0.5).floor(),
                        w,
                        bar_h,
                        fg,
                    ));
                }
            }
        }
    }

    fn cursor(&self, quads: &mut Vec<QuadInstance>, pane: &PaneViewState, is_focused: bool) {
        let vp = self.pane_rect.viewport;
        let (cell_w, cell_h) = self.cell_size;
        // Cursor quad (hidden when cursor.visible == false or scrolled into history)
        if pane.cursor.visible
            && pane.scroll_offset == 0
            && pane.cursor.col < self.pane_rect.cols
            && pane.cursor.row < self.pane_rect.rows
        {
            let cx = vp.x + (pane.cursor.col as f32) * cell_w;
            let cy = vp.y + (pane.cursor.row as f32) * cell_h;
            let cursor_rect = Rect::new(cx, cy, cell_w, cell_h);
            if is_focused {
                quads.push(QuadInstance::new(
                    cursor_rect,
                    self.theme.cursor_bg.with_alpha(0.65),
                ));
            } else {
                push_rect_border(
                    quads,
                    cursor_rect,
                    self.border_w,
                    self.theme.cursor_bg.with_alpha(0.80),
                );
            }
        }
    }
}
