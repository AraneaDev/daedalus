use super::TextRenderer;
use super::{
    AppState, CHROME_BADGE_FONT_SIZE_LOGICAL, CHROME_BADGE_LINE_HEIGHT_LOGICAL,
    CHROME_BTN_FONT_SIZE_LOGICAL, CHROME_FONT_SIZE_LOGICAL, CHROME_LINE_HEIGHT_LOGICAL,
    CHROME_SUB_FONT_SIZE_LOGICAL, CHROME_SUB_LINE_HEIGHT_LOGICAL, ChromeLineSpec, Rect, Theme,
    WorkspaceLayout, truncate_right_with_ellipsis,
};
use crate::layout::{
    LayoutMetrics, PaneRect, sidebar_new_workspace_rect, sidebar_new_worktree_rect,
    sidebar_workspace_close_rect,
};
use crate::renderer::sidebar::{Badge, BadgeKind, Card};
use crate::renderer::{
    SidebarCards, active_external_detail_preview, agent_state_badge, banner_appearance,
    format_ports_summary, pane_agent_state_pill_rect,
};
use daedalus_proto::{PaneRole, PaneStatus, WorkspaceInfo};

fn sidebar_badge_line(badge: &Badge, s: f32) -> ChromeLineSpec<'_> {
    let runner = badge.kind == BadgeKind::Runner;
    let top_inset = match badge.kind {
        BadgeKind::Runner | BadgeKind::External => 2.0,
        BadgeKind::Branch => 1.0,
        BadgeKind::Agent | BadgeKind::Docker => 1.5,
    };
    let rect = badge.rect;
    ChromeLineSpec {
        text: &badge.label,
        bounds: rect,
        left: rect.x + if runner { 4.0 } else { 5.0 } * s,
        top: rect.y + top_inset * s,
        max_w: (rect.width - if runner { 2.0 } else { 8.0 } * s).max(1.0),
        font_size_logical: if runner {
            CHROME_SUB_FONT_SIZE_LOGICAL
        } else {
            CHROME_BADGE_FONT_SIZE_LOGICAL
        },
        line_height_logical: if runner {
            CHROME_SUB_LINE_HEIGHT_LOGICAL
        } else {
            CHROME_BADGE_LINE_HEIGHT_LOGICAL
        },
        color: badge.color,
        bold: badge.bold,
    }
}

impl TextRenderer {
    pub(super) fn build_chrome_items(
        &mut self,
        state: &AppState,
        layout: &WorkspaceLayout,
        theme: &Theme,
        cards: Option<&SidebarCards>,
    ) {
        let s = self.scale_factor;

        let arrows = cards.and_then(|cards| cards.list.arrows);
        if let Some(mut toggle) = layout.sidebar_toggle {
            if let Some((up, _)) = arrows {
                toggle.width = (up.x - toggle.x).max(0.0);
            }
            let arrow = if layout.sidebar.is_some() {
                "▾"
            } else {
                "▸"
            };
            let suffix = state
                .remote_host
                .as_ref()
                .map(|host| format!("  [REMOTE: {host}]"))
                .unwrap_or_default();
            let label = format!("{arrow} Workspaces{suffix}");
            self.push_chrome_line(ChromeLineSpec {
                text: &label,
                left: toggle.x + 12.0 * s,
                top: toggle.y + (toggle.height - CHROME_LINE_HEIGHT_LOGICAL * s) * 0.5,
                max_w: (toggle.width - 24.0 * s).max(1.0),
                bounds: toggle,
                font_size_logical: CHROME_FONT_SIZE_LOGICAL,
                line_height_logical: CHROME_LINE_HEIGHT_LOGICAL,
                color: theme.text_primary,
                bold: true,
            });
        }

        // Sidebar text
        if let (Some(sidebar), Some(cards)) = (layout.sidebar, cards) {
            let list = &cards.list;
            let m = LayoutMetrics::for_scale(s);
            if layout.sidebar_toggle.is_none() {
                let header_rect = Rect::new(
                    sidebar.x,
                    sidebar.y,
                    arrows.map_or(sidebar.width, |(up, _)| (up.x - sidebar.x).max(0.0)),
                    m.sidebar_header_height,
                );
                let header = state
                    .remote_host
                    .as_ref()
                    .map(|host| format!("[REMOTE: {host}]"));
                self.push_chrome_line(ChromeLineSpec {
                    text: header.as_deref().unwrap_or("WORKSPACES"),
                    left: header_rect.x + m.sidebar_pad + 4.0 * s,
                    top: header_rect.y
                        + (header_rect.height - CHROME_SUB_LINE_HEIGHT_LOGICAL * s) * 0.5,
                    max_w: header_rect.width - 2.0 * m.sidebar_pad,
                    bounds: header_rect,
                    font_size_logical: CHROME_SUB_FONT_SIZE_LOGICAL,
                    line_height_logical: CHROME_SUB_LINE_HEIGHT_LOGICAL,
                    color: theme.text_muted,
                    bold: true,
                });
            }

            if let Some((up, down)) = arrows {
                for (rect, label, enabled) in [
                    (up, "▲", list.visible.start > 0),
                    (down, "▼", list.visible.end < state.workspaces.len()),
                ] {
                    self.push_chrome_line(ChromeLineSpec {
                        text: label,
                        left: rect.x + rect.width * 0.33,
                        top: rect.y + (rect.height - CHROME_LINE_HEIGHT_LOGICAL * s) * 0.5,
                        max_w: rect.width * 0.67,
                        bounds: rect,
                        font_size_logical: CHROME_BTN_FONT_SIZE_LOGICAL,
                        line_height_logical: CHROME_LINE_HEIGHT_LOGICAL,
                        color: if enabled {
                            theme.text_primary
                        } else {
                            theme.border_unfocused
                        },
                        bold: false,
                    });
                }
            }
            for (idx, card) in list.visible.clone().zip(&cards.cards) {
                self.build_sidebar_workspace_text(&state.workspaces[idx], card, state, theme);
            }

            let new_btn = sidebar_new_workspace_rect(sidebar, state.workspaces.len(), s);
            if new_btn.y < sidebar.bottom() {
                self.push_chrome_line(ChromeLineSpec {
                    text: "+ Workspace",
                    left: new_btn.x + 8.0 * s,
                    top: new_btn.y + (new_btn.height - CHROME_LINE_HEIGHT_LOGICAL * s) * 0.5,
                    max_w: new_btn.width - 12.0 * s,
                    bounds: new_btn,
                    font_size_logical: CHROME_BTN_FONT_SIZE_LOGICAL,
                    line_height_logical: CHROME_LINE_HEIGHT_LOGICAL,
                    color: theme.text_primary,
                    bold: false,
                });
            }

            let new_wt_btn = sidebar_new_worktree_rect(sidebar, state.workspaces.len(), s);
            if new_wt_btn.y < sidebar.bottom() {
                self.push_chrome_line(ChromeLineSpec {
                    text: "+ Worktree",
                    left: new_wt_btn.x + 8.0 * s,
                    top: new_wt_btn.y + (new_wt_btn.height - CHROME_LINE_HEIGHT_LOGICAL * s) * 0.5,
                    max_w: new_wt_btn.width - 12.0 * s,
                    bounds: new_wt_btn,
                    font_size_logical: CHROME_BTN_FONT_SIZE_LOGICAL,
                    line_height_logical: CHROME_LINE_HEIGHT_LOGICAL,
                    color: theme.border_focused,
                    bold: true,
                });
            }
        }

        // Top banner (disconnected or toast)
        if let Some(banner) = layout.banner
            && let (_, Some((msg, color))) = banner_appearance(state, theme)
            && !msg.is_empty()
        {
            self.push_chrome_line(ChromeLineSpec {
                text: &msg,
                left: banner.x + 10.0 * s,
                top: banner.y + (banner.height - CHROME_LINE_HEIGHT_LOGICAL * s) * 0.5,
                max_w: banner.width - 20.0 * s,
                bounds: banner,
                font_size_logical: CHROME_FONT_SIZE_LOGICAL,
                line_height_logical: CHROME_LINE_HEIGHT_LOGICAL,
                color,
                bold: true,
            });
        }

        // Pane headers & action pills
        for pane_rect in &layout.panes {
            self.build_pane_header_text(pane_rect, state, theme);
        }
    }

    fn build_sidebar_workspace_text(
        &mut self,
        ws: &WorkspaceInfo,
        card: &Card,
        state: &AppState,
        theme: &Theme,
    ) {
        let s = self.scale_factor;
        let row_rect = card.rect;
        let is_active = state.active_workspace_id == Some(ws.id);
        let left_x = row_rect.x + 22.0 * s;

        let name_max_w = card.title_width;
        let title_color = if is_active {
            theme.text_primary
        } else {
            theme.text_primary.scale_luminance(0.85)
        };

        self.push_chrome_line(ChromeLineSpec {
            text: &ws.name,
            left: left_x,
            top: row_rect.y + 6.0 * s,
            max_w: name_max_w,
            bounds: Rect::new(left_x, row_rect.y, name_max_w, row_rect.height),
            font_size_logical: CHROME_FONT_SIZE_LOGICAL,
            line_height_logical: CHROME_LINE_HEIGHT_LOGICAL,
            color: title_color,
            bold: is_active,
        });

        // Compact `×` close button in top-right corner
        let close_rect = sidebar_workspace_close_rect(row_rect, s);
        self.push_chrome_line(ChromeLineSpec {
            text: "×",
            left: close_rect.x + 4.0 * s,
            top: close_rect.y + 1.0 * s,
            max_w: (close_rect.width - 4.0 * s).max(1.0),
            bounds: close_rect,
            font_size_logical: CHROME_BTN_FONT_SIZE_LOGICAL,
            line_height_logical: CHROME_SUB_LINE_HEIGHT_LOGICAL,
            color: theme.text_muted,
            bold: true,
        });

        for badge in &card.badges {
            self.push_chrome_line(sidebar_badge_line(badge, s));
        }
        if !card.full {
            return;
        }
        if let Some(sub_max_w) = card.subtitle_width {
            let dir_name = ws
                .root_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(".");
            let agent_status = ws
                .panes
                .iter()
                .rev()
                .find(|p| p.role == PaneRole::Agent)
                .map(|p| match p.status {
                    PaneStatus::Starting => "starting",
                    PaneStatus::Idle => "idle",
                    PaneStatus::Busy => "busy",
                    PaneStatus::Exited { .. } => "exited",
                })
                .unwrap_or("none");
            let subtitle = format!("{dir_name} · agent:{agent_status}");
            self.push_chrome_line(ChromeLineSpec {
                text: &subtitle,
                left: left_x,
                top: row_rect.y + 24.0 * s,
                max_w: sub_max_w,
                bounds: Rect::new(left_x, row_rect.y + 22.0 * s, sub_max_w, 18.0 * s),
                font_size_logical: CHROME_SUB_FONT_SIZE_LOGICAL,
                line_height_logical: CHROME_SUB_LINE_HEIGHT_LOGICAL,
                color: theme.text_muted,
                bold: false,
            });
        }

        // Line 3: Bound ports summary + Docker container health
        let (ports_label, has_ports) = format_ports_summary(&ws.bound_ports);
        let ports_left = row_rect.x + 10.0 * s;
        let ports_max_w = card.ports_width;
        self.push_chrome_line(ChromeLineSpec {
            text: &ports_label,
            left: ports_left,
            top: row_rect.y + 42.0 * s,
            max_w: ports_max_w,
            bounds: Rect::new(ports_left, row_rect.y + 40.0 * s, ports_max_w, 16.0 * s),
            font_size_logical: CHROME_BADGE_FONT_SIZE_LOGICAL,
            line_height_logical: CHROME_BADGE_LINE_HEIGHT_LOGICAL,
            color: if has_ports {
                theme.status_ok
            } else {
                theme.text_muted
            },
            bold: has_ports,
        });

        // Line 5 (active workspace only): truncated external status detail preview
        if is_active && let Some(preview) = active_external_detail_preview(ws) {
            let detail_left = row_rect.x + 10.0 * s;
            let detail_max_w = (row_rect.right() - detail_left - 8.0 * s).max(1.0);
            let approx_char_w = (CHROME_BADGE_FONT_SIZE_LOGICAL * s * 0.62).max(1.0);
            let max_chars = ((detail_max_w / approx_char_w).floor() as usize).max(8);
            let truncated = truncate_right_with_ellipsis(&preview, max_chars);
            self.push_chrome_line(ChromeLineSpec {
                text: &truncated,
                left: detail_left,
                top: row_rect.y + 79.0 * s,
                max_w: detail_max_w,
                bounds: Rect::new(detail_left, row_rect.y + 77.0 * s, detail_max_w, 17.0 * s),
                font_size_logical: CHROME_BADGE_FONT_SIZE_LOGICAL,
                line_height_logical: CHROME_BADGE_LINE_HEIGHT_LOGICAL,
                color: theme.text_muted,
                bold: false,
            });
        }
    }

    fn build_pane_header_text(&mut self, pane_rect: &PaneRect, state: &AppState, theme: &Theme) {
        let s = self.scale_factor;
        let header = pane_rect.header;
        let is_focused = pane_rect.role == state.focused_role;
        let pane_state = state.pane_for_role(pane_rect.role);

        let role_label = match pane_rect.role {
            PaneRole::Agent => {
                let preset_name = pane_state
                    .and_then(|p| p.agent_preset)
                    .map(|p| p.short_name())
                    .unwrap_or("agent");
                format!("AGENT [{preset_name}]")
            }
            PaneRole::Service => "SERVICE".to_string(),
            PaneRole::Runner => "RUNNER".to_string(),
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

        let agent_badge = if pane_rect.role == PaneRole::Agent {
            pane_state
                .and_then(|p| p.summary.agent_state)
                .and_then(|ast| {
                    let (label, color) = agent_state_badge(ast, theme);
                    pane_agent_state_pill_rect(pane_rect, label, s).map(|rect| (rect, label, color))
                })
        } else {
            None
        };

        let title = format!("{role_label}{cwd_suffix}{scroll_suffix}");
        let text_left = header.x + 22.0 * s;
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
            self.push_chrome_line(ChromeLineSpec {
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

        if let Some(pill) = pane_rect.action_pill {
            let (pill_label, pill_color) = match pane_rect.role {
                PaneRole::Agent => {
                    let preset_name = pane_state
                        .and_then(|p| p.agent_preset)
                        .map(|p| p.short_name())
                        .unwrap_or("preset");
                    (format!("Preset: {preset_name}"), theme.border_focused)
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
                self.push_chrome_line(ChromeLineSpec {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::{collect_sidebar_quads, tests::make_state_with_agent_row};
    use daedalus_proto::{AgentState, ExternalStatus, PaneId, StatusLevel};

    #[test]
    fn sidebar_cards_keep_labels_and_backdrops_together_at_both_sizes() {
        let (mut state, _) = make_state_with_agent_row();
        let theme = Theme::dark();
        let full = Rect::new(0.0, 0.0, 240.0, 400.0);
        let compact = Rect {
            height: 140.0,
            ..full
        };
        for sidebar in [full, compact] {
            let cards = SidebarCards::new(&state, sidebar, 1.0, &theme);
            assert!(cards.cards[0].badges.is_empty());
            assert_eq!(cards.cards[0].title_width, cards.cards[0].rect.width - 46.0);
        }
        let ws = state.active_workspace_mut().unwrap();
        ws.git_branch = Some("main".into());
        ws.panes[0].agent_state = Some(AgentState::WaitingForApproval);
        let mut runner = ws.panes[0].clone();
        runner.id = PaneId::new();
        runner.role = PaneRole::Runner;
        runner.last_exit_code = Some(0);
        ws.panes.push(runner);
        for label in ["old", "second", "last"] {
            ws.external_statuses.push(ExternalStatus {
                source: "ci".into(),
                level: StatusLevel::Ok,
                label: label.into(),
                detail: None,
                updated_at_ms: 1,
            });
        }

        for (sidebar, full_size) in [(full, true), (compact, false)] {
            let cards = SidebarCards::new(&state, sidebar, 1.0, &theme);
            let card = &cards.cards[0];
            assert_eq!(card.full, full_size);
            assert_eq!(card.title_width, card.rect.width - 96.0);
            assert_eq!(card.badges[0].kind, BadgeKind::Runner);
            assert_eq!(card.badges[0].label, "✓ 0");
            if full_size {
                assert_eq!(card.badges.len(), 5);
                assert!(card.badges[2].rect.right() < card.badges[1].rect.x);
                assert!(card.badges[3].label.ends_with("second"));
                assert!(card.badges[4].label.ends_with("last"));
            } else {
                assert_eq!(card.badges.len(), 1);
            }
            let mut quads = Vec::new();
            collect_sidebar_quads(&mut quads, &state, sidebar, &theme, 1.0, 1.0, &cards);
            for badge in &card.badges {
                let line = sidebar_badge_line(badge, 1.0);
                let rect = line.bounds;
                assert_eq!(line.text, badge.label);
                assert!(rect.contains(line.left, line.top));
                assert!(rect.bottom() <= card.rect.bottom());
                assert!(quads.iter().any(|quad| quad.rect
                    == [rect.x, rect.y, rect.width, rect.height]
                    && quad.color == theme.bg_pane.to_array()));
            }
        }
    }
}
