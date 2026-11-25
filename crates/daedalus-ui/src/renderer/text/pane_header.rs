use super::{
    AppState, CHROME_BADGE_FONT_SIZE_LOGICAL, CHROME_BADGE_LINE_HEIGHT_LOGICAL,
    CHROME_FONT_SIZE_LOGICAL, CHROME_LINE_HEIGHT_LOGICAL, CHROME_SUB_FONT_SIZE_LOGICAL,
    CHROME_SUB_LINE_HEIGHT_LOGICAL, ChromeLineSpec, Rect, TextRenderer, Theme, WorkspaceLayout,
};
use crate::layout::{NamedTab, PaneRect, session_tabs, tab_label_padding};
use crate::renderer::{agent_state_badge, agent_tab_mark, pane_agent_state_pill_rect};
use crate::state::PaneViewState;
use crate::theme::Rgba;
use daedalus_proto::{PaneRole, PaneStatus};

impl TextRenderer {
    pub(super) fn build_pane_header_text(
        &mut self,
        pane_rect: &PaneRect,
        state: &AppState,
        theme: &Theme,
        layout: &WorkspaceLayout,
    ) {
        let s = self.scale_factor;
        let header = pane_rect.header;
        let is_focused = pane_rect.role == state.focused_role;
        let pane_state = state.pane_for_role(pane_rect.role);
        let title = pane_title(state, pane_rect.role, pane_state);
        let agent_badge = pane_activity_badge(state, pane_rect, pane_state, theme, s);
        let text_left = self.draw_session_tabs(pane_rect, state, theme, layout);
        let default_right = pane_rect
            .action_pill
            .map_or(header.right() - 8.0 * s, |pill| pill.x - 8.0 * s);
        let text_right = agent_badge.map_or(default_right, |(rect, _, _)| rect.x - 8.0 * s);
        let text_w = (text_right - text_left).max(1.0);
        let title_bounds = Rect::new(text_left, header.y, text_w, header.height);

        self.push_chrome_line(ChromeLineSpec {
            text: &title,
            left: text_left,
            top: header.y + (header.height - CHROME_LINE_HEIGHT_LOGICAL * s) * 0.5,
            max_w: text_w,
            bounds: title_bounds,
            font_size_logical: CHROME_FONT_SIZE_LOGICAL,
            line_height_logical: CHROME_LINE_HEIGHT_LOGICAL,
            color: if is_focused {
                theme.text_primary
            } else {
                theme.text_muted
            },
            bold: is_focused,
        });

        if let Some((pill, label, color)) = agent_badge {
            self.draw_activity_badge(pill, label, color);
        }
        self.draw_header_action(pane_rect, pane_state, theme);
    }

    fn draw_session_tabs(
        &mut self,
        pane_rect: &PaneRect,
        state: &AppState,
        theme: &Theme,
        layout: &WorkspaceLayout,
    ) -> f32 {
        let s = self.scale_factor;
        if let Some(last) = layout.named_tabs.last() {
            for tab in &layout.named_tabs {
                self.draw_named_tab(tab, state, theme, layout);
            }
            return last.rect.right() + 4. * s;
        }
        if state.zoomed {
            let tabs = session_tabs(pane_rect, s);
            for (role, rect) in tabs {
                let padding = tab_label_padding(rect, s);
                let label = match role {
                    PaneRole::Agent => "Agent",
                    PaneRole::Service => "Service",
                    PaneRole::Runner => "Runner",
                };
                self.push_chrome_line(ChromeLineSpec {
                    text: label,
                    left: rect.x + padding,
                    top: rect.y + (rect.height - CHROME_LINE_HEIGHT_LOGICAL * s) * 0.5,
                    max_w: (rect.width - 2. * padding).max(1.),
                    bounds: rect,
                    font_size_logical: CHROME_FONT_SIZE_LOGICAL,
                    line_height_logical: CHROME_LINE_HEIGHT_LOGICAL,
                    color: if role == state.focused_role {
                        theme.border_focused
                    } else {
                        theme.text_muted
                    },
                    bold: role == state.focused_role,
                });
            }
            tabs[2].1.right() + 4. * s
        } else {
            pane_rect.header.x + 22.0 * s
        }
    }

    fn draw_named_tab(
        &mut self,
        tab: &NamedTab,
        state: &AppState,
        theme: &Theme,
        layout: &WorkspaceLayout,
    ) {
        let s = self.scale_factor;
        let rect = tab.rect;
        let session = tab.kind.session();
        let active = session.is_some_and(|id| Some(id) == state.focused_pane_id());
        let close = layout.close_tab.filter(|(id, _)| Some(*id) == session);
        if let Some((_, button)) = close {
            self.push_chrome_button(ChromeLineSpec {
                text: "×",
                left: button.x,
                top: button.y + (button.height - CHROME_LINE_HEIGHT_LOGICAL * s) * 0.5,
                max_w: button.width,
                bounds: button,
                font_size_logical: CHROME_FONT_SIZE_LOGICAL,
                line_height_logical: CHROME_LINE_HEIGHT_LOGICAL,
                color: theme.text_muted,
                bold: false,
            });
        }

        let has_mark = agent_tab_mark(state, tab, s).is_some();
        let padding = tab_label_padding(rect, s);
        let mut inset = if has_mark { 22. * s } else { 0. };
        if let Some(pane) = session.and_then(|id| state.panes.get(&id))
            && (has_mark || pane.unread_attention)
        {
            if let Some(indicator) = pane.tab_indicator(state.reduced_motion) {
                self.push_chrome_line(ChromeLineSpec {
                    text: &indicator.to_string(),
                    left: rect.x + padding + inset,
                    top: rect.y + (rect.height - CHROME_LINE_HEIGHT_LOGICAL * s) * 0.5,
                    max_w: 14. * s,
                    bounds: rect,
                    font_size_logical: CHROME_FONT_SIZE_LOGICAL,
                    line_height_logical: CHROME_LINE_HEIGHT_LOGICAL,
                    color: theme.border_focused,
                    bold: true,
                });
            }
            inset += 14. * s;
        }
        let max_w =
            (rect.width - inset - close.map_or(0., |(_, button)| button.width) - 2. * padding)
                .max(1.);
        let color = if state.panes.get(&tab.kind.target()).is_some_and(|pane| {
            matches!(pane.summary.status, PaneStatus::Exited { code: Some(code) } if code != 0)
        }) {
            theme.status_err
        } else if active {
            theme.border_focused
        } else {
            theme.text_muted
        };
        self.push_chrome_line(ChromeLineSpec {
            text: &tab.label,
            left: rect.x + inset + padding,
            top: rect.y + (rect.height - CHROME_LINE_HEIGHT_LOGICAL * s) * 0.5,
            max_w,
            bounds: rect,
            font_size_logical: CHROME_FONT_SIZE_LOGICAL,
            line_height_logical: CHROME_LINE_HEIGHT_LOGICAL,
            color,
            bold: active,
        });
    }

    fn draw_activity_badge(&mut self, pill: Rect, label: &str, color: Rgba) {
        let s = self.scale_factor;
        self.push_chrome_button(ChromeLineSpec {
            text: label,
            left: pill.x + 6.0 * s,
            top: pill.y + (pill.height - CHROME_BADGE_LINE_HEIGHT_LOGICAL * s) * 0.5,
            max_w: (pill.width - 10.0 * s).max(1.0),
            bounds: pill,
            font_size_logical: CHROME_BADGE_FONT_SIZE_LOGICAL,
            line_height_logical: CHROME_BADGE_LINE_HEIGHT_LOGICAL,
            color,
            bold: true,
        });
    }

    fn draw_header_action(
        &mut self,
        pane_rect: &PaneRect,
        pane_state: Option<&PaneViewState>,
        theme: &Theme,
    ) {
        let s = self.scale_factor;
        let Some(pill) = pane_rect.action_pill else {
            return;
        };
        let (pill_label, pill_color) = match pane_rect.role {
            PaneRole::Agent if pill.width < 105. * s => ("Preset".into(), theme.border_focused),
            PaneRole::Runner if pill.width < 150. * s => ("Forward".into(), theme.status_err),
            PaneRole::Agent => {
                let preset_label = pane_state.and_then(|p| p.agent_preset).map_or_else(
                    || "Choose preset".into(),
                    |p| format!("Preset: {}", p.short_name()),
                );
                (preset_label, theme.border_focused)
            }
            PaneRole::Runner => (
                if cfg!(target_os = "macos") {
                    "Send Error → Agent ⌘⇧E".to_string()
                } else {
                    "Send Error → Agent ^⇧E".to_string()
                },
                theme.status_err,
            ),
            PaneRole::Service => (String::new(), theme.text_primary),
        };
        if !pill_label.is_empty() {
            self.push_chrome_button(ChromeLineSpec {
                text: &pill_label,
                left: pill.x + 8.0 * s,
                top: pill.y + (pill.height - CHROME_SUB_LINE_HEIGHT_LOGICAL * s) * 0.5,
                max_w: (pill.width - 12.0 * s).max(1.0),
                bounds: pill,
                font_size_logical: CHROME_SUB_FONT_SIZE_LOGICAL,
                line_height_logical: CHROME_SUB_LINE_HEIGHT_LOGICAL,
                color: pill_color,
                bold: true,
            });
        }
    }
}

fn pane_title(state: &AppState, role: PaneRole, pane_state: Option<&PaneViewState>) -> String {
    let role_label = match role {
        PaneRole::Agent => "Agent".to_owned(),
        PaneRole::Service => "Service".to_owned(),
        PaneRole::Runner => "Runner".to_owned(),
    };

    let cwd_suffix = pane_state
        .and_then(|p| p.summary.cwd.as_ref())
        .and_then(|cwd| cwd.file_name())
        .and_then(|n| n.to_str())
        .map(|name| format!("  {name}"))
        .unwrap_or_default();

    let scroll_suffix = pane_state
        .filter(|p| p.scroll_offset > 0)
        .map(|p| format!("  [SCROLL -{}]", p.scroll_offset))
        .unwrap_or_default();

    let label = pane_state
        .and_then(|pane| state.session_names.get(&pane.summary.id))
        .unwrap_or(&role_label);
    let title = if state.zoomed {
        format!("{cwd_suffix}{scroll_suffix}")
            .trim_start()
            .to_owned()
    } else {
        state
            .widget_label(role)
            .unwrap_or_else(|| format!("{label}{cwd_suffix}{scroll_suffix}"))
    };
    match pane_state {
        Some(pane) if !state.zoomed => crate::state::terminal_title(title, pane.summary.status),
        _ => title,
    }
}

fn pane_activity_badge(
    state: &AppState,
    pane_rect: &PaneRect,
    pane_state: Option<&PaneViewState>,
    theme: &Theme,
    scale: f32,
) -> Option<(Rect, &'static str, Rgba)> {
    if pane_rect.role != PaneRole::Agent || state.zoomed {
        return None;
    }
    let (label, color) = agent_state_badge(pane_state?.summary.agent_state?, theme);
    pane_agent_state_pill_rect(pane_rect, label, scale).map(|rect| (rect, label, color))
}
