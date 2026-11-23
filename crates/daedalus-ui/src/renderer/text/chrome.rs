use super::TextRenderer;
use super::{
    AppState, CHROME_BADGE_FONT_SIZE_LOGICAL, CHROME_BADGE_LINE_HEIGHT_LOGICAL,
    CHROME_BTN_FONT_SIZE_LOGICAL, CHROME_FONT_SIZE_LOGICAL, CHROME_LINE_HEIGHT_LOGICAL,
    CHROME_SUB_FONT_SIZE_LOGICAL, CHROME_SUB_LINE_HEIGHT_LOGICAL, ChromeLineSpec, Rect, Theme,
    WorkspaceLayout, truncate_right_with_ellipsis,
};
use crate::layout::{PaneRect, sidebar_workspace_close_rect};
use crate::renderer::sidebar::{Badge, BadgeKind, Card};
use crate::renderer::{
    SidebarCards, active_external_detail_preview, agent_state_badge, banner_appearance,
    format_ports_summary, pane_agent_state_pill_rect,
};
use daedalus_proto::{PaneRole, PaneStatus, WorkspaceInfo};

fn service_observations(ws: &WorkspaceInfo) -> Vec<String> {
    let mut lines: Vec<_> = ws
        .containers
        .iter()
        .map(|container| {
            let name = container.service.as_ref().unwrap_or(&container.name);
            let status = container.health.as_ref().unwrap_or(&container.state);
            format!("{name} · {status}")
        })
        .collect();
    for port in &ws.bound_ports {
        let label = port.service.as_deref().unwrap_or("Port");
        lines.push(format!("{label} · bound :{}", port.host_port));
    }
    if lines.is_empty() {
        lines.push("No service observations".into());
    }
    lines
}

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
            if state.active_workspace_id.is_some() {
                let button = crate::layout::settings_button(toggle, arrows, s);
                self.push_chrome_line(ChromeLineSpec {
                    text: "Settings",
                    bounds: button,
                    left: button.x + 3. * s,
                    top: button.y + (button.height - CHROME_LINE_HEIGHT_LOGICAL * s) * 0.5,
                    max_w: button.width - 3. * s,
                    font_size_logical: CHROME_SUB_FONT_SIZE_LOGICAL,
                    line_height_logical: CHROME_LINE_HEIGHT_LOGICAL,
                    color: theme.border_focused,
                    bold: true,
                });
                toggle.width = (button.x - toggle.x).max(0.);
            } else if let Some((up, _)) = arrows {
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
            let name = state
                .active_workspace()
                .map_or("Select", |ws| ws.name.as_str());
            let label = format!("{arrow} Workspaces · {name}{suffix}");
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
            if let Some((up, down)) = arrows {
                for (rect, label, enabled) in [
                    (up, "▲", list.visible.start > 0),
                    (down, "▼", list.visible.end < list.rows.len()),
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
            for (row, rect) in list.rows[list.visible.clone()].iter().zip(&list.rects) {
                use crate::sidebar_content::SidebarRow;
                let (text, color, bold) = match row {
                    SidebarRow::Workspace(_) => continue,
                    SidebarRow::Heading(text) => (text, theme.text_primary, true),
                    SidebarRow::Detail(text, level) => (
                        text,
                        level.map_or(theme.text_muted, |level| {
                            super::super::external_status_color(level, theme)
                        }),
                        false,
                    ),
                };
                self.push_chrome_line(ChromeLineSpec {
                    text,
                    bounds: *rect,
                    left: rect.x + 5.0 * s,
                    top: rect.y,
                    max_w: (rect.width - 10.0 * s).max(1.0),
                    font_size_logical: CHROME_SUB_FONT_SIZE_LOGICAL,
                    line_height_logical: CHROME_SUB_LINE_HEIGHT_LOGICAL,
                    color,
                    bold,
                });
            }
            for (idx, card) in cards.indices.iter().copied().zip(&cards.cards) {
                self.build_sidebar_workspace_text(&state.workspaces[idx], card, state, theme);
            }

            let new_btn = cards.list.new_workspace;
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

            let new_wt_btn = cards.list.new_worktree;
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
            if let Some(id) = state.active_workspace_id {
                let mut top = new_wt_btn.bottom() + 12.0 * s;
                let height = CHROME_SUB_LINE_HEIGHT_LOGICAL * s;
                let left = new_btn.x + 4.0 * s;
                let width = (new_wt_btn.right() - left).max(1.0);
                if top + height * 2.0 <= sidebar.bottom() {
                    let lines = state.recap.lines(id);
                    for (index, text) in std::iter::once("Since you left")
                        .chain(lines.iter().map(String::as_str))
                        .enumerate()
                    {
                        if top + height > sidebar.bottom() {
                            break;
                        }
                        let max_chars =
                            (width / (CHROME_SUB_FONT_SIZE_LOGICAL * s * 0.62)) as usize;
                        let label = truncate_right_with_ellipsis(text, max_chars);
                        self.push_chrome_line(ChromeLineSpec {
                            text: &label,
                            bounds: Rect::new(left, top, width, height),
                            left,
                            top,
                            max_w: width,
                            font_size_logical: CHROME_SUB_FONT_SIZE_LOGICAL,
                            line_height_logical: CHROME_SUB_LINE_HEIGHT_LOGICAL,
                            color: if index == 0 {
                                theme.text_primary
                            } else {
                                theme.text_muted
                            },
                            bold: index == 0,
                        });
                        self.recap_drawn |= index > 0;
                        top += height;
                    }
                    if let Some(ws) = state.active_workspace() {
                        top += 10. * s;
                        let service_title =
                            if state.connection == crate::state::ConnectionState::Connected {
                                "Workspace services"
                            } else {
                                "Last known services"
                            };
                        for (i, label) in std::iter::once(service_title.to_owned())
                            .chain(service_observations(ws))
                            .enumerate()
                        {
                            if top + height > sidebar.bottom() {
                                break;
                            }
                            self.push_chrome_line(ChromeLineSpec {
                                text: &label,
                                bounds: Rect::new(left, top, width, height),
                                left,
                                top,
                                max_w: width,
                                font_size_logical: CHROME_SUB_FONT_SIZE_LOGICAL,
                                line_height_logical: CHROME_SUB_LINE_HEIGHT_LOGICAL,
                                color: if i == 0 {
                                    theme.border_focused
                                } else {
                                    theme.text_primary
                                },
                                bold: i == 0,
                            });
                            top += height;
                        }
                    }
                }
            }
        }

        if let Some(rect) = layout.compact_recap
            && let Some(ws) = state.active_workspace()
        {
            let mut lines = vec!["Since you left".to_owned()];
            lines.extend(state.recap.lines(ws.id));
            if !ws.containers.is_empty() || !ws.bound_ports.is_empty() {
                lines.push(format!(
                    "{} listening ports · {} containers",
                    ws.bound_ports.len(),
                    ws.containers.len()
                ));
            }
            let step = CHROME_SUB_LINE_HEIGHT_LOGICAL * s;
            for (i, text) in lines.iter().enumerate() {
                let top = rect.y + 3. * s + i as f32 * step;
                if top + step > rect.bottom() {
                    break;
                }
                self.recap_drawn |= i > 0;
                self.push_chrome_line(ChromeLineSpec {
                    text,
                    bounds: rect,
                    top,
                    left: rect.x + 8. * s,
                    max_w: (rect.width - 16. * s).max(1.),
                    font_size_logical: CHROME_SUB_FONT_SIZE_LOGICAL,
                    line_height_logical: CHROME_SUB_LINE_HEIGHT_LOGICAL,
                    color: if i == 0 {
                        theme.border_focused
                    } else {
                        theme.text_primary
                    },
                    bold: i == 0,
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
            self.build_pane_header_text(pane_rect, state, theme, layout);
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

    fn build_pane_header_text(
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

        let agent_badge = if pane_rect.role == PaneRole::Agent && !state.zoomed {
            pane_state
                .and_then(|p| p.summary.agent_state)
                .and_then(|ast| {
                    let (label, color) = agent_state_badge(ast, theme);
                    pane_agent_state_pill_rect(pane_rect, label, s).map(|rect| (rect, label, color))
                })
        } else {
            None
        };

        let label = pane_state
            .and_then(|pane| state.session_names.get(&pane.summary.id))
            .unwrap_or(&role_label);
        let title = format!("{label}{cwd_suffix}{scroll_suffix}");
        let text_left = if let Some((_, _, last)) = layout.named_tabs.last() {
            for (id, label, rect) in &layout.named_tabs {
                let active = Some(*id) == state.focused_pane_id();
                let close = layout.close_tab.filter(|(pane, _)| pane == id);
                if let Some((_, button)) = close {
                    self.push_chrome_line(ChromeLineSpec {
                        text: "×",
                        left: button.x + 3. * s,
                        top: button.y + (button.height - CHROME_LINE_HEIGHT_LOGICAL * s) * 0.5,
                        max_w: button.width,
                        bounds: button,
                        font_size_logical: CHROME_FONT_SIZE_LOGICAL,
                        line_height_logical: CHROME_LINE_HEIGHT_LOGICAL,
                        color: theme.text_muted,
                        bold: false,
                    });
                }
                self.push_chrome_line(ChromeLineSpec {
                    text: label,
                    left: rect.x + 4. * s,
                    top: rect.y + (rect.height - CHROME_LINE_HEIGHT_LOGICAL * s) * 0.5,
                    max_w: (rect.width - close.map_or(0., |(_, b)| b.width) - 8. * s).max(1.),
                    bounds: *rect,
                    font_size_logical: CHROME_FONT_SIZE_LOGICAL,
                    line_height_logical: CHROME_LINE_HEIGHT_LOGICAL,
                    color: if active {
                        theme.border_focused
                    } else {
                        theme.text_muted
                    },
                    bold: active,
                });
            }
            last.right() + 4. * s
        } else if state.zoomed {
            let tabs = crate::layout::session_tabs(pane_rect, s);
            for (role, rect) in tabs {
                let label = match role {
                    PaneRole::Agent => "Agent",
                    PaneRole::Service => "Service",
                    PaneRole::Runner => "Runner",
                };
                self.push_chrome_line(ChromeLineSpec {
                    text: label,
                    left: rect.x + 4. * s,
                    top: rect.y + (rect.height - CHROME_LINE_HEIGHT_LOGICAL * s) * 0.5,
                    max_w: (rect.width - 4. * s).max(1.),
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
            header.x + 22.0 * s
        };
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
    fn recap_quotes_agent_pane_changes_and_ignores_old_frames() {
        use crate::bridge::UiEvent;
        use daedalus_proto::{Cell, GridDelta, GridRow, ServerEvent};
        let (mut state, _) = make_state_with_agent_row();
        let ws = state.active_workspace().unwrap();
        let id = ws.id;
        let pane_id = ws.panes[0].id;
        let event = |seq, text: &str| {
            UiEvent::ServerEvent(ServerEvent::GridDelta(GridDelta {
                pane_id,
                seq,
                cols: 20,
                rows: 5,
                cursor: Default::default(),
                dirty_rows: vec![GridRow {
                    row_index: 0,
                    abs_row: 0,
                    cells: vec![Cell {
                        text: text.into(),
                        ..Default::default()
                    }],
                }],
                history_generation: 0,
                scrollback_len: None,
            }))
        };
        state.recap.select(Some(id));
        state.recap.focus(false);
        state.panes.get_mut(&pane_id).unwrap().last_seq = 4;
        state
            .recap
            .observe(&event(5, "Build passed"), &state.workspaces, &state.panes);
        state
            .recap
            .observe(&event(3, "obsolete"), &state.workspaces, &state.panes);
        state.recap.observe(
            &UiEvent::ServerEvent(ServerEvent::CommandFinished {
                pane_id,
                command: None,
                exit_code: 0,
                has_failure_trace: false,
            }),
            &state.workspaces,
            &state.panes,
        );
        state.recap.focus(true);
        assert_eq!(
            state.recap.lines(id),
            ["Agent terminal · Build passed", "agent command · exit 0"]
        );
    }

    #[test]
    fn recap_replay_cannot_quote_a_row_redrawn_after_the_recorded_frame() {
        use daedalus_proto::{ActivityEntry, ActivityKind};
        let (mut state, _) = make_state_with_agent_row();
        let pane = state.panes.values().next().unwrap();
        let pane_id = pane.summary.id;
        let entry = ActivityEntry {
            seq: 3,
            at_ms: 0,
            kind: ActivityKind::AgentOutput {
                pane_id,
                abs_row: 0,
                history_generation: pane.history_generation,
                pane_seq: pane.last_seq,
            },
        };
        assert_eq!(
            crate::recap::activity_excerpt(&entry, &state.panes).as_deref(),
            Some("Agent terminal · abc")
        );
        state.panes.get_mut(&pane_id).unwrap().last_seq += 1;
        assert!(crate::recap::activity_excerpt(&entry, &state.panes).is_none());
    }

    #[test]
    fn repository_headers_and_widget_rows_share_geometry_with_pointer_targets() {
        use crate::layout::{HitTarget, hit_test};
        use crate::sidebar_content::{SidebarContent, SidebarRow};
        use daedalus_proto::{SidebarWidget, WorkspaceManifest};
        use std::sync::Arc;
        let (state, mut layout) = make_state_with_agent_row();
        let mut main = state.workspaces[0].clone();
        main.project.repository = Some("/one/api".into());
        main.project.tools = vec!["Cargo".into(), "npm".into()];
        main.manifest = Some(WorkspaceManifest {
            sidebar_widgets: vec![SidebarWidget {
                title: "Checks".into(),
                provider: "ci".into(),
                statuses: vec!["build".into(), "test".into()],
            }],
            ..Default::default()
        });
        main.external_statuses.push(ExternalStatus {
            source: "ci.build".into(),
            label: "passed".into(),
            level: StatusLevel::Ok,
            detail: None,
            updated_at_ms: 0,
        });
        let mut other = main.clone();
        other.project.repository = Some("/two/api".into());
        other.manifest = None;
        let mut linked = main.clone();
        linked.external_statuses.clear();
        let mut standalone = other.clone();
        standalone.project.repository = None;
        let workspaces = [main, other, linked, standalone];
        let sidebar = Rect::new(0., 0., 240., 1200.);
        let scene = Arc::new(SidebarContent::for_workspaces(&workspaces, sidebar, 0, 1.));
        let indices: Vec<_> = scene
            .rows
            .iter()
            .filter_map(|row| match row {
                SidebarRow::Workspace(i) => Some(*i),
                _ => None,
            })
            .collect();
        assert_eq!(indices, [0, 2, 1, 3]);
        assert_eq!(scene.rows[0], SidebarRow::Heading("/one/api".into()));
        assert!(scene.rows.contains(&SidebarRow::Heading("/two/api".into())));
        assert!(
            scene
                .rows
                .contains(&SidebarRow::Detail("Cargo · npm".into(), None))
        );
        assert!(scene.rows.contains(&SidebarRow::Detail(
            "build · passed".into(),
            Some(StatusLevel::Ok)
        )));
        assert!(
            scene
                .rows
                .contains(&SidebarRow::Detail("build · No report".into(), None))
        );
        assert!(
            scene
                .rows
                .contains(&SidebarRow::Detail("test · No report".into(), None))
        );
        layout.sidebar = Some(sidebar);
        layout.sidebar_content = Some(scene.clone());
        for (row, rect) in scene.rows.iter().zip(&scene.rects) {
            let target = hit_test(&layout, 4, 1., 8., 16., rect.x + 3., rect.y + 2.);
            if let SidebarRow::Workspace(i) = row {
                assert_eq!(target, HitTarget::SidebarWorkspace(*i));
                let close = sidebar_workspace_close_rect(*rect, 1.);
                assert_eq!(
                    hit_test(&layout, 4, 1., 8., 16., close.x + 1., close.y + 1.),
                    HitTarget::SidebarWorkspaceClose(*i)
                );
            } else {
                assert_eq!(target, HitTarget::None);
            }
        }
        let short = Rect {
            height: 160.,
            ..sidebar
        };
        let last = SidebarContent::for_workspaces(&workspaces, short, usize::MAX, 1.);
        assert_eq!(last.visible.end, last.rows.len());
        assert!(last.arrows.is_some());
    }

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
            let cards = SidebarCards::new(&state, sidebar, 1.0, &theme, None);
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
            let cards = SidebarCards::new(&state, sidebar, 1.0, &theme, None);
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
