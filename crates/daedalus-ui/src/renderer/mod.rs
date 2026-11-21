pub mod quads;
pub mod text;

use std::collections::BTreeSet;
use std::sync::Arc;

use daedalus_proto::{
    CellFlags, ContainerHealth, ExternalStatus, PaneRole, PaneStatus, PortMapping, StatusLevel,
    WorkspaceInfo,
};
use winit::window::Window;

use crate::layout::{
    PaneRect, Rect, WorkspaceLayout, sidebar_new_workspace_rect, sidebar_new_worktree_rect,
    sidebar_workspace_rect,
};
use crate::state::{AgentPreset, AppState};
use crate::theme::{self, Rgba, Theme};

use self::quads::{QuadInstance, QuadPipeline, rgba_to_wgpu_clear_color};
use self::text::TextRenderer;

const PICKER_MODAL_LOGICAL_WIDTH: f32 = 380.0;
const PICKER_HEADER_LOGICAL_HEIGHT: f32 = 42.0;
const PICKER_ITEM_LOGICAL_HEIGHT: f32 = 36.0;
const PICKER_ITEM_LOGICAL_GAP: f32 = 4.0;
const PICKER_PAD_LOGICAL: f32 = 12.0;

const WORKTREE_MODAL_LOGICAL_WIDTH: f32 = 420.0;
const WORKTREE_MODAL_LOGICAL_HEIGHT: f32 = 136.0;
const WORKTREE_HEADER_LOGICAL_HEIGHT: f32 = 38.0;
const WORKTREE_INPUT_LOGICAL_HEIGHT: f32 = 36.0;
const WORKTREE_PAD_LOGICAL: f32 = 14.0;

fn sanitize_scale(scale_factor: f32) -> f32 {
    if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    }
}

/// Computes the centered physical-pixel rectangle of the Agent preset picker modal.
pub fn picker_modal_rect(window_w: f32, window_h: f32, scale_factor: f32) -> Rect {
    let s = sanitize_scale(scale_factor);
    let item_count = AgentPreset::ALL.len() as f32;
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

/// Computes the physical-pixel rectangle of item `index` inside the Agent preset picker modal.
pub fn picker_item_rect(modal: Rect, index: usize, scale_factor: f32) -> Rect {
    let s = sanitize_scale(scale_factor);
    let pad = PICKER_PAD_LOGICAL * s;
    let x = modal.x + pad;
    let w = (modal.width - 2.0 * pad).max(0.0);
    let y = modal.y
        + PICKER_HEADER_LOGICAL_HEIGHT * s
        + (index as f32) * (PICKER_ITEM_LOGICAL_HEIGHT + PICKER_ITEM_LOGICAL_GAP) * s;
    Rect::new(x, y, w, PICKER_ITEM_LOGICAL_HEIGHT * s)
}

/// Computes the centered physical-pixel rectangle of the "New Git Worktree" modal.
pub fn worktree_modal_rect(window_w: f32, window_h: f32, scale_factor: f32) -> Rect {
    let s = sanitize_scale(scale_factor);
    let modal_w = (WORKTREE_MODAL_LOGICAL_WIDTH * s).min((window_w - 24.0 * s).max(160.0));
    let modal_h = (WORKTREE_MODAL_LOGICAL_HEIGHT * s).min((window_h - 24.0 * s).max(100.0));
    let x = ((window_w - modal_w) * 0.5).max(0.0);
    let y = ((window_h - modal_h) * 0.5).max(0.0);
    Rect::new(x, y, modal_w, modal_h)
}

/// Computes the physical-pixel rectangle of the branch input field inside the worktree modal.
pub fn worktree_input_rect(modal: Rect, scale_factor: f32) -> Rect {
    let s = sanitize_scale(scale_factor);
    let pad = WORKTREE_PAD_LOGICAL * s;
    let x = modal.x + pad;
    let w = (modal.width - 2.0 * pad).max(0.0);
    let y = modal.y + (WORKTREE_HEADER_LOGICAL_HEIGHT + 10.0) * s;
    Rect::new(x, y, w, WORKTREE_INPUT_LOGICAL_HEIGHT * s)
}

pub(super) fn workspace_branch_badge(ws: &WorkspaceInfo) -> Option<(String, bool)> {
    match (&ws.git_branch, ws.slot_index > 0) {
        (Some(branch), true) => Some((format!("wt{} · {branch}", ws.slot_index), true)),
        (Some(branch), false) => Some((branch.clone(), false)),
        (None, true) => Some((format!("wt{}", ws.slot_index), true)),
        (None, false) => None,
    }
}

pub(super) fn sidebar_branch_badge_rect(row_rect: Rect, label: &str, scale_factor: f32) -> Rect {
    let s = sanitize_scale(scale_factor);
    let left_x = row_rect.x + 22.0 * s;
    let max_w = (row_rect.right() - left_x - 8.0 * s).max(0.0);
    let est_w = ((label.chars().count() as f32) * 6.8 + 12.0) * s;
    Rect::new(
        left_x,
        row_rect.y + 24.0 * s,
        est_w.min(max_w),
        15.0 * s,
    )
}

pub(super) fn format_ports_summary(bound_ports: &[PortMapping]) -> (String, bool) {
    let unique: BTreeSet<u16> = bound_ports.iter().map(|p| p.host_port).collect();
    if unique.is_empty() {
        return ("no bound ports".to_string(), false);
    }
    let count = unique.len();
    let noun = if count == 1 { "port" } else { "ports" };
    let mut preview: Vec<String> = unique.iter().take(2).map(u16::to_string).collect();
    let suffix = if count > 2 { ", …" } else { "" };
    if !suffix.is_empty()
        && let Some(last) = preview.last_mut()
    {
        last.push_str(suffix);
    }
    (format!("● {count} {noun}: {}", preview.join(", ")), true)
}

pub(super) fn format_docker_health(containers: &[ContainerHealth]) -> Option<(String, bool)> {
    if containers.is_empty() {
        return None;
    }
    let total = containers.len();
    let healthy = containers
        .iter()
        .filter(|c| {
            c.state.eq_ignore_ascii_case("running") && c.health.as_deref() != Some("unhealthy")
        })
        .count();
    if healthy == total {
        Some((format!("{healthy}/{total} up"), true))
    } else {
        Some((format!("{healthy}/{total} up !"), false))
    }
}

pub(super) fn sidebar_docker_pill_rect(row_rect: Rect, scale_factor: f32) -> Rect {
    let s = sanitize_scale(scale_factor);
    let w = 64.0 * s;
    Rect::new(
        row_rect.right() - w - 6.0 * s,
        row_rect.y + 42.0 * s,
        w,
        15.0 * s,
    )
}

pub(super) fn visible_external_statuses(ws: &WorkspaceInfo) -> &[ExternalStatus] {
    let start = ws.external_statuses.len().saturating_sub(2);
    &ws.external_statuses[start..]
}

pub(super) fn external_status_color(level: StatusLevel, theme: &Theme) -> Rgba {
    match level {
        StatusLevel::Info => theme.border_focused,
        StatusLevel::Ok => theme.status_ok,
        StatusLevel::Warn => theme.status_busy,
        StatusLevel::Error => theme.status_err,
    }
}

pub(super) fn sidebar_external_badge_rect(
    row_rect: Rect,
    badge_idx: usize,
    badge_count: usize,
    scale_factor: f32,
) -> Rect {
    let s = sanitize_scale(scale_factor);
    let left_x = row_rect.x + 10.0 * s;
    let total_w = (row_rect.right() - 8.0 * s - left_x).max(0.0);
    let y = row_rect.y + 61.0 * s;
    let h = 16.0 * s;
    if badge_count <= 1 {
        Rect::new(left_x, y, total_w, h)
    } else {
        let gap = 4.0 * s;
        let w = ((total_w - gap) * 0.5).max(0.0);
        Rect::new(left_x + (badge_idx as f32) * (w + gap), y, w, h)
    }
}

fn push_rect_border(quads: &mut Vec<QuadInstance>, rect: Rect, thickness: f32, color: Rgba) {
    if rect.width <= 0.0 || rect.height <= 0.0 || thickness <= 0.0 {
        return;
    }
    let t = thickness.min(rect.width * 0.5).min(rect.height * 0.5);
    // Top
    quads.push(QuadInstance::from_xywh(rect.x, rect.y, rect.width, t, color));
    // Bottom
    quads.push(QuadInstance::from_xywh(
        rect.x,
        rect.bottom() - t,
        rect.width,
        t,
        color,
    ));
    // Left
    quads.push(QuadInstance::from_xywh(
        rect.x,
        rect.y + t,
        t,
        (rect.height - 2.0 * t).max(0.0),
        color,
    ));
    // Right
    quads.push(QuadInstance::from_xywh(
        rect.right() - t,
        rect.y + t,
        t,
        (rect.height - 2.0 * t).max(0.0),
        color,
    ));
}

fn status_dot_color(status: PaneStatus, theme: &Theme) -> Rgba {
    match status {
        PaneStatus::Idle => theme.status_ok.scale_luminance(0.75),
        PaneStatus::Busy => theme.status_busy,
        PaneStatus::Starting => theme.border_focused,
        PaneStatus::Exited { .. } => theme.text_muted,
    }
}

pub(super) fn banner_appearance(state: &AppState, theme: &Theme) -> (Rgba, Option<(String, Rgba)>) {
    if !state.connected {
        if let Some((ref msg, true, _)) = state.toast {
            (
                theme.status_err.with_alpha(0.26),
                Some((format!("Disconnected — {msg}"), theme.status_err)),
            )
        } else {
            (
                theme.status_busy.with_alpha(0.24),
                Some((
                    "Disconnected from daedalusd — reconnecting...".to_string(),
                    theme.status_busy,
                )),
            )
        }
    } else if let Some((ref msg, is_error, _)) = state.toast {
        if is_error {
            (
                theme.status_err.with_alpha(0.26),
                Some((msg.clone(), theme.status_err)),
            )
        } else {
            (
                theme.status_ok.with_alpha(0.22),
                Some((msg.clone(), theme.status_ok)),
            )
        }
    } else {
        (theme.bg_header, None)
    }
}

/// Pure Pass 1 quad collector (headless-testable without a GPU).
///
/// Returns `(quads, base_quad_count)` where `0..base_quad_count` are rendered beneath base text
/// and `base_quad_count..quads.len()` are modal overlay quads rendered above base text.
pub fn collect_quads(
    state: &AppState,
    layout: &WorkspaceLayout,
    theme: &Theme,
    screen_w: f32,
    screen_h: f32,
    scale_factor: f32,
    cell_size: (f32, f32),
) -> (Vec<QuadInstance>, u32) {
    let s = sanitize_scale(scale_factor);
    let border_w = s.round().max(1.0);
    let (cell_w, cell_h) = cell_size;
    let mut quads = Vec::with_capacity(256);

    // 1. Sidebar background, workspace cards, status dots, and "+ New Workspace" button.
    if let Some(sidebar) = layout.sidebar {
        quads.push(QuadInstance::new(sidebar, theme.bg_sidebar));
        // Right divider border
        quads.push(QuadInstance::from_xywh(
            sidebar.right() - border_w,
            sidebar.y,
            border_w,
            sidebar.height,
            theme.border_unfocused,
        ));

        for (idx, ws) in state.workspaces.iter().enumerate() {
            let row_rect = sidebar_workspace_rect(sidebar, idx, s);
            if row_rect.y > sidebar.bottom() {
                break;
            }
            let is_active = state.active_workspace_id == Some(ws.id);
            if is_active {
                quads.push(QuadInstance::new(row_rect, theme.bg_header));
                // Left accent stripe
                quads.push(QuadInstance::from_xywh(
                    row_rect.x,
                    row_rect.y,
                    3.0 * s,
                    row_rect.height,
                    theme.border_focused,
                ));
            } else {
                quads.push(QuadInstance::new(row_rect, theme.bg_pane.with_alpha(0.55)));
            }

            // Agent status dot
            let agent_status = ws
                .panes
                .iter()
                .rev()
                .find(|p| p.role == PaneRole::Agent)
                .map_or(PaneStatus::Exited { code: None }, |p| p.status);
            let dot_size = 7.0 * s;
            quads.push(QuadInstance::from_xywh(
                row_rect.x + 9.0 * s,
                row_rect.y + 11.0 * s,
                dot_size,
                dot_size,
                status_dot_color(agent_status, theme),
            ));

            // Runner status pill backdrop
            if let Some(runner) = ws.panes.iter().rev().find(|p| p.role == PaneRole::Runner) {
                let pill_bg = if runner.has_failure_trace {
                    Some(theme.status_err.with_alpha(0.22))
                } else if runner.last_exit_code == Some(0) {
                    Some(theme.status_ok.with_alpha(0.18))
                } else {
                    None
                };
                if let Some(bg) = pill_bg {
                    quads.push(QuadInstance::from_xywh(
                        row_rect.right() - 52.0 * s,
                        row_rect.y + 5.0 * s,
                        46.0 * s,
                        18.0 * s,
                        bg,
                    ));
                }
            }

            // Git branch / worktree slot badge backdrop
            if let Some((branch_label, is_wt)) = workspace_branch_badge(ws) {
                let badge_rect = sidebar_branch_badge_rect(row_rect, &branch_label, s);
                let bg = if is_wt {
                    theme.border_focused.with_alpha(0.18)
                } else {
                    theme.bg_window.with_alpha(0.75)
                };
                quads.push(QuadInstance::new(badge_rect, bg));
            }

            // Docker container health pill backdrop
            if let Some((_, is_healthy)) = format_docker_health(&ws.containers) {
                let docker_rect = sidebar_docker_pill_rect(row_rect, s);
                let bg = if is_healthy {
                    theme.status_ok.with_alpha(0.15)
                } else {
                    theme.status_err.with_alpha(0.24)
                };
                quads.push(QuadInstance::new(docker_rect, bg));
            }

            // Up to 2 external status badges
            let ext_statuses = visible_external_statuses(ws);
            for (badge_idx, ext) in ext_statuses.iter().enumerate() {
                let badge_rect =
                    sidebar_external_badge_rect(row_rect, badge_idx, ext_statuses.len(), s);
                let color = external_status_color(ext.level, theme);
                quads.push(QuadInstance::new(badge_rect, color.with_alpha(0.18)));
                push_rect_border(&mut quads, badge_rect, border_w, color.with_alpha(0.55));
            }
        }

        let new_btn = sidebar_new_workspace_rect(sidebar, state.workspaces.len(), s);
        if new_btn.y < sidebar.bottom() {
            quads.push(QuadInstance::new(new_btn, theme.bg_header));
            push_rect_border(&mut quads, new_btn, border_w, theme.border_unfocused);
        }

        let new_wt_btn = sidebar_new_worktree_rect(sidebar, state.workspaces.len(), s);
        if new_wt_btn.y < sidebar.bottom() {
            quads.push(QuadInstance::new(new_wt_btn, theme.bg_header));
            push_rect_border(
                &mut quads,
                new_wt_btn,
                border_w,
                theme.border_focused.with_alpha(0.60),
            );
        }
    }

    // 2. Top disconnect / toast banner.
    if let Some(banner) = layout.banner {
        let (banner_bg, _) = banner_appearance(state, theme);
        quads.push(QuadInstance::new(banner, banner_bg));
        quads.push(QuadInstance::from_xywh(
            banner.x,
            banner.bottom() - border_w,
            banner.width,
            border_w,
            theme.border_unfocused,
        ));
    }

    // 3. Semantic panes (background, header, pills, merged cell bgs, selection, decorations, cursor, border).
    for pane_rect in &layout.panes {
        collect_pane_quads(
            &mut quads,
            pane_rect,
            state,
            theme,
            s,
            border_w,
            cell_w,
            cell_h,
        );
    }

    let base_quad_count = quads.len() as u32;

    // 4. Centered Agent preset picker modal overlay (rendered after base text).
    if state.picker.open {
        // Dimmed backdrop scrim
        quads.push(QuadInstance::from_xywh(
            0.0,
            0.0,
            screen_w,
            screen_h,
            Rgba::new(0.0, 0.0, 0.0, 0.55),
        ));

        let modal = picker_modal_rect(screen_w, screen_h, s);
        quads.push(QuadInstance::new(modal, theme.picker_bg));
        push_rect_border(&mut quads, modal, border_w, theme.border_focused);

        // Header divider
        quads.push(QuadInstance::from_xywh(
            modal.x + border_w,
            modal.y + (PICKER_HEADER_LOGICAL_HEIGHT - 4.0) * s,
            (modal.width - 2.0 * border_w).max(0.0),
            border_w,
            theme.border_unfocused,
        ));

        // Selected row highlight
        let selected_rect = picker_item_rect(modal, state.picker.selected_index, s);
        quads.push(QuadInstance::new(
            selected_rect,
            theme.border_focused.with_alpha(0.26),
        ));
        push_rect_border(
            &mut quads,
            selected_rect,
            border_w,
            theme.border_focused.with_alpha(0.75),
        );
    } else if state.worktree_modal.open {
        // Dimmed backdrop scrim
        quads.push(QuadInstance::from_xywh(
            0.0,
            0.0,
            screen_w,
            screen_h,
            Rgba::new(0.0, 0.0, 0.0, 0.55),
        ));

        let modal = worktree_modal_rect(screen_w, screen_h, s);
        quads.push(QuadInstance::new(modal, theme.picker_bg));
        push_rect_border(&mut quads, modal, border_w, theme.border_focused);

        // Header divider
        quads.push(QuadInstance::from_xywh(
            modal.x + border_w,
            modal.y + (WORKTREE_HEADER_LOGICAL_HEIGHT - 2.0) * s,
            (modal.width - 2.0 * border_w).max(0.0),
            border_w,
            theme.border_unfocused,
        ));

        // Branch input box
        let input_rect = worktree_input_rect(modal, s);
        quads.push(QuadInstance::new(input_rect, theme.bg_window));
        push_rect_border(&mut quads, input_rect, border_w, theme.border_focused);
    }

    (quads, base_quad_count)
}

#[allow(clippy::too_many_arguments)]
fn collect_pane_quads(
    quads: &mut Vec<QuadInstance>,
    pane_rect: &PaneRect,
    state: &AppState,
    theme: &Theme,
    scale_factor: f32,
    border_w: f32,
    cell_w: f32,
    cell_h: f32,
) {
    let is_focused = pane_rect.role == state.focused_role;
    let pane_state = state.pane_for_role(pane_rect.role);

    // Pane body & header backgrounds
    quads.push(QuadInstance::new(pane_rect.outer, theme.bg_pane));
    quads.push(QuadInstance::new(pane_rect.header, theme.bg_header));

    // Role / status dot in pane header
    let dot_color = pane_state.map_or_else(
        || theme.role_accent(pane_rect.role),
        |p| status_dot_color(p.summary.status, theme),
    );
    let dot_size = 7.0 * scale_factor;
    quads.push(QuadInstance::from_xywh(
        pane_rect.header.x + 9.0 * scale_factor,
        pane_rect.header.y + (pane_rect.header.height - dot_size) * 0.5,
        dot_size,
        dot_size,
        dot_color,
    ));

    // Action pill background + border
    if let Some(pill) = pane_rect.action_pill {
        let (pill_bg, pill_border) = match pane_rect.role {
            PaneRole::Agent => (
                theme.border_focused.with_alpha(0.20),
                theme.border_focused.with_alpha(0.55),
            ),
            PaneRole::Runner => (
                theme.status_err.with_alpha(0.25),
                theme.status_err.with_alpha(0.75),
            ),
            PaneRole::Service => (theme.bg_pane, theme.border_unfocused),
        };
        quads.push(QuadInstance::new(pill, pill_bg));
        push_rect_border(quads, pill, border_w, pill_border);
    }

    // Header separator
    quads.push(QuadInstance::from_xywh(
        pane_rect.header.x,
        pane_rect.header.bottom() - border_w,
        pane_rect.header.width,
        border_w,
        theme.border_unfocused,
    ));

    // Terminal grid quads (run-length merged backgrounds, selection, underline/strikethrough, cursor)
    if let Some(pane) = pane_state {
        let vp = pane_rect.viewport;
        let max_rows = pane_rect.rows as usize;
        let visible_rows = pane.visible_rows();

        // Run-length merged cell backgrounds
        for (row_idx, grid_row) in visible_rows.iter().take(max_rows).enumerate() {
            let row_y = vp.y + (row_idx as f32) * cell_h;
            let mut active_run: Option<(usize, usize, Rgba)> = None;

            for (col_idx, cell) in grid_row.cells.iter().enumerate() {
                if col_idx >= pane_rect.cols as usize {
                    break;
                }
                if cell.width == 0 {
                    continue;
                }
                let span = (cell.width as usize).min((pane_rect.cols as usize) - col_idx);
                let (_fg, bg_opt) = theme::resolve_cell_colors(cell.fg, cell.bg, cell.flags);

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

        // Selection highlight quads
        if let Some(sel) = pane.selection {
            let (start, end) = sel.normalized();
            let max_col = pane_rect.cols.saturating_sub(1);
            for (row_idx, grid_row) in visible_rows.iter().take(max_rows).enumerate() {
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
                        theme.selection_bg,
                    );
                }
            }
        }

        // Underline and strikethrough 1px bars
        let bar_h = border_w;
        for (row_idx, grid_row) in visible_rows.iter().take(max_rows).enumerate() {
            let row_y = vp.y + (row_idx as f32) * cell_h;
            for (col_idx, cell) in grid_row.cells.iter().enumerate() {
                if col_idx >= pane_rect.cols as usize {
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
                let (fg, _bg) = theme::resolve_cell_colors(cell.fg, cell.bg, cell.flags);
                let span = (cell.width as usize).min((pane_rect.cols as usize) - col_idx);
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

        // Cursor quad (hidden when cursor.visible == false or scrolled into history)
        if pane.cursor.visible
            && pane.scroll_offset == 0
            && pane.cursor.col < pane_rect.cols
            && pane.cursor.row < pane_rect.rows
        {
            let cx = vp.x + (pane.cursor.col as f32) * cell_w;
            let cy = vp.y + (pane.cursor.row as f32) * cell_h;
            let cursor_rect = Rect::new(cx, cy, cell_w, cell_h);
            if is_focused {
                quads.push(QuadInstance::new(
                    cursor_rect,
                    theme.cursor_bg.with_alpha(0.65),
                ));
            } else {
                push_rect_border(
                    quads,
                    cursor_rect,
                    border_w,
                    theme.cursor_bg.with_alpha(0.80),
                );
            }
        }
    }

    // Pane outer border (focused pane highlighted in accent blue)
    let border_color = if is_focused {
        theme.border_focused
    } else {
        theme.border_unfocused
    };
    push_rect_border(quads, pane_rect.outer, border_w, border_color);
}

#[allow(clippy::too_many_arguments)]
fn emit_cell_run_quad(
    quads: &mut Vec<QuadInstance>,
    viewport: Rect,
    row_y: f32,
    start_col: usize,
    end_col_excl: usize,
    cell_w: f32,
    cell_h: f32,
    color: Rgba,
) {
    if end_col_excl <= start_col {
        return;
    }
    let x = viewport.x + (start_col as f32) * cell_w;
    let w = (((end_col_excl - start_col) as f32) * cell_w).min((viewport.right() - x).max(0.0));
    if w > 0.0 {
        quads.push(QuadInstance::from_xywh(x, row_y, w, cell_h, color));
    }
}

/// Two-pass `wgpu` + `glyphon` GPU renderer for `daedalus-ui`.
pub struct GpuRenderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    scale_factor: f32,
    theme: Theme,
    quad_pipeline: QuadPipeline,
    text_renderer: TextRenderer,
}

impl GpuRenderer {
    pub fn new(window: Arc<Window>) -> Result<Self, String> {
        let size = window.inner_size();
        let width = size.width.max(1);
        let height = size.height.max(1);
        let scale_factor = sanitize_scale(window.scale_factor() as f32);

        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });

        let surface = instance
            .create_surface(window)
            .map_err(|err| format!("failed to create wgpu surface: {err}"))?;

        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(|err| format!("failed to create init runtime: {err}"))?;

        let (adapter, device, queue) = rt.block_on(async {
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    compatible_surface: Some(&surface),
                    force_fallback_adapter: false,
                })
                .await
                .ok_or_else(|| "no compatible GPU adapter found".to_string())?;

            let (device, queue) = adapter
                .request_device(
                    &wgpu::DeviceDescriptor {
                        label: Some("daedalus-ui-device"),
                        required_features: wgpu::Features::empty(),
                        required_limits: wgpu::Limits::default(),
                        memory_hints: wgpu::MemoryHints::Performance,
                    },
                    None,
                )
                .await
                .map_err(|err| format!("failed to request GPU device: {err}"))?;

            Ok::<_, String>((adapter, device, queue))
        })?;

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .or_else(|| caps.formats.first().copied())
            .ok_or_else(|| "surface reported no supported texture formats".to_string())?;

        let alpha_mode = caps
            .alpha_modes
            .first()
            .copied()
            .unwrap_or(wgpu::CompositeAlphaMode::Auto);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width,
            height,
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let quad_pipeline = QuadPipeline::new(&device, format);
        let text_renderer = TextRenderer::new(&device, &queue, format, width, height, scale_factor);

        Ok(Self {
            surface,
            device,
            queue,
            config,
            scale_factor,
            theme: Theme::dark(),
            quad_pipeline,
            text_renderer,
        })
    }

    pub fn resize(&mut self, width: u32, height: u32, scale_factor: f32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        self.scale_factor = sanitize_scale(scale_factor);
        self.text_renderer
            .resize(&self.queue, width, height, self.scale_factor);
    }

    pub fn cell_size(&self) -> (f32, f32) {
        self.text_renderer.cell_size()
    }

    pub fn render(
        &mut self,
        state: &mut AppState,
        layout: &WorkspaceLayout,
    ) -> Result<(), wgpu::SurfaceError> {
        let screen_w = self.config.width as f32;
        let screen_h = self.config.height as f32;
        let cell_size = self.text_renderer.cell_size();

        let (quads, base_quad_count) = collect_quads(
            state,
            layout,
            &self.theme,
            screen_w,
            screen_h,
            self.scale_factor,
            cell_size,
        );
        let total_quad_count = quads.len() as u32;

        self.quad_pipeline
            .prepare(&self.device, &self.queue, screen_w, screen_h, &quads);
        self.text_renderer.prepare(
            &self.device,
            &self.queue,
            state,
            layout,
            &self.theme,
            screen_w,
            screen_h,
        );

        let frame = self.surface.get_current_texture()?;
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("daedalus-frame-encoder"),
            });

        let clear_color =
            rgba_to_wgpu_clear_color(self.theme.bg_window, self.config.format.is_srgb());

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("daedalus-main-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear_color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            // Pass 1: base colored quads (sidebar, panes, merged cell backgrounds, selection, cursor, borders)
            self.quad_pipeline
                .render_range(&mut render_pass, 0..base_quad_count);

            // Pass 2: base text (terminal rows + UI chrome)
            self.text_renderer.render_base(&mut render_pass);

            // Overlay pass (same render pass): active modal above base text
            if state.picker.open || state.worktree_modal.open {
                self.quad_pipeline
                    .render_range(&mut render_pass, base_quad_count..total_quad_count);
                self.text_renderer.render_modal(&mut render_pass);
            }
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();
        self.text_renderer.trim_atlas();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{LayoutInput, compute_layout};
    use crate::selection::{CellPos, Selection};
    use daedalus_proto::{
        Cell, Color, CursorState, GridRow, GridSnapshot, PaneId, PaneSummary, WorkspaceId,
        WorkspaceInfo,
    };
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn make_state_with_agent_row() -> (AppState, WorkspaceLayout) {
        let ws_id = WorkspaceId::new();
        let agent_id = PaneId::new();
        let ws = WorkspaceInfo {
            id: ws_id,
            name: "main".to_string(),
            root_dir: PathBuf::from("/tmp/repo"),
            slot_index: 0,
            injected_env: HashMap::new(),
            panes: vec![PaneSummary {
                id: agent_id,
                workspace_id: ws_id,
                role: PaneRole::Agent,
                status: PaneStatus::Idle,
                cols: 20,
                rows: 5,
                cwd: Some(PathBuf::from("/tmp/repo")),
                last_exit_code: None,
                has_failure_trace: false,
            }],
            port_offset: 0,
            compose_project_name: None,
            git_branch: None,
            bound_ports: Vec::new(),
            containers: Vec::new(),
            external_statuses: Vec::new(),
        };

        let mut state = AppState::new();
        state.connected = true;
        state.set_workspaces(vec![ws]);

        let red = Color::Rgb(200, 30, 30);
        let row = GridRow {
            row_index: 0,
            abs_row: 0,
            cells: vec![
                Cell {
                    text: "a".into(),
                    fg: Color::Default,
                    bg: red,
                    flags: CellFlags::UNDERLINE,
                    width: 1,
                },
                Cell {
                    text: "b".into(),
                    fg: Color::Default,
                    bg: red,
                    flags: CellFlags::empty(),
                    width: 1,
                },
                Cell {
                    text: "c".into(),
                    fg: Color::Default,
                    bg: Color::Default,
                    flags: CellFlags::STRIKETHROUGH,
                    width: 1,
                },
            ],
        };

        let pane = state.panes.get_mut(&agent_id).unwrap();
        pane.apply_snapshot(GridSnapshot {
            pane_id: agent_id,
            seq: 1,
            cols: 20,
            rows: 5,
            cursor: CursorState {
                col: 1,
                row: 0,
                visible: true,
            },
            rows_data: vec![row],
            scrollback_len: 0,
        });
        pane.selection = Some(Selection::new(CellPos::new(0, 0), CellPos::new(2, 0)));

        let layout = compute_layout(&LayoutInput {
            window_w: 1200.0,
            window_h: 800.0,
            scale_factor: 1.0,
            cell_w: 8.0,
            cell_h: 19.0,
            sidebar_visible: true,
            has_top_banner: false,
            zoomed: false,
            focused_role: PaneRole::Agent,
            runner_has_failure: false,
        });

        (state, layout)
    }

    #[test]
    fn collect_quads_merges_adjacent_cell_backgrounds_and_splits_modal_overlay() {
        let (mut state, layout) = make_state_with_agent_row();
        let theme = Theme::dark();

        let (quads, base_count) = collect_quads(&state, &layout, &theme, 1200.0, 800.0, 1.0, (8.0, 19.0));
        assert_eq!(base_count as usize, quads.len());

        // Merged red background quad for cols 0..2 should have width 16.0 (2 * cell_w)
        let red_rgba = Rgba::from_rgb_u8(200, 30, 30).to_array();
        let red_quads: Vec<&QuadInstance> = quads.iter().filter(|q| q.color == red_rgba).collect();
        assert_eq!(red_quads.len(), 1);
        assert!((red_quads[0].rect[2] - 16.0).abs() < 1e-4);

        // Selection quad for cols 0..=2 should have width 24.0 (3 * cell_w)
        let sel_quads: Vec<&QuadInstance> = quads
            .iter()
            .filter(|q| q.color == theme.selection_bg.to_array())
            .collect();
        assert_eq!(sel_quads.len(), 1);
        assert!((sel_quads[0].rect[2] - 24.0).abs() < 1e-4);

        // Opening the picker appends overlay quads strictly after base_quad_count
        state.picker.open = true;
        let (quads_with_modal, base_count_modal) =
            collect_quads(&state, &layout, &theme, 1200.0, 800.0, 1.0, (8.0, 19.0));
        assert_eq!(base_count_modal, base_count);
        assert!(quads_with_modal.len() > base_count_modal as usize);
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
    fn disconnected_error_toast_renders_error_banner_instead_of_amber_reconnect() {
        let (mut state, _) = make_state_with_agent_row();
        state.connected = false;
        state.show_toast("daemon handshake failed", true);

        let layout = compute_layout(&LayoutInput {
            window_w: 1200.0,
            window_h: 800.0,
            scale_factor: 1.0,
            cell_w: 8.0,
            cell_h: 19.0,
            sidebar_visible: true,
            has_top_banner: true,
            zoomed: false,
            focused_role: PaneRole::Agent,
            runner_has_failure: false,
        });
        let theme = Theme::dark();
        let banner_rect = layout.banner.expect("banner rect should exist");

        let (quads, _) = collect_quads(&state, &layout, &theme, 1200.0, 800.0, 1.0, (8.0, 19.0));
        let expected_err_bg = theme.status_err.with_alpha(0.26).to_array();
        let banner_quad = quads
            .iter()
            .find(|q| {
                (q.rect[0] - banner_rect.x).abs() < 1e-3
                    && (q.rect[1] - banner_rect.y).abs() < 1e-3
                    && (q.rect[2] - banner_rect.width).abs() < 1e-3
                    && (q.rect[3] - banner_rect.height).abs() < 1e-3
            })
            .expect("banner quad should be emitted");
        assert_eq!(banner_quad.color, expected_err_bg);

        let (bg, text_and_color) = banner_appearance(&state, &theme);
        assert_eq!(bg, theme.status_err.with_alpha(0.26));
        assert_eq!(
            text_and_color,
            Some((
                "Disconnected — daemon handshake failed".to_string(),
                theme.status_err
            ))
        );
    }

    #[test]
    fn collect_quads_renders_worktree_modal_overlay_and_sidebar_external_status_badges() {
        use daedalus_proto::{PortMapping, PortSource};

        let (mut state, layout) = make_state_with_agent_row();
        let theme = Theme::dark();

        {
            let ws = state.active_workspace_mut().unwrap();
            ws.slot_index = 1;
            ws.git_branch = Some("feat/auth".to_string());
            ws.bound_ports = vec![
                PortMapping {
                    host_port: 3100,
                    container_port: Some(3000),
                    service: Some("web".to_string()),
                    source: PortSource::Docker,
                },
                PortMapping {
                    host_port: 5532,
                    container_port: Some(5432),
                    service: Some("db".to_string()),
                    source: PortSource::Docker,
                },
            ];
            ws.containers = vec![
                ContainerHealth {
                    name: "app-wt1-web-1".to_string(),
                    service: Some("web".to_string()),
                    state: "running".to_string(),
                    health: Some("healthy".to_string()),
                    ports: vec![3100],
                },
                ContainerHealth {
                    name: "app-wt1-db-1".to_string(),
                    service: Some("db".to_string()),
                    state: "exited".to_string(),
                    health: Some("unhealthy".to_string()),
                    ports: vec![5532],
                },
            ];
            ws.external_statuses = vec![
                ExternalStatus {
                    source: "ignored-oldest".to_string(),
                    level: StatusLevel::Info,
                    label: "old".to_string(),
                    detail: None,
                    updated_at_ms: 1,
                },
                ExternalStatus {
                    source: "phrourion".to_string(),
                    level: StatusLevel::Warn,
                    label: "3 dirty".to_string(),
                    detail: None,
                    updated_at_ms: 2,
                },
                ExternalStatus {
                    source: "cassandra".to_string(),
                    level: StatusLevel::Error,
                    label: "retry".to_string(),
                    detail: None,
                    updated_at_ms: 3,
                },
            ];
        }

        let (quads, base_count) = collect_quads(&state, &layout, &theme, 1200.0, 800.0, 1.0, (8.0, 19.0));
        assert_eq!(base_count as usize, quads.len());

        let sidebar = layout.sidebar.unwrap();
        let row_rect = sidebar_workspace_rect(sidebar, 0, 1.0);
        let branch_rect = sidebar_branch_badge_rect(row_rect, "wt1 · feat/auth", 1.0);
        let ext_y = row_rect.y + 61.0;

        // Worktree slot branch badge quad is rendered with border_focused tint
        let info_bg = theme.border_focused.with_alpha(0.18).to_array();
        assert!(quads.iter().any(|q| {
            q.color == info_bg
                && (q.rect[0] - branch_rect.x).abs() < 1e-3
                && (q.rect[1] - branch_rect.y).abs() < 1e-3
        }));

        // Sidebar should render the 2 most recent external status badges (Warn + Error) on the badge row
        let warn_bg = theme.status_busy.with_alpha(0.18).to_array();
        let err_bg = theme.status_err.with_alpha(0.18).to_array();
        assert!(
            quads
                .iter()
                .any(|q| q.color == warn_bg && (q.rect[1] - ext_y).abs() < 1e-3)
        );
        assert!(
            quads
                .iter()
                .any(|q| q.color == err_bg && (q.rect[1] - ext_y).abs() < 1e-3)
        );
        // The oldest Info badge was truncated beyond the 2-badge cap
        assert!(
            !quads
                .iter()
                .any(|q| q.color == info_bg && (q.rect[1] - ext_y).abs() < 1e-3),
            "only 2 most recent external status badges should be drawn"
        );

        // Opening the worktree modal appends overlay quads strictly after base_quad_count
        state.worktree_modal.open();
        state.worktree_modal.branch_input = "feat/isolation".to_string();
        let (quads_with_modal, base_count_modal) =
            collect_quads(&state, &layout, &theme, 1200.0, 800.0, 1.0, (8.0, 19.0));
        assert_eq!(base_count_modal, base_count);
        assert!(quads_with_modal.len() > base_count_modal as usize);

        let modal = worktree_modal_rect(1200.0, 800.0, 1.0);
        let input = worktree_input_rect(modal, 1.0);
        assert!((modal.x + modal.width * 0.5 - 600.0).abs() < 1e-3);
        assert!((modal.y + modal.height * 0.5 - 400.0).abs() < 1e-3);
        assert!(input.x > modal.x && input.right() < modal.right());
        assert!(input.y > modal.y && input.bottom() < modal.bottom());

        assert!(quads_with_modal[base_count_modal as usize..].iter().any(|q| {
            q.color == theme.bg_window.to_array()
                && (q.rect[0] - input.x).abs() < 1e-3
                && (q.rect[1] - input.y).abs() < 1e-3
                && (q.rect[2] - input.width).abs() < 1e-3
                && (q.rect[3] - input.height).abs() < 1e-3
        }));

        // Verify port & docker formatting helpers
        let ws = state.active_workspace().unwrap();
        assert_eq!(
            format_ports_summary(&ws.bound_ports),
            ("● 2 ports: 3100, 5532".to_string(), true)
        );
        assert_eq!(
            format_docker_health(&ws.containers),
            Some(("1/2 up !".to_string(), false))
        );
    }
}
