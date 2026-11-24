mod pane;
mod sidebar;
use sidebar::SidebarCards;
pub mod quads;
pub mod text;

use std::collections::BTreeSet;
use std::sync::Arc;

use daedalus_proto::{
    AgentState, ContainerHealth, ExternalStatus, GitWorkspaceStatus, PaneRole, PaneStatus,
    PortMapping, StatusLevel,
};
use winit::window::Window;

use crate::layout::{PaneRect, Rect, WorkspaceLayout, sidebar_workspace_close_rect};
use crate::state::ConnectionState;
use crate::state::{AppState, WorkspaceView};
use crate::theme::{Rgba, Theme};

use self::quads::{QuadInstance, QuadPipeline, rgba_to_wgpu_clear_color};
use self::text::TextRenderer;

use crate::layout::overlays::{
    PICKER_HEADER_LOGICAL_HEIGHT, WORKTREE_HEADER_LOGICAL_HEIGHT, close_modal_dirty_banner_rect,
    close_modal_metrics, sanitize_scale, worktree_content_scale,
};
pub use crate::layout::overlays::{
    action_palette_layout, close_modal_cancel_rect, close_modal_compose_rect,
    close_modal_confirm_rect, close_modal_force_rect, close_modal_worktree_rect,
    close_workspace_modal_rect, picker_item_rect, picker_modal_rect, picker_row_stride,
    preset_picker_layout, search_bar_rect, search_controls, worktree_button_rects,
    worktree_input_rect, worktree_modal_rect,
};

/// Formats `(label, color)` for an [`AgentState`] badge.
pub fn agent_state_badge(state: AgentState, theme: &Theme) -> (&'static str, Rgba) {
    match state {
        AgentState::WaitingForApproval => ("Approval", theme.status_busy),
        AgentState::Thinking => ("Thinking", theme.border_focused),
        AgentState::Idle => ("Idle", theme.status_ok),
        AgentState::Error => ("Error", theme.status_err),
    }
}

/// Computes the Agent pane header state badge rectangle to the left of `action_pill`.
pub fn pane_agent_state_pill_rect(
    pane_rect: &PaneRect,
    label: &str,
    scale_factor: f32,
) -> Option<Rect> {
    if pane_rect.role != PaneRole::Agent {
        return None;
    }
    let s = sanitize_scale(scale_factor);
    let header = pane_rect.header;
    let right_edge = pane_rect
        .action_pill
        .map_or(header.right() - 6.0 * s, |p| p.x - 6.0 * s);
    let desired_w = ((label.chars().count() as f32) * 8.0 + 16.0) * s;
    let max_w = (right_edge - header.x - 24.0 * s).max(0.0);
    let pill_w = desired_w.min(max_w);
    let pill_h = (header.height - 6.0 * s).clamp(0.0, 20.0 * s);
    if pill_w >= 24.0 * s && pill_h > 0.0 {
        Some(Rect::new(
            right_edge - pill_w,
            header.y + (header.height - pill_h) * 0.5,
            pill_w,
            pill_h,
        ))
    } else {
        None
    }
}

/// Computes the compact Agent state badge rectangle on Line 2 right side of a sidebar workspace card.
pub fn sidebar_agent_state_pill_rect(row_rect: Rect, label: &str, scale_factor: f32) -> Rect {
    let s = sanitize_scale(scale_factor);
    let w = ((label.chars().count() as f32) * 8.0 + 16.0) * s;
    let max_w = (row_rect.width - 28.0 * s).max(0.0);
    let clamped_w = w.min(max_w);
    Rect::new(
        row_rect.right() - clamped_w - 10.0 * s,
        row_rect.y + 32.0 * s,
        clamped_w,
        20.0 * s,
    )
}

/// Formats compact git status indicators (`*`, `↑n`, `↓n`).
pub fn format_git_status_suffix(gs: &GitWorkspaceStatus) -> String {
    let mut parts = String::new();
    if gs.dirty {
        parts.push('*');
    }
    if gs.ahead > 0 {
        if !parts.is_empty() {
            parts.push(' ');
        }
        parts.push_str(&format!("↑{}", gs.ahead));
    }
    if gs.behind > 0 {
        if !parts.is_empty() {
            parts.push(' ');
        }
        parts.push_str(&format!("↓{}", gs.behind));
    }
    parts
}

/// Formats a compact uppercase source tag for external tool badges (`[PHR]`, `[CAS]`, etc.).
pub fn format_external_source_tag(source: &str) -> String {
    let trimmed = source.trim();
    match trimmed.to_ascii_lowercase().as_str() {
        "phrourion" => "[PHR]".to_string(),
        "cassandra" => "[CAS]".to_string(),
        _ => {
            let short: String = trimmed
                .chars()
                .filter(|c| !c.is_whitespace())
                .take(3)
                .flat_map(|c| c.to_uppercase())
                .collect();
            if short.is_empty() {
                "[EXT]".to_string()
            } else {
                format!("[{short}]")
            }
        }
    }
}

pub(super) fn active_external_detail_preview(ws: &WorkspaceView) -> Option<String> {
    ws.external_statuses.iter().rev().find_map(|ext| {
        let detail = ext
            .detail
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())?;
        let tag = format_external_source_tag(&ext.source);
        Some(format!("{tag} {detail}"))
    })
}

pub(super) fn workspace_branch_badge(ws: &WorkspaceView) -> Option<(String, bool)> {
    let is_wt = ws.slot_index > 0 || ws.git_status.is_some_and(|g| g.is_worktree);
    let mut label = match (&ws.git_branch, ws.slot_index > 0) {
        (Some(branch), true) => format!("wt{} · {branch}", ws.slot_index),
        (Some(branch), false) => branch.clone(),
        (None, true) => format!("wt{}", ws.slot_index),
        (None, false) => return None,
    };
    if let Some(ref gs) = ws.git_status {
        let suffix = format_git_status_suffix(gs);
        if !suffix.is_empty() {
            label.push(' ');
            label.push_str(&suffix);
        }
    }
    Some((label, is_wt))
}

pub(super) fn sidebar_branch_badge_rect_with_reserve(
    row_rect: Rect,
    label: &str,
    reserve_right: f32,
    scale_factor: f32,
) -> Rect {
    let s = sanitize_scale(scale_factor);
    let left_x = row_rect.x + 22.0 * s;
    let max_w = (row_rect.right() - left_x - 8.0 * s - reserve_right).max(0.0);
    let est_w = ((label.chars().count() as f32) * 7.0 + 12.0) * s;
    Rect::new(left_x, row_rect.y + 32.0 * s, est_w.min(max_w), 22.0 * s)
}

#[cfg(test)]
pub(super) fn sidebar_branch_badge_rect(row_rect: Rect, label: &str, scale_factor: f32) -> Rect {
    sidebar_branch_badge_rect_with_reserve(row_rect, label, 0.0, scale_factor)
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
        row_rect.y + 62.0 * s,
        w,
        15.0 * s,
    )
}

pub(super) fn visible_external_statuses(ws: &WorkspaceView) -> &[ExternalStatus] {
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
    let y = row_rect.y + 90.0 * s;
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
    quads.push(QuadInstance::from_xywh(
        rect.x, rect.y, rect.width, t, color,
    ));
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

/// Keep colored label ink on an opaque surface; reserve tint for the outline.
fn push_chrome_badge(
    quads: &mut Vec<QuadInstance>,
    rect: Rect,
    theme: &Theme,
    border_w: f32,
    accent: Rgba,
) {
    push_control(
        quads,
        rect,
        theme.bg_pane,
        accent,
        border_w,
        rect.height * 0.5,
    );
}

fn push_status_chip(quads: &mut Vec<QuadInstance>, rect: Rect, color: Rgba) {
    quads.push(QuadInstance::new(rect, color.with_alpha(0.15)).rounded(
        rect.height * 0.5,
        0.0,
        0.0,
    ));
}

fn push_control(
    quads: &mut Vec<QuadInstance>,
    rect: Rect,
    fill: Rgba,
    edge: Rgba,
    stroke: f32,
    radius: f32,
) {
    quads.push(QuadInstance::new(rect, fill).rounded(radius, 0.0, 0.04));
    quads.push(QuadInstance::new(rect, edge).rounded(radius, stroke, 0.0));
}

fn push_inner_focus(quads: &mut Vec<QuadInstance>, rect: Rect, stroke: f32, color: Rgba) {
    let inset = 2.0 * stroke;
    let ring = Rect::new(
        rect.x + inset,
        rect.y + inset,
        (rect.width - 2.0 * inset).max(0.0),
        (rect.height - 2.0 * inset).max(0.0),
    );
    quads.push(QuadInstance::new(ring, color).rounded(4.0 * stroke, stroke, 0.0));
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
    let message = if state.connection != ConnectionState::Connected {
        if let Some((msg, true, _)) = &state.toast {
            Some((msg.clone(), theme.status_err))
        } else {
            let label = if state.connection == ConnectionState::Connecting {
                "Connecting to daedalusd…"
            } else {
                "Reconnecting to daedalusd…"
            };
            Some((label.to_owned(), theme.status_busy))
        }
    } else {
        state.toast.as_ref().map(|(msg, error, _)| {
            (
                msg.clone(),
                if *error {
                    theme.status_err
                } else {
                    theme.status_ok
                },
            )
        })
    };
    (theme.bg_header, message)
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
    let scale = sanitize_scale(scale_factor);
    let cards = layout.sidebar.map(|sidebar| {
        SidebarCards::new(
            state,
            sidebar,
            scale,
            theme,
            layout.sidebar_content.as_ref(),
        )
    });
    collect_prepared_quads(
        state,
        layout,
        theme,
        (screen_w, screen_h),
        scale,
        cell_size,
        cards.as_ref(),
    )
}

fn collect_prepared_quads(
    state: &AppState,
    layout: &WorkspaceLayout,
    theme: &Theme,
    (screen_w, screen_h): (f32, f32),
    s: f32,
    cell_size: (f32, f32),
    cards: Option<&SidebarCards>,
) -> (Vec<QuadInstance>, u32) {
    let border_w = s.round().max(1.0);
    let mut quads = Vec::with_capacity(256);

    if let (Some(sidebar), Some(cards)) = (layout.sidebar, cards) {
        collect_sidebar_quads(&mut quads, state, sidebar, theme, s, border_w, cards);
    }

    if let Some(toggle) = layout.sidebar_toggle {
        quads.push(QuadInstance::new(toggle, theme.bg_header));
    }

    // 2. Top disconnect / toast banner.
    if let Some(banner) = layout.banner {
        let (banner_bg, message) = banner_appearance(state, theme);
        quads.push(QuadInstance::new(banner, banner_bg));
        quads.push(QuadInstance::from_xywh(
            banner.x,
            banner.bottom() - border_w,
            banner.width,
            border_w,
            message.map_or(theme.border_unfocused, |(_, color)| color),
        ));
    }

    // 3. Semantic panes (background, header, pills, merged cell bgs, selection, decorations, cursor, border).
    for pane_rect in &layout.panes {
        pane::collect_pane_quads(&mut quads, pane_rect, state, theme, s, border_w, cell_size);
    }

    if state.zoomed
        && let [pane] = layout.panes.as_slice()
    {
        let selected = if layout.named_tabs.is_empty() {
            crate::layout::session_tabs(pane, s)
                .into_iter()
                .find_map(|(role, rect)| (role == state.focused_role).then_some(rect))
        } else {
            layout
                .named_tabs
                .iter()
                .find_map(|(id, _, rect)| (Some(*id) == state.focused_pane_id()).then_some(*rect))
        };
        if let Some(tab) = selected {
            let surface = Rect::new(
                tab.x + s,
                tab.y + 2.0 * s,
                (tab.width - 2.0 * s).max(0.0),
                (tab.height - 4.0 * s).max(0.0),
            );
            quads.push(
                QuadInstance::new(surface, theme.text_primary.with_alpha(0.08)).rounded(
                    5.0 * s,
                    0.0,
                    0.0,
                ),
            );
            quads.push(
                QuadInstance::from_xywh(
                    surface.x + 3.0 * s,
                    tab.bottom() - 2.0 * s,
                    (surface.width - 6.0 * s).max(0.0),
                    2.0 * s,
                    theme.border_focused,
                )
                .rounded(s, 0.0, 0.0),
            );
        }
    }

    for (id, label, rect) in &layout.named_tabs {
        if let Some((preset, bounds)) = agent_tab_mark(state, *id, label, *rect, s) {
            quads.push(QuadInstance::agent_mark(bounds, preset, theme));
        }
    }
    for (_, rect) in &layout.insight_rows {
        let body = Rect::new(
            rect.x + 2. * s,
            rect.y + 2. * s,
            (rect.width - 4. * s).max(0.),
            (rect.height - 4. * s).max(0.),
        );
        quads.push(QuadInstance::new(body, theme.bg_sidebar).rounded(5. * s, 0., 0.));
    }
    let base_quad_count = quads.len() as u32;

    collect_overlay_quads(
        &mut quads,
        state,
        layout,
        theme,
        (screen_w, screen_h),
        s,
        border_w,
    );
    (quads, base_quad_count)
}

fn agent_tab_mark(
    state: &AppState,
    id: daedalus_proto::PaneId,
    label: &str,
    rect: Rect,
    scale: f32,
) -> Option<(crate::state::AgentPreset, Rect)> {
    let pane = state.panes.get(&id)?;
    if pane.summary.role != PaneRole::Agent
        || rect.width <= 60. * scale
        || matches!(label, "‹" | "›" | "‹ Prev" | "Next ›")
    {
        return None;
    }
    let size = 15. * scale;
    Some((
        pane.agent_preset
            .unwrap_or(crate::state::AgentPreset::Shell),
        Rect::new(
            rect.x + 6. * scale,
            rect.y + (rect.height - size) * 0.5,
            size,
            size,
        ),
    ))
}

fn collect_sidebar_quads(
    quads: &mut Vec<QuadInstance>,
    state: &AppState,
    sidebar: Rect,
    theme: &Theme,
    s: f32,
    border_w: f32,
    cards: &SidebarCards,
) {
    quads.push(QuadInstance::new(sidebar, theme.bg_sidebar));
    // Right divider border
    quads.push(QuadInstance::from_xywh(
        sidebar.right() - border_w,
        sidebar.y,
        border_w,
        sidebar.height,
        theme.border_unfocused,
    ));

    for (idx, card) in cards.indices.iter().copied().zip(&cards.cards) {
        let ws = &state.workspaces[idx];
        let row_rect = card.rect;
        let is_active = state.active_workspace_id == Some(ws.id);
        if is_active {
            quads.push(QuadInstance::new(row_rect, theme.bg_header).rounded(8.0 * s, 0.0, 0.38));
            // Left accent stripe
            quads.push(
                QuadInstance::from_xywh(
                    row_rect.x,
                    row_rect.y + 6.0 * s,
                    3.0 * s,
                    (row_rect.height - 12.0 * s).max(0.0),
                    theme.border_focused,
                )
                .rounded(1.5 * s, 0.0, 0.0),
            );
        } else {
            quads.push(
                QuadInstance::new(row_rect, theme.bg_pane.with_alpha(0.55)).rounded(
                    8.0 * s,
                    0.0,
                    0.0,
                ),
            );
        }

        // Agent status dot (highlighted in amber/error when agent_state demands attention)
        let dot_size = 7.0 * s;
        quads.push(
            QuadInstance::from_xywh(
                row_rect.x + 9.0 * s,
                row_rect.y + 11.0 * s,
                dot_size,
                dot_size,
                card.dot,
            )
            .rounded(dot_size * 0.5, 0.0, 0.0),
        );

        // Sidebar workspace `×` close button
        let close_rect = sidebar_workspace_close_rect(row_rect, s);
        quads.push(QuadInstance::new(
            close_rect,
            theme.bg_window.with_alpha(0.55),
        ));
        push_rect_border(
            quads,
            close_rect,
            border_w,
            theme.border_unfocused.with_alpha(0.75),
        );

        for badge in &card.badges {
            if badge.kind == sidebar::BadgeKind::FailedRun {
                quads.push(QuadInstance::new(badge.rect, theme.status_err).rounded(
                    5.0 * s,
                    0.0,
                    0.0,
                ));
            } else if badge.kind == sidebar::BadgeKind::Agent {
                push_status_chip(quads, badge.rect, badge.color);
            } else {
                push_chrome_badge(quads, badge.rect, theme, border_w, badge.border);
            }
        }
    }

    let new_btn = cards.list.new_workspace;
    if new_btn.y < sidebar.bottom() {
        push_control(
            quads,
            new_btn,
            theme.bg_header,
            theme.border_unfocused,
            border_w,
            6.0 * s,
        );
    }

    let new_wt_btn = cards.list.new_worktree;
    if new_wt_btn.y < sidebar.bottom() {
        push_control(
            quads,
            new_wt_btn,
            theme.bg_header,
            theme.border_unfocused,
            border_w,
            6.0 * s,
        );
    }
}

fn push_modal_frame(
    quads: &mut Vec<QuadInstance>,
    modal: Rect,
    screen: (f32, f32),
    theme: &Theme,
    border_w: f32,
    divider_y: f32,
) {
    quads.push(QuadInstance::from_xywh(
        0.0,
        0.0,
        screen.0,
        screen.1,
        Rgba::new(0.0, 0.0, 0.0, 0.55),
    ));
    quads.push(
        QuadInstance::new(modal, theme.picker_bg.with_alpha(1.0)).rounded(
            10.0 * border_w,
            0.0,
            0.12,
        ),
    );
    quads.push(QuadInstance::new(modal, theme.border_unfocused).rounded(
        10.0 * border_w,
        border_w,
        0.0,
    ));
    quads.push(QuadInstance::from_xywh(
        modal.x + border_w,
        modal.y + divider_y,
        (modal.width - 2.0 * border_w).max(0.0),
        border_w,
        theme.border_unfocused,
    ));
}

fn collect_overlay_quads(
    quads: &mut Vec<QuadInstance>,
    state: &AppState,
    layout: &WorkspaceLayout,
    theme: &Theme,
    screen: (f32, f32),
    s: f32,
    border_w: f32,
) {
    let (screen_w, screen_h) = screen;
    if let Some(choices) = &state.failure_recipients {
        let (modal, visible) = action_palette_layout(
            screen_w,
            screen_h,
            s,
            choices.agents.len(),
            choices.selected,
        );
        push_modal_frame(
            quads,
            modal,
            screen,
            theme,
            border_w,
            (PICKER_HEADER_LOGICAL_HEIGHT - 4.0) * s,
        );
        if visible.contains(&choices.selected) {
            push_chrome_badge(
                quads,
                picker_item_rect(modal, choices.selected - visible.start, s),
                theme,
                border_w,
                theme.border_focused,
            );
        }
        return;
    }
    if state.setup_review.open {
        let modal = crate::setup_review::modal_rect(screen_w, screen_h, s);
        push_modal_frame(quads, modal, screen, theme, border_w, modal.height * 0.14);
        for (control, rect) in state.setup_review.controls(modal) {
            if control == crate::setup_review::Control::Tool
                && !state.setup_review.busy
                && !state.setup_review.enabled(control)
            {
                continue;
            }
            let border = if state.setup_review.focus == control {
                theme.border_focused
            } else {
                theme.border_unfocused
            };
            let fill = if control == crate::setup_review::Control::Open
                && state.setup_review.enabled(control)
            {
                theme.border_focused
            } else {
                theme.bg_header
            };
            push_control(quads, rect, fill, border, border_w, 6.0 * s);
            if control == crate::setup_review::Control::Open && state.setup_review.focus == control
            {
                push_inner_focus(quads, rect, border_w, theme.bg_window);
            }
        }
        return;
    }
    if state.workspace_form.open {
        let form = &state.workspace_form;
        let modal = form.modal_rect(screen_w, screen_h, s);
        let heading = form
            .recent_heading(modal)
            .map_or(36. * s, |band| band.height * 0.9);
        push_modal_frame(quads, modal, screen, theme, border_w, heading);
        if form.recent_heading(modal).is_some() {
            // Separates the recent list from the new-project form below it.
            let area = form.form_area(modal);
            quads.push(QuadInstance::from_xywh(
                modal.x + border_w,
                area.y,
                (modal.width - 2.0 * border_w).max(0.0),
                border_w,
                theme.border_unfocused,
            ));
        }
        for (control, rect) in form.controls(modal) {
            let field = Rect::new(
                rect.x + 2. * s,
                rect.y + 2. * s,
                (rect.width - 4. * s).max(0.),
                (rect.height - 4. * s).max(0.),
            );
            let border = if state.workspace_form.focus == control {
                theme.border_focused
            } else {
                theme.border_unfocused
            };
            push_chrome_badge(quads, field, theme, border_w, border);
        }
        return;
    }
    if state.settings.open {
        push_modal_frame(
            quads,
            state.settings.modal_rect(screen_w, screen_h, s),
            screen,
            theme,
            border_w,
            46. * state
                .settings
                .content_scale(state.settings.modal_rect(screen_w, screen_h, s)),
        );
        let modal = state.settings.modal_rect(screen_w, screen_h, s);
        for (control, rect) in state.settings.controls(modal) {
            let field = Rect::new(
                rect.x + 2. * s,
                rect.y + 2. * s,
                (rect.width - 4. * s).max(0.),
                (rect.height - 4. * s).max(0.),
            );
            let border = if state.settings.focus == control {
                theme.border_focused
            } else {
                theme.border_unfocused
            };
            let fill =
                if control == crate::settings::Control::Save && state.settings.enabled(control) {
                    theme.border_focused
                } else if control.field_label().is_some() {
                    theme.bg_pane
                } else {
                    theme.bg_header
                };
            push_control(quads, field, fill, border, border_w, 6.0 * s);
            if control == crate::settings::Control::Save && state.settings.focus == control {
                push_inner_focus(quads, field, border_w, theme.bg_window);
            }
        }
        return;
    }
    // 4. Centered Agent preset picker modal overlay (rendered after base text).
    if state.picker.open || state.action_palette.open {
        let (modal, selected) = if state.action_palette.open {
            let idx = state.action_palette.selected_index;
            let (modal, visible) =
                action_palette_layout(screen_w, screen_h, s, state.active_actions().len(), idx);
            (modal, visible.contains(&idx).then(|| idx - visible.start))
        } else {
            let idx = state.picker.selected_index;
            let (modal, visible) = preset_picker_layout(screen_w, screen_h, s, idx);
            (modal, visible.contains(&idx).then(|| idx - visible.start))
        };
        push_modal_frame(
            quads,
            modal,
            screen,
            theme,
            border_w,
            (PICKER_HEADER_LOGICAL_HEIGHT - 4.0) * s,
        );

        // Selected row highlight
        if let Some(selected) = selected {
            let selected_rect = picker_item_rect(modal, selected, s);
            push_chrome_badge(quads, selected_rect, theme, border_w, theme.border_focused);
        }
    } else if state.worktree_modal.open {
        let modal = worktree_modal_rect(screen_w, screen_h, s);
        push_modal_frame(
            quads,
            modal,
            screen,
            theme,
            border_w,
            (WORKTREE_HEADER_LOGICAL_HEIGHT - 2.0) * worktree_content_scale(modal, s),
        );

        // Branch input box
        let input_rect = worktree_input_rect(modal, s);
        push_control(
            quads,
            input_rect,
            theme.bg_window,
            if state.worktree_modal.missing_branch() {
                theme.status_err
            } else {
                theme.border_focused
            },
            border_w,
            6.0 * s,
        );
        let (cancel, create) = worktree_button_rects(modal, s);
        push_control(
            quads,
            cancel,
            theme.bg_header,
            theme.border_unfocused,
            border_w,
            6.0 * s,
        );
        let can_create = state.can_create_worktree();
        push_control(
            quads,
            create,
            if can_create {
                theme.border_focused
            } else {
                theme.bg_header
            },
            if can_create {
                theme.border_focused
            } else {
                theme.border_unfocused
            },
            border_w,
            6.0 * s,
        );
    } else if state.close_modal.open {
        let modal = close_workspace_modal_rect(screen_w, screen_h, s);
        let metrics = close_modal_metrics(modal, s);
        push_modal_frame(
            quads,
            modal,
            screen,
            theme,
            border_w,
            (metrics.header_height - 2.0) * metrics.scale,
        );

        // Dirty worktree warning banner
        if state.close_modal.is_dirty {
            let warn_rect = close_modal_dirty_banner_rect(modal, s);
            push_chrome_badge(quads, warn_rect, theme, border_w, theme.status_busy);
        }

        // Interactive checkbox rows
        let mut draw_checkbox_row = |rect: Rect, checked: bool, accent: Rgba| {
            let border = if checked {
                accent
            } else {
                theme.border_unfocused
            };
            push_chrome_badge(quads, rect, theme, border_w, border);
        };

        let compose_rect = close_modal_compose_rect(modal, &state.close_modal, s);
        draw_checkbox_row(
            compose_rect,
            state.close_modal.compose_down,
            theme.border_focused,
        );

        if let Some(wt_rect) = close_modal_worktree_rect(modal, &state.close_modal, s) {
            draw_checkbox_row(
                wt_rect,
                state.close_modal.remove_worktree,
                theme.status_busy,
            );
        }

        if let Some(force_rect) = close_modal_force_rect(modal, &state.close_modal, s) {
            draw_checkbox_row(
                force_rect,
                state.close_modal.force_remove_worktree,
                theme.status_err,
            );
        }

        // Cancel & Confirm buttons
        let cancel_rect = close_modal_cancel_rect(modal, s);
        quads.push(QuadInstance::new(cancel_rect, theme.bg_header));
        push_rect_border(quads, cancel_rect, border_w, theme.border_unfocused);

        let confirm_rect = close_modal_confirm_rect(modal, s);
        let confirm_accent = if state.close_modal.remove_worktree {
            theme.status_err
        } else {
            theme.border_focused
        };
        push_chrome_badge(quads, confirm_rect, theme, border_w, confirm_accent);
    } else if let Some(ref search) = state.search
        && let Some(pane_rect) = layout
            .panes
            .iter()
            .find(|p| state.pane_id_for_role(p.role) == Some(search.pane_id))
            .or_else(|| layout.pane(state.focused_role))
    {
        let bar = search_bar_rect(pane_rect, s);
        quads.push(QuadInstance::new(bar, theme.picker_bg.with_alpha(1.0)));
        for (idx, rect) in search_controls(bar).into_iter().enumerate() {
            let border = if idx == 2 && search.case_sensitive {
                theme.border_focused
            } else {
                theme.border_unfocused
            };
            push_rect_border(quads, rect, border_w, border);
        }
        push_rect_border(quads, bar, border_w, theme.border_focused);
    }
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

/// Outcome when surface acquisition or presentation requires caller action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderError {
    /// Surface is lost, outdated, or suboptimal and should be reconfigured.
    Reconfigure,
    /// Acquiring the next surface texture timed out.
    Timeout,
    /// A wgpu surface validation error occurred.
    Validation,
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
    pub fn new(
        window: Arc<Window>,
        theme: Theme,
        font_size: f32,
        font_family: Option<&str>,
    ) -> Result<Self, String> {
        let size = window.inner_size();
        let width = size.width.max(1);
        let height = size.height.max(1);
        let scale_factor = sanitize_scale(window.scale_factor() as f32);

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..wgpu::InstanceDescriptor::new_without_display_handle()
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
                    apply_limit_buckets: false,
                })
                .await
                .map_err(|err| format!("no compatible GPU adapter found: {err}"))?;

            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("daedalus-ui-device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                    experimental_features: wgpu::ExperimentalFeatures::disabled(),
                    memory_hints: wgpu::MemoryHints::Performance,
                    trace: wgpu::Trace::Off,
                })
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
            color_space: wgpu::SurfaceColorSpace::Auto,
            width,
            height,
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let quad_pipeline = QuadPipeline::new(&device, format);
        let text_renderer = TextRenderer::new(
            &device,
            &queue,
            format,
            width,
            height,
            scale_factor,
            font_size,
            font_family,
        );

        Ok(Self {
            surface,
            device,
            queue,
            config,
            scale_factor,
            theme,
            quad_pipeline,
            text_renderer,
        })
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
    }

    pub fn font_size(&self) -> f32 {
        self.text_renderer.font_size()
    }

    pub fn set_font_size(&mut self, font_size: f32) -> bool {
        self.text_renderer.set_font_size(font_size)
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

    pub fn recap_targets(&self) -> &[(Rect, crate::recap::RecapEntry)] {
        if self.recap_visible() {
            &self.text_renderer.recap_targets
        } else {
            &[]
        }
    }

    pub fn recap_visible(&self) -> bool {
        self.text_renderer.recap_drawn && !self.text_renderer.has_modal_text
    }

    pub fn render(
        &mut self,
        state: &mut AppState,
        layout: &WorkspaceLayout,
    ) -> Result<bool, RenderError> {
        let screen_w = self.config.width as f32;
        let screen_h = self.config.height as f32;
        let cell_size = self.text_renderer.cell_size();

        let cards = layout.sidebar.map(|sidebar| {
            SidebarCards::new(
                state,
                sidebar,
                self.scale_factor,
                &self.theme,
                layout.sidebar_content.as_ref(),
            )
        });
        let (quads, base_quad_count) = collect_prepared_quads(
            state,
            layout,
            &self.theme,
            (screen_w, screen_h),
            self.scale_factor,
            cell_size,
            cards.as_ref(),
        );
        let total_quad_count = quads.len() as u32;

        self.quad_pipeline
            .prepare(&self.device, &self.queue, screen_w, screen_h, &quads);
        self.text_renderer.prepare_with_sidebar(
            &self.device,
            &self.queue,
            state,
            layout,
            &self.theme,
            screen_w,
            screen_h,
            cards.as_ref(),
        );

        let (frame, suboptimal) = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => (frame, false),
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => (frame, true),
            wgpu::CurrentSurfaceTexture::Occluded => return Ok(false),
            wgpu::CurrentSurfaceTexture::Timeout => return Err(RenderError::Timeout),
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                return Err(RenderError::Reconfigure);
            }
            wgpu::CurrentSurfaceTexture::Validation => return Err(RenderError::Validation),
        };
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
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear_color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            // Pass 1: base colored quads (sidebar, panes, merged cell backgrounds, selection, cursor, borders)
            self.quad_pipeline
                .render_range(&mut render_pass, 0..base_quad_count);

            // Pass 2: base text (terminal rows + UI chrome)
            self.text_renderer.render_base(&mut render_pass);

            // Overlay pass (same render pass): active modal or search bar above base text
            if total_quad_count > base_quad_count
                || state.action_palette.open
                || state.picker.open
                || state.worktree_modal.open
                || state.close_modal.open
                || state.search.is_some()
            {
                self.quad_pipeline
                    .render_range(&mut render_pass, base_quad_count..total_quad_count);
                self.text_renderer.render_modal(&mut render_pass);
            }
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        self.queue.present(frame);
        self.text_renderer.trim_atlas();
        if suboptimal {
            return Err(RenderError::Reconfigure);
        }
        Ok(true)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::layout::{LayoutInput, compute_layout, sidebar_workspace_rect};
    use crate::selection::{CellPos, Selection};
    use daedalus_proto::{
        Cell, CellFlags, Color, CursorState, GridRow, GridSnapshot, PaneId, PaneSummary,
        WorkspaceId, WorkspaceInfo,
    };
    use std::collections::HashMap;
    use std::path::PathBuf;

    pub(crate) fn make_state_with_agent_row() -> (AppState, WorkspaceLayout) {
        let ws_id = WorkspaceId::new();
        let agent_id = PaneId::new();
        let ws = WorkspaceInfo {
            project: Default::default(),
            setup: None,
            default_panes_pending: None,
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
                launch_preset: None,
                agent_state: None,
            }],
            port_offset: 0,
            compose_project_name: None,
            git_branch: None,
            git_status: None,
            bound_ports: Vec::new(),
            containers: Vec::new(),
            external_statuses: Vec::new(),
            manifest: None,
        };

        let mut state = AppState::new();
        state.connection = ConnectionState::Connected;
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
            insight: None,
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
            history_generation: 0,
            scrollback_len: 0,
        });
        pane.selection = Some(Selection::new(CellPos::new(0, 0), CellPos::new(2, 0)));

        state.zoomed = false;
        let layout = compute_layout(&LayoutInput {
            window_w: 1200.0,
            window_h: 800.0,
            scale_factor: 1.0,
            cell_w: 8.0,
            cell_h: 19.0,
            sidebar_override: Some(true),
            has_top_banner: false,
            zoomed: false,
            focused_role: PaneRole::Agent,
            runner_has_failure: false,
        });

        (state, layout)
    }

    #[test]
    fn exited_terminals_keep_their_name_and_output_and_hide_the_input_cursor() {
        use crate::state::terminal_title;
        let (mut state, layout) = make_state_with_agent_row();
        let pane_id = state.focused_pane_id().unwrap();
        let theme = Theme::dark();
        let has_cursor = |state: &AppState| {
            let (quads, _) = collect_quads(state, &layout, &theme, 1200., 800., 1., (8., 19.));
            quads
                .iter()
                .any(|q| q.color == theme.cursor_bg.with_alpha(0.65).to_array())
        };
        assert!(has_cursor(&state));
        assert_eq!(
            terminal_title("Reviewer".into(), PaneStatus::Idle),
            "Reviewer"
        );
        let output = state.panes[&pane_id].viewport_rows.clone();
        for (code, title) in [
            (Some(0), "Exited 0 · Reviewer"),
            (Some(17), "Exited 17 · Reviewer"),
            (None, "Exited · Reviewer"),
        ] {
            state.apply_server_event(daedalus_proto::ServerEvent::PaneExited {
                pane_id,
                exit_code: code,
            });
            assert_eq!(
                terminal_title("Reviewer".into(), state.panes[&pane_id].summary.status),
                title
            );
            assert!(!has_cursor(&state));
            assert_eq!(state.panes[&pane_id].viewport_rows, output);
        }
    }

    #[test]
    fn collect_quads_merges_adjacent_cell_backgrounds_and_splits_modal_overlay() {
        let (mut state, layout) = make_state_with_agent_row();
        let pane_id = state.pane_id_for_role(PaneRole::Agent).unwrap();
        let row = &mut state.panes.get_mut(&pane_id).unwrap().viewport_rows[0];
        row.cells[0].text = "界".into();
        row.cells[0].width = 2;
        row.cells.insert(
            1,
            Cell {
                text: String::new(),
                width: 0,
                ..Cell::default()
            },
        );
        let theme = Theme::dark();

        let (quads, base_count) =
            collect_quads(&state, &layout, &theme, 1200.0, 800.0, 1.0, (8.0, 19.0));
        assert_eq!(base_count as usize, quads.len());

        // A wide red glyph and its narrow red neighbor form a single three-cell run.
        let red_rgba = Rgba::from_rgb_u8(200, 30, 30).to_array();
        let red_quads: Vec<&QuadInstance> = quads.iter().filter(|q| q.color == red_rgba).collect();
        assert_eq!(red_quads.len(), 1);
        assert!((red_quads[0].rect[2] - 24.0).abs() < 1e-4);

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
    fn search_overlay_stays_inside_a_short_narrow_pane() {
        let (_, layout) = make_state_with_agent_row();
        let mut pane = *layout.pane(PaneRole::Runner).unwrap();
        pane.outer = Rect::new(240.0, 200.0, 80.0, 68.0);
        pane.header = Rect::new(240.0, 200.0, 80.0, 52.0);
        let bar = search_bar_rect(&pane, 2.0);
        assert!(bar.width > 0.0 && bar.height > 0.0);
        assert!(bar.x >= pane.outer.x && bar.right() <= pane.outer.right());
        assert!(bar.y >= pane.header.bottom() && bar.bottom() <= pane.outer.bottom());
    }

    #[test]
    fn connection_banner_distinguishes_startup_from_a_lost_session() {
        let theme = Theme::dark();
        let mut state = AppState::new();
        assert_eq!(
            banner_appearance(&state, &theme).1.unwrap().0,
            "Connecting to daedalusd…"
        );
        state.connection = ConnectionState::Connected;
        assert!(banner_appearance(&state, &theme).1.is_none());
        state.connection = ConnectionState::Reconnecting;
        assert_eq!(
            banner_appearance(&state, &theme).1.unwrap().0,
            "Reconnecting to daedalusd…"
        );
    }

    #[test]
    fn disconnected_error_toast_renders_error_banner_instead_of_amber_reconnect() {
        let (mut state, _) = make_state_with_agent_row();
        state.connection = ConnectionState::Reconnecting;
        state.show_toast("daemon handshake failed", true);

        let layout = compute_layout(&LayoutInput {
            window_w: 1200.0,
            window_h: 800.0,
            scale_factor: 1.0,
            cell_w: 8.0,
            cell_h: 19.0,
            sidebar_override: Some(true),
            has_top_banner: true,
            zoomed: false,
            focused_role: PaneRole::Agent,
            runner_has_failure: false,
        });
        let theme = Theme::dark();
        let banner_rect = layout.banner.expect("banner rect should exist");

        let (quads, _) = collect_quads(&state, &layout, &theme, 1200.0, 800.0, 1.0, (8.0, 19.0));
        let expected_err_bg = theme.bg_header.to_array();
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
        assert_eq!(bg, theme.bg_header);
        assert!(quads.iter().any(|quad| quad.rect
            == [
                banner_rect.x,
                banner_rect.bottom() - 1.0,
                banner_rect.width,
                1.0
            ]
            && quad.color == theme.status_err.to_array()));
        assert_eq!(
            text_and_color,
            Some(("daemon handshake failed".to_string(), theme.status_err))
        );
    }

    #[test]
    fn worktree_create_outline_tracks_the_available_pointer_action() {
        let (mut state, layout) = make_state_with_agent_row();
        let theme = Theme::dark();
        state.worktree_modal.open();
        let (_, create) = worktree_button_rects(worktree_modal_rect(1200.0, 800.0, 1.0), 1.0);
        for (connection, branch, expected) in [
            (ConnectionState::Connected, "", theme.border_unfocused),
            (
                ConnectionState::Reconnecting,
                "draft",
                theme.border_unfocused,
            ),
            (ConnectionState::Connected, "draft", theme.border_focused),
        ] {
            state.connection = connection;
            state.worktree_modal.branch_input = branch.into();
            let (quads, base) =
                collect_quads(&state, &layout, &theme, 1200.0, 800.0, 1.0, (8.0, 19.0));
            assert!(quads[base as usize..].iter().any(|quad| quad.rect
                == [create.x, create.y, create.width, create.height]
                && quad.style[1] == 1.0
                && quad.color == expected.to_array()));
        }
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

        let (quads, base_count) =
            collect_quads(&state, &layout, &theme, 1200.0, 800.0, 1.0, (8.0, 19.0));
        assert_eq!(base_count as usize, quads.len());

        let sidebar = layout.sidebar.unwrap();
        let row_rect = sidebar_workspace_rect(sidebar, 0, 1.0);
        let branch_rect = sidebar_branch_badge_rect(row_rect, "wt1 · feat/auth", 1.0);
        let ext_y = row_rect.y + 90.0;

        // Worktree slot uses the same focused outline as the Info badge
        let info_border = theme.border_focused.with_alpha(0.65).to_array();
        assert!(quads.iter().any(|q| {
            q.color == info_border
                && (q.rect[0] - branch_rect.x).abs() < 1e-3
                && (q.rect[1] - branch_rect.y).abs() < 1e-3
        }));

        // Sidebar should render the 2 most recent external status badges (Warn + Error) on the badge row
        let warn_border = theme.status_busy.with_alpha(0.65).to_array();
        let err_border = theme.status_err.with_alpha(0.65).to_array();
        assert!(
            quads
                .iter()
                .any(|q| q.color == warn_border && (q.rect[1] - ext_y).abs() < 1e-3)
        );
        assert!(
            quads
                .iter()
                .any(|q| q.color == err_border && (q.rect[1] - ext_y).abs() < 1e-3)
        );
        // The oldest Info badge was truncated beyond the 2-badge cap
        assert!(
            !quads
                .iter()
                .any(|q| q.color == info_border && (q.rect[1] - ext_y).abs() < 1e-3),
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
        // Terminal glyphs behind the dialog must not bleed through its labels.
        assert!(
            quads_with_modal[base_count_modal as usize..]
                .iter()
                .any(|q| q.rect == [modal.x, modal.y, modal.width, modal.height]
                    && q.color == theme.picker_bg.with_alpha(1.0).to_array())
        );
        let input = worktree_input_rect(modal, 1.0);
        assert!((modal.x + modal.width * 0.5 - 600.0).abs() < 1e-3);
        assert!((modal.y + modal.height * 0.5 - 400.0).abs() < 1e-3);
        assert!(input.x > modal.x && input.right() < modal.right());
        assert!(input.y > modal.y && input.bottom() < modal.bottom());

        assert!(
            quads_with_modal[base_count_modal as usize..]
                .iter()
                .any(|q| {
                    q.color == theme.bg_window.to_array()
                        && (q.rect[0] - input.x).abs() < 1e-3
                        && (q.rect[1] - input.y).abs() < 1e-3
                        && (q.rect[2] - input.width).abs() < 1e-3
                        && (q.rect[3] - input.height).abs() < 1e-3
                })
        );

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

    #[test]
    fn collect_quads_renders_search_match_highlights_and_overlay_bar() {
        use crate::state::SearchState;
        use daedalus_proto::SearchMatch;

        let (mut state, layout) = make_state_with_agent_row();
        let pane_id = state.pane_id_for_role(PaneRole::Agent).unwrap();
        let theme = Theme::from_preset(crate::theme::ThemeName::SolarizedLight);

        let pane = state.panes.get_mut(&pane_id).unwrap();
        let mut second_row = pane.viewport_rows[0].clone();
        second_row.abs_row = 93;
        second_row.row_index = 1;
        pane.viewport_rows.push(second_row);

        let mut hits: Vec<SearchMatch> = (100..2097)
            .map(|abs_row| SearchMatch {
                abs_row,
                start_col: 0,
                end_col: 1,
            })
            .collect();
        // Provide matches out of screen order so grouping must preserve the original active index.
        hits.extend([
            SearchMatch {
                abs_row: 93,
                start_col: 0,
                end_col: 2,
            },
            SearchMatch {
                abs_row: 0,
                start_col: 2,
                end_col: 3,
            },
            SearchMatch {
                abs_row: 0,
                start_col: 0,
                end_col: 1,
            },
        ]);
        let mut search = SearchState::new(pane_id);
        search.query = "ab".to_string();
        search.set_results(0, hits);
        search.current_index = 1997;
        state.search = Some(search);

        let (quads, base_count) =
            collect_quads(&state, &layout, &theme, 1200.0, 800.0, 1.0, (8.0, 19.0));

        // Base quads contain active + inactive match highlights plus pane background in light theme
        let active_fill = theme.status_busy.with_alpha(0.55).to_array();
        let inactive_fill = theme.status_busy.with_alpha(0.24).to_array();
        let vp = layout.pane(PaneRole::Agent).unwrap().viewport;
        let fills: Vec<_> = quads[..base_count as usize]
            .iter()
            .filter(|q| q.color == active_fill || q.color == inactive_fill)
            .map(|q| (q.rect, q.color))
            .collect();
        assert_eq!(
            fills,
            vec![
                ([vp.x + 16.0, vp.y, 8.0, 19.0], inactive_fill),
                ([vp.x, vp.y, 8.0, 19.0], inactive_fill),
                ([vp.x, vp.y + 19.0, 16.0, 19.0], active_fill),
            ]
        );
        assert!(
            quads[..base_count as usize]
                .iter()
                .any(|q| q.color == theme.bg_pane.to_array())
        );

        // An opaque search surface keeps terminal text from showing through the query.
        let bar = search_bar_rect(layout.pane(PaneRole::Agent).unwrap(), 1.0);
        assert!(quads[base_count as usize..].iter().any(|q| {
            q.color == theme.picker_bg.with_alpha(1.0).to_array()
                && (q.rect[0] - bar.x).abs() < 1e-3
                && (q.rect[1] - bar.y).abs() < 1e-3
        }));
    }

    #[test]
    fn agent_state_git_status_and_external_source_tag_helpers() {
        let theme = Theme::dark();
        assert_eq!(
            agent_state_badge(AgentState::WaitingForApproval, &theme),
            ("Approval", theme.status_busy)
        );
        assert_eq!(
            agent_state_badge(AgentState::Thinking, &theme),
            ("Thinking", theme.border_focused)
        );
        assert_eq!(
            agent_state_badge(AgentState::Idle, &theme),
            ("Idle", theme.status_ok)
        );
        assert_eq!(
            agent_state_badge(AgentState::Error, &theme),
            ("Error", theme.status_err)
        );

        assert_eq!(format_external_source_tag("phrourion"), "[PHR]");
        assert_eq!(format_external_source_tag("Phrourion"), "[PHR]");
        assert_eq!(format_external_source_tag("cassandra"), "[CAS]");
        assert_eq!(format_external_source_tag("custom-ci"), "[CUS]");
        assert_eq!(format_external_source_tag("ci"), "[CI]");
        assert_eq!(format_external_source_tag("  "), "[EXT]");

        assert_eq!(
            format_git_status_suffix(&GitWorkspaceStatus {
                is_worktree: true,
                dirty: true,
                ahead: 2,
                behind: 1,
            }),
            "* ↑2 ↓1"
        );
        assert_eq!(
            format_git_status_suffix(&GitWorkspaceStatus {
                is_worktree: false,
                dirty: false,
                ahead: 3,
                behind: 0,
            }),
            "↑3"
        );
        assert_eq!(format_git_status_suffix(&GitWorkspaceStatus::default()), "");
    }

    #[test]
    fn collect_quads_renders_agent_badges_and_close_workspace_modal() {
        let (mut state, layout) = make_state_with_agent_row();
        let theme = Theme::dark();
        let ws_id = state.active_workspace_id.unwrap();
        let agent_id = state.pane_id_for_role(PaneRole::Agent).unwrap();
        state.apply_agent_state(ws_id, agent_id, AgentState::WaitingForApproval);
        {
            let ws = state.active_workspace_mut().unwrap();
            ws.slot_index = 1;
            ws.compose_project_name = Some("daedalus-wt1".to_string());
            ws.git_branch = Some("feat/agent-badges".to_string());
            ws.git_status = Some(GitWorkspaceStatus {
                is_worktree: true,
                dirty: true,
                ahead: 1,
                behind: 0,
            });
        }

        let (quads, base_count) =
            collect_quads(&state, &layout, &theme, 1200.0, 800.0, 1.0, (8.0, 19.0));
        assert_eq!(base_count as usize, quads.len());

        let sidebar = sidebar_workspace_rect(layout.sidebar.unwrap(), 0, 1.0);
        let mut sidebar_pill = sidebar_agent_state_pill_rect(sidebar, "Approval", 1.0);
        sidebar_pill.y = sidebar.y + 62.0;
        let header_pill =
            pane_agent_state_pill_rect(layout.pane(PaneRole::Agent).unwrap(), "Approval", 1.0)
                .unwrap();
        for rect in [sidebar_pill, header_pill] {
            assert!(quads.iter().any(|quad| quad.rect
                == [rect.x, rect.y, rect.width, rect.height]
                && quad.color == theme.status_busy.with_alpha(0.15).to_array()));
        }

        // Open close modal with worktree + dirty + remove_worktree enabled
        assert!(state.open_close_modal_for_workspace(ws_id));
        state.close_modal.toggle_worktree();
        let (quads_modal, base_modal) =
            collect_quads(&state, &layout, &theme, 1200.0, 800.0, 1.0, (8.0, 19.0));
        assert_eq!(base_modal, base_count);
        assert!(quads_modal.len() > base_modal as usize);

        let modal = close_workspace_modal_rect(1200.0, 800.0, 1.0);
        let warning = close_modal_dirty_banner_rect(modal, 1.0);
        assert!(quads_modal.iter().any(|quad| quad.rect
            == [warning.x, warning.y, warning.width, warning.height]
            && quad.color == theme.bg_pane.to_array()));
        let compose = close_modal_compose_rect(modal, &state.close_modal, 1.0);
        let worktree = close_modal_worktree_rect(modal, &state.close_modal, 1.0).unwrap();
        let force = close_modal_force_rect(modal, &state.close_modal, 1.0).unwrap();
        let cancel = close_modal_cancel_rect(modal, 1.0);
        let confirm = close_modal_confirm_rect(modal, 1.0);

        for r in [compose, worktree, force, cancel, confirm] {
            assert!(r.x >= modal.x && r.right() <= modal.right());
            assert!(r.y >= modal.y && r.bottom() <= modal.bottom());
        }
        assert!(compose.bottom() <= worktree.y);
        assert!(worktree.bottom() <= force.y);
        assert!(force.bottom() <= cancel.y);
        assert!(cancel.right() <= confirm.x);
    }

    #[test]
    fn setup_forms_outline_the_control_with_keyboard_focus() {
        let (mut state, layout) = make_state_with_agent_row();
        let (w, h) = (1200., 800.);
        let theme = Theme::dark();
        let assert_focus = |state: &AppState, controls: Vec<(bool, Rect)>, inset: f32| {
            let (quads, base) = collect_quads(state, &layout, &theme, w, h, 1., (8., 19.));
            assert!(controls.iter().any(|(focused, _)| *focused));
            for (focused, rect) in controls {
                let expected = if focused {
                    theme.border_focused
                } else {
                    theme.border_unfocused
                };
                let outline = [
                    rect.x + inset,
                    rect.y + inset,
                    rect.width - 2. * inset,
                    rect.height - 2. * inset,
                ];
                assert!(quads[base as usize..].iter().any(|q| q.rect == outline
                    && q.style[1] == 1.0
                    && q.color == expected.to_array()));
            }
        };
        state.workspace_form.begin(std::path::Path::new("/repo"));
        state.workspace_form.focus = crate::workspace_form::Control::Folder;
        assert_focus(
            &state,
            state
                .workspace_form
                .controls(state.workspace_form.modal_rect(w, h, 1.))
                .into_iter()
                .map(|(c, r)| (c == state.workspace_form.focus, r))
                .collect(),
            2.,
        );
        state.workspace_form.open = false;
        state.setup_review.begin(
            "demo".into(),
            "/repo".into(),
            daedalus_proto::SetupInspection {
                commands: vec![],
                available: vec!["sh".into()],
                host: "local".into(),
                platform: "linux".into(),
                dependency_install: None,
            },
        );
        state
            .setup_review
            .set_focus(crate::setup_review::Control::Command(1));
        assert_focus(
            &state,
            state
                .setup_review
                .controls(crate::setup_review::modal_rect(w, h, 1.))
                .into_iter()
                .filter(|(c, _)| {
                    *c != crate::setup_review::Control::Tool || state.setup_review.enabled(*c)
                })
                .map(|(c, r)| (c == state.setup_review.focus, r))
                .collect(),
            0.,
        );
        state.setup_review.open = false;
        state.settings.begin(vec![]);
        state.settings.activate(crate::settings::Control::Claude);
        state
            .settings
            .set_focus(crate::settings::Control::Executable);
        assert_focus(
            &state,
            state
                .settings
                .controls(state.settings.modal_rect(w, h, 1.))
                .map(|(c, r)| (c == state.settings.focus, r))
                .collect(),
            2.,
        );
    }
}
