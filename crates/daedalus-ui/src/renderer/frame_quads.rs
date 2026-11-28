use super::footer_widget::FooterWidget;
use super::{
    agent_tab_mark, banner_appearance, overlay_quads::collect_overlay_quads, pane,
    push_chrome_badge, push_control, push_rect_border, push_status_chip, quads::QuadInstance,
    sidebar, sidebar::SidebarCards,
};
use crate::layout::overlays::sanitize_scale;
use crate::layout::{Rect, WorkspaceLayout, sidebar_workspace_close_rect};
use crate::state::AppState;
use crate::theme::Theme;

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

pub(super) fn collect_prepared_quads(
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
        let settings = state.active_workspace_id.map(|_| {
            crate::layout::settings_button(toggle, cards.and_then(|cards| cards.list.arrows), s)
        });
        for button in [
            layout.layout_button,
            layout.run_tests_button,
            layout.launch_button,
            settings,
        ]
        .into_iter()
        .flatten()
        {
            let surface = Rect::new(
                button.x + 1. * s,
                button.y + 4. * s,
                (button.width - 2. * s).max(0.),
                (button.height - 8. * s).max(0.),
            );
            quads.push(QuadInstance::new(surface, theme.bg_sidebar).rounded(5. * s, 0., 0.));
        }
    }

    if let Some(bar) = layout.cargo_bar.or(layout.widget_bar) {
        quads.push(QuadInstance::new(bar, theme.bg_header));
        for (_, button) in layout.cargo_buttons(s) {
            quads.push(QuadInstance::new(button, theme.bg_sidebar).rounded(5. * s, 0., 0.));
        }
        for &(index, button) in &layout.widget_buttons {
            if let Some(widget) = FooterWidget::get(state, index) {
                widget.paint(&mut quads, button, layout.widget_row(index), theme, s);
            } else {
                quads.push(QuadInstance::new(button, theme.bg_sidebar).rounded(5. * s, 0., 0.));
            }
        }
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
            state.focused_pane_id().and_then(|id| {
                layout
                    .named_tabs
                    .iter()
                    .find(|tab| tab.kind.session() == Some(id))
                    .map(|tab| tab.rect)
            })
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

    if state.active_workspace_id.is_some()
        && let [agent, service, runner] = layout.panes.as_slice()
    {
        let agent = agent.outer;
        let service = service.outer;
        let runner = runner.outer;
        let length = 24. * s;
        let thickness = 2. * s;
        let mut horizontal_grip = |x: f32, y: f32| {
            quads.push(
                QuadInstance::from_xywh(
                    x - length / 2.,
                    y - thickness / 2.,
                    length,
                    thickness,
                    theme.text_muted,
                )
                .rounded(thickness / 2., 0., 0.),
            );
        };
        let right_center = service.x + service.width / 2.;
        if layout.stacked {
            horizontal_grip(right_center, service.y);
            horizontal_grip(right_center, runner.y);
        } else {
            horizontal_grip(right_center, (service.bottom() + runner.y) / 2.);
            quads.push(
                QuadInstance::from_xywh(
                    (agent.right() + service.x - thickness) / 2.,
                    agent.y + (agent.height - length) / 2.,
                    thickness,
                    length,
                    theme.text_muted,
                )
                .rounded(thickness / 2., 0., 0.),
            );
        }
    }

    for tab in &layout.named_tabs {
        if let Some((preset, bounds)) = agent_tab_mark(state, tab, s) {
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

pub(super) fn collect_sidebar_quads(
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
        if card.needs_reply {
            quads.push(QuadInstance::new(row_rect, theme.bg_header).rounded(8.0 * s, 0.0, 0.));
            quads.push(
                QuadInstance::new(
                    row_rect,
                    theme
                        .status_busy
                        .with_alpha(if is_active { 0.20 } else { 0.12 }),
                )
                .rounded(8.0 * s, 0.0, 0.),
            );
            quads.push(
                QuadInstance::new(row_rect, theme.status_busy.with_alpha(0.75)).rounded(
                    8.0 * s,
                    border_w,
                    0.,
                ),
            );
        } else if is_active {
            quads.push(QuadInstance::new(row_rect, theme.bg_header).rounded(8.0 * s, 0.0, 0.38));
        } else {
            quads.push(
                QuadInstance::new(row_rect, theme.bg_pane.with_alpha(0.55)).rounded(
                    8.0 * s,
                    0.0,
                    0.0,
                ),
            );
        }
        if is_active {
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
