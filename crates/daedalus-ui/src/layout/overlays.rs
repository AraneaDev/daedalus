use super::{PaneRect, Rect};
use crate::state::{AgentPreset, CloseWorkspaceModalState};

const PICKER_MODAL_LOGICAL_WIDTH: f32 = 380.0;
pub(crate) const PICKER_HEADER_LOGICAL_HEIGHT: f32 = 42.0;
const PICKER_ITEM_LOGICAL_HEIGHT: f32 = 36.0;
const PICKER_ITEM_LOGICAL_GAP: f32 = 4.0;
const PICKER_PAD_LOGICAL: f32 = 12.0;

const WORKTREE_MODAL_LOGICAL_WIDTH: f32 = 420.0;
const WORKTREE_MODAL_LOGICAL_HEIGHT: f32 = 136.0;
pub(crate) const WORKTREE_HEADER_LOGICAL_HEIGHT: f32 = 38.0;
const WORKTREE_INPUT_LOGICAL_HEIGHT: f32 = 36.0;
const WORKTREE_PAD_LOGICAL: f32 = 14.0;

const SEARCH_BAR_LOGICAL_WIDTH: f32 = 340.0;
const SEARCH_BAR_LOGICAL_HEIGHT: f32 = 58.0;

const CLOSE_MODAL_LOGICAL_WIDTH: f32 = 440.0;
const CLOSE_MODAL_LOGICAL_HEIGHT: f32 = 272.0;
const CLOSE_MODAL_HEADER_LOGICAL_HEIGHT: f32 = 38.0;
const CLOSE_MODAL_PAD_LOGICAL: f32 = 16.0;
const CLOSE_MODAL_ROW_LOGICAL_HEIGHT: f32 = 26.0;
const CLOSE_MODAL_ROW_LOGICAL_GAP: f32 = 6.0;
const CLOSE_MODAL_BTN_LOGICAL_HEIGHT: f32 = 30.0;

pub(crate) fn sanitize_scale(scale_factor: f32) -> f32 {
    if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    }
}

/// Computes the centered physical-pixel rectangle of the Agent preset picker modal.
pub fn picker_modal_rect(window_w: f32, window_h: f32, scale_factor: f32) -> Rect {
    picker_rect_for_rows(window_w, window_h, scale_factor, AgentPreset::ALL.len())
}

pub fn action_palette_layout(
    window_w: f32,
    window_h: f32,
    scale_factor: f32,
    count: usize,
    selected: usize,
) -> (Rect, std::ops::Range<usize>) {
    let s = sanitize_scale(scale_factor);
    let row_height = picker_row_stride(1.0);
    let available = window_h / s - 24.0 - PICKER_HEADER_LOGICAL_HEIGHT - PICKER_PAD_LOGICAL
        + PICKER_ITEM_LOGICAL_GAP;
    let rows = ((available / row_height) as usize).max(1).min(count.max(1));
    let start = (selected.min(count.saturating_sub(1)) + 1).saturating_sub(rows);
    (
        picker_rect_for_rows(window_w, window_h, s, rows),
        start..(start + rows).min(count),
    )
}

/// Visible rows shared by overlay drawing, text shaping and pointer hit testing.
pub fn preset_picker_layout(
    width: f32,
    height: f32,
    scale: f32,
    selected: usize,
) -> (Rect, std::ops::Range<usize>) {
    action_palette_layout(width, height, scale, AgentPreset::ALL.len(), selected)
}

fn picker_rect_for_rows(window_w: f32, window_h: f32, scale_factor: f32, rows: usize) -> Rect {
    let s = sanitize_scale(scale_factor);
    let item_count = rows as f32;
    let modal_w = (PICKER_MODAL_LOGICAL_WIDTH * s).min((window_w - 24.0 * s).max(120.0));
    let modal_h = (PICKER_HEADER_LOGICAL_HEIGHT
        + item_count * PICKER_ITEM_LOGICAL_HEIGHT
        + (item_count - 1.0).max(0.0) * PICKER_ITEM_LOGICAL_GAP
        + PICKER_PAD_LOGICAL)
        * s;
    let x = ((window_w - modal_w) * 0.5).max(0.0);
    let y = ((window_h - modal_h) * 0.5).max(0.0);
    Rect::new(x, y, modal_w, modal_h)
}

/// Distance between adjacent preset or action rows in physical pixels.
pub fn picker_row_stride(scale_factor: f32) -> f32 {
    (PICKER_ITEM_LOGICAL_HEIGHT + PICKER_ITEM_LOGICAL_GAP) * sanitize_scale(scale_factor)
}

/// Computes the physical-pixel rectangle of item `index` inside the Agent preset picker modal.
pub fn picker_item_rect(modal: Rect, index: usize, scale_factor: f32) -> Rect {
    let s = sanitize_scale(scale_factor);
    let pad = PICKER_PAD_LOGICAL * s;
    let x = modal.x + pad;
    let w = (modal.width - 2.0 * pad).max(0.0);
    let y = modal.y + PICKER_HEADER_LOGICAL_HEIGHT * s + (index as f32) * picker_row_stride(s);
    Rect::new(x, y, w, PICKER_ITEM_LOGICAL_HEIGHT * s)
}

/// Computes the centered physical-pixel rectangle of the "New Git Worktree" modal.
pub fn worktree_modal_rect(window_w: f32, window_h: f32, scale_factor: f32) -> Rect {
    let s = sanitize_scale(scale_factor);
    let margin_x = (12.0 * s).min(window_w.max(0.0) * 0.04);
    let margin_y = (12.0 * s).min(window_h.max(0.0) * 0.04);
    let modal_w = (WORKTREE_MODAL_LOGICAL_WIDTH * s).min((window_w - 2.0 * margin_x).max(0.0));
    let modal_h = (WORKTREE_MODAL_LOGICAL_HEIGHT * s).min((window_h - 2.0 * margin_y).max(0.0));
    let x = ((window_w - modal_w) * 0.5).max(0.0);
    let y = ((window_h - modal_h) * 0.5).max(0.0);
    Rect::new(x, y, modal_w, modal_h)
}

pub(crate) fn worktree_content_scale(modal: Rect, scale_factor: f32) -> f32 {
    sanitize_scale(scale_factor).min(modal.height / WORKTREE_MODAL_LOGICAL_HEIGHT)
}

pub fn worktree_button_rects(modal: Rect, scale_factor: f32) -> (Rect, Rect) {
    let s = worktree_content_scale(modal, scale_factor);
    let pad = (WORKTREE_PAD_LOGICAL * s).min(modal.width * 0.1);
    let gap = (8.0 * s).min(modal.width * 0.1);
    let width = (modal.width - 2.0 * pad - gap) * 0.5;
    let height = 30.0 * s;
    let y = modal.bottom() - 10.0 * s - height;
    (
        Rect::new(modal.x + pad, y, width, height),
        Rect::new(modal.right() - pad - width, y, width, height),
    )
}

/// Computes the physical-pixel rectangle of the branch input field inside the worktree modal.
pub fn worktree_input_rect(modal: Rect, scale_factor: f32) -> Rect {
    let s = worktree_content_scale(modal, scale_factor);
    let pad = (WORKTREE_PAD_LOGICAL * s).min(modal.width * 0.1);
    let x = modal.x + pad;
    let w = (modal.width - 2.0 * pad).max(0.0);
    let y = modal.y + (WORKTREE_HEADER_LOGICAL_HEIGHT + 10.0) * s;
    Rect::new(x, y, w, WORKTREE_INPUT_LOGICAL_HEIGHT * s)
}

/// Computes the top-right floating physical-pixel rectangle of the scrollback search bar
/// inside `pane_rect`.
pub fn search_bar_rect(pane_rect: &PaneRect, scale_factor: f32) -> Rect {
    let s = sanitize_scale(scale_factor);
    let margin = (8.0 * s).min(pane_rect.outer.width * 0.25);
    let bar_w = (SEARCH_BAR_LOGICAL_WIDTH * s).min(pane_rect.outer.width - 2.0 * margin);
    let body_h = (pane_rect.outer.bottom() - pane_rect.header.bottom()).max(0.0);
    let top_margin = (6.0 * s).min(body_h * 0.25);
    let bar_h = (SEARCH_BAR_LOGICAL_HEIGHT * s).min(body_h - top_margin);
    let x = pane_rect.outer.right() - bar_w - margin;
    let y = pane_rect.header.bottom() + top_margin;
    Rect::new(x, y, bar_w, bar_h)
}

/// Previous, next and match-case hit areas occupy the lower row of the search bar.
pub fn search_controls(bar: Rect) -> [Rect; 3] {
    let y = bar.y + bar.height * 0.5;
    let h = bar.bottom() - y;
    let first = bar.width * 0.28;
    let second = bar.width * 0.22;
    [
        Rect::new(bar.x, y, first, h),
        Rect::new(bar.x + first, y, second, h),
        Rect::new(bar.x + first + second, y, bar.width - first - second, h),
    ]
}

/// Computes the centered physical-pixel rectangle of the "Close Workspace" confirmation modal.
pub fn close_workspace_modal_rect(window_w: f32, window_h: f32, scale_factor: f32) -> Rect {
    let s = sanitize_scale(scale_factor);
    // Yield the outer margin first when the available space is tight.
    let modal_w =
        (CLOSE_MODAL_LOGICAL_WIDTH * s).min(window_w - (12.0 * s).min(window_w * 0.04) * 2.0);
    let modal_h =
        (CLOSE_MODAL_LOGICAL_HEIGHT * s).min(window_h - (12.0 * s).min(window_h * 0.025) * 2.0);
    let x = ((window_w - modal_w) * 0.5).max(0.0);
    let y = ((window_h - modal_h) * 0.5).max(0.0);
    Rect::new(x, y, modal_w, modal_h)
}

#[derive(Clone, Copy)]
pub(crate) struct CloseModalMetrics {
    pub(crate) scale: f32,
    pub(crate) compact: bool,
    pub(crate) header_height: f32,
    row_height: f32,
    row_gap: f32,
    pad: f32,
}

pub(crate) fn close_modal_metrics(modal: Rect, scale_factor: f32) -> CloseModalMetrics {
    let requested = sanitize_scale(scale_factor);
    let compact = modal.height < CLOSE_MODAL_LOGICAL_HEIGHT * requested - 0.01
        || modal.width < 310.0 * requested;
    let scale = requested.min(modal.height / 170.0).min(modal.width / 235.0);
    CloseModalMetrics {
        compact,
        scale,
        header_height: if compact {
            25.0
        } else {
            CLOSE_MODAL_HEADER_LOGICAL_HEIGHT
        },
        row_height: if compact {
            22.0
        } else {
            CLOSE_MODAL_ROW_LOGICAL_HEIGHT
        },
        row_gap: if compact {
            1.0
        } else {
            CLOSE_MODAL_ROW_LOGICAL_GAP
        },
        pad: if compact {
            6.0
        } else {
            CLOSE_MODAL_PAD_LOGICAL
        },
    }
}

pub(crate) fn close_modal_dirty_banner_rect(modal: Rect, scale_factor: f32) -> Rect {
    let metrics = close_modal_metrics(modal, scale_factor);
    let s = metrics.scale;
    let pad = metrics.pad * s;
    Rect::new(
        modal.x + pad,
        modal.y + (if metrics.compact { 45.0 } else { 72.0 }) * s,
        (modal.width - 2.0 * pad).max(0.0),
        (if metrics.compact { 20.0 } else { 24.0 }) * s,
    )
}

fn close_modal_checkbox_start_y(
    modal: Rect,
    close_state: &CloseWorkspaceModalState,
    metrics: CloseModalMetrics,
) -> f32 {
    let s = metrics.scale;
    if metrics.compact {
        return modal.y + if close_state.is_dirty { 66.0 } else { 45.0 } * s;
    }
    if close_state.is_dirty {
        modal.y + 104.0 * s
    } else {
        modal.y + 74.0 * s
    }
}

fn close_modal_row_rect(modal: Rect, y: f32, metrics: CloseModalMetrics) -> Rect {
    let s = metrics.scale;
    let pad = metrics.pad * s;
    Rect::new(
        modal.x + pad,
        y,
        (modal.width - 2.0 * pad).max(0.0),
        metrics.row_height * s,
    )
}

/// Rectangle for the "Stop Docker Compose (c)" checkbox row inside the close modal.
pub fn close_modal_compose_rect(
    modal: Rect,
    close_state: &CloseWorkspaceModalState,
    scale_factor: f32,
) -> Rect {
    let metrics = close_modal_metrics(modal, scale_factor);
    let y = close_modal_checkbox_start_y(modal, close_state, metrics);
    close_modal_row_rect(modal, y, metrics)
}

/// Rectangle for the "Remove git worktree (w)" checkbox row inside the close modal (when `is_worktree`).
pub fn close_modal_worktree_rect(
    modal: Rect,
    close_state: &CloseWorkspaceModalState,
    scale_factor: f32,
) -> Option<Rect> {
    if !close_state.is_worktree {
        return None;
    }
    let metrics = close_modal_metrics(modal, scale_factor);
    let s = metrics.scale;
    let step = (metrics.row_height + metrics.row_gap) * s;
    let y = close_modal_checkbox_start_y(modal, close_state, metrics) + step;
    Some(close_modal_row_rect(modal, y, metrics))
}

/// Rectangle for the "Force remove dirty worktree (f)" checkbox row (when `is_worktree && is_dirty && remove_worktree`).
pub fn close_modal_force_rect(
    modal: Rect,
    close_state: &CloseWorkspaceModalState,
    scale_factor: f32,
) -> Option<Rect> {
    if !(close_state.is_worktree && close_state.is_dirty && close_state.remove_worktree) {
        return None;
    }
    let metrics = close_modal_metrics(modal, scale_factor);
    let s = metrics.scale;
    let step = (metrics.row_height + metrics.row_gap) * s;
    let y = close_modal_checkbox_start_y(modal, close_state, metrics) + 2.0 * step;
    Some(close_modal_row_rect(modal, y, metrics))
}

/// Rectangle for the Cancel button inside the close modal.
pub fn close_modal_cancel_rect(modal: Rect, scale_factor: f32) -> Rect {
    let metrics = close_modal_metrics(modal, scale_factor);
    let s = metrics.scale;
    let pad = metrics.pad * s;
    let gap = 10.0 * s;
    let btn_w = ((modal.width - 2.0 * pad - gap) * 0.5).max(0.0);
    let btn_h = if metrics.compact {
        26.0
    } else {
        CLOSE_MODAL_BTN_LOGICAL_HEIGHT
    } * s;
    let y = modal.bottom() - btn_h - if metrics.compact { 4.0 } else { 28.0 } * s;
    Rect::new(modal.x + pad, y, btn_w, btn_h)
}

/// Rectangle for the Confirm button inside the close modal.
pub fn close_modal_confirm_rect(modal: Rect, scale_factor: f32) -> Rect {
    let metrics = close_modal_metrics(modal, scale_factor);
    let s = metrics.scale;
    let cancel = close_modal_cancel_rect(modal, scale_factor);
    let gap = 10.0 * s;
    let pad = metrics.pad * s;
    let btn_w = (modal.right() - pad - (cancel.right() + gap)).max(0.0);
    Rect::new(cancel.right() + gap, cancel.y, btn_w, cancel.height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_palette_scrolls_last_action_into_view_at_high_dpi() {
        let (modal, visible) = action_palette_layout(1280.0, 720.0, 2.0, 32, 31);
        assert!(visible.start > 0);
        assert_eq!(visible.end, 32);
        assert!(picker_item_rect(modal, visible.len() - 1, 2.0).bottom() <= modal.bottom());
        assert!(modal.bottom() <= 720.0);
        assert!(action_palette_layout(1280.0, 720.0, 2.0, 0, 0).1.is_empty());
    }

    #[test]
    fn picker_modal_geometry_is_centered_and_items_fit_inside() {
        let modal = picker_modal_rect(1280.0, 720.0, 1.0);
        assert!((modal.x + modal.width * 0.5 - 640.0).abs() < 1e-3);
        assert!((modal.y + modal.height * 0.5 - 360.0).abs() < 1e-3);
        for idx in 0..AgentPreset::ALL.len() {
            let item = picker_item_rect(modal, idx, 1.0);
            assert!(item.x >= modal.x && item.right() <= modal.right());
            assert!(item.y >= modal.y && item.bottom() <= modal.bottom());
        }
    }

    #[test]
    fn short_preset_picker_keeps_each_selection_visible_and_clickable() {
        for scale in [1.0, 2.0] {
            for selected in 0..AgentPreset::ALL.len() {
                let (modal, visible) =
                    preset_picker_layout(320.0 * scale, 144.0 * scale, scale, selected);
                assert!(modal.bottom() <= 144.0 * scale);
                let row = picker_item_rect(modal, selected - visible.start, scale);
                assert!(visible.contains(&selected) && row.bottom() <= modal.bottom());
                let clicked = visible.enumerate().find(|(i, _)| {
                    picker_item_rect(modal, *i, scale).contains(row.x + 1.0, row.y + 1.0)
                });
                assert_eq!(clicked.map(|(_, index)| index), Some(selected));
            }
        }
    }
}
