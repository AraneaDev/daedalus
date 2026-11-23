use super::TextRenderer;
use super::{
    AppState, CHROME_BTN_FONT_SIZE_LOGICAL, CHROME_FONT_SIZE_LOGICAL, CHROME_LINE_HEIGHT_LOGICAL,
    CHROME_SUB_FONT_SIZE_LOGICAL, CHROME_SUB_LINE_HEIGHT_LOGICAL, ChromeLineSpec,
    MODAL_TITLE_FONT_SIZE_LOGICAL, MODAL_TITLE_LINE_HEIGHT_LOGICAL, Rect, Theme, WorkspaceLayout,
    truncate_left_with_ellipsis,
};
use crate::renderer::{
    action_palette_layout, close_modal_cancel_rect, close_modal_compose_rect,
    close_modal_confirm_rect, close_modal_dirty_banner_rect, close_modal_force_rect,
    close_modal_metrics, close_modal_worktree_rect, close_workspace_modal_rect, picker_item_rect,
    preset_picker_layout, search_bar_rect, worktree_button_rects, worktree_content_scale,
    worktree_input_rect, worktree_modal_rect,
};
use crate::state::{AgentPreset, SearchState};
use crate::theme;

pub(super) enum PickerKind {
    Preset,
    Action,
    Recipient,
}

impl PickerKind {
    pub(super) fn header(self, modal: Rect, theme: &Theme, s: f32) -> [ChromeLineSpec<'static>; 2] {
        let (title, hint) = match self {
            Self::Preset => ("Agent preset", "Enter to switch · Shift+Enter to add"),
            Self::Action => ("Workspace actions", "↑↓ to choose · Enter to run · Esc"),
            Self::Recipient => (
                "Send failure to agent",
                "↑↓ to choose · Enter to send · Esc",
            ),
        };
        [
            (
                title,
                4.,
                MODAL_TITLE_FONT_SIZE_LOGICAL,
                MODAL_TITLE_LINE_HEIGHT_LOGICAL,
                theme.text_primary,
                true,
            ),
            (
                hint,
                23.,
                CHROME_SUB_FONT_SIZE_LOGICAL,
                CHROME_SUB_LINE_HEIGHT_LOGICAL,
                theme.text_muted,
                false,
            ),
        ]
        .map(|(text, offset, size, height, color, bold)| ChromeLineSpec {
            text,
            left: modal.x + 16. * s,
            top: modal.y + offset * s,
            max_w: (modal.width - 32. * s).max(1.),
            bounds: modal,
            font_size_logical: size,
            line_height_logical: height,
            color,
            bold,
        })
    }
}

impl TextRenderer {
    pub(super) fn build_picker_header(&mut self, modal: Rect, theme: &Theme, kind: PickerKind) {
        for line in kind.header(modal, theme, self.scale_factor) {
            self.push_modal_line(line);
        }
    }

    pub(super) fn build_picker_items(
        &mut self,
        state: &AppState,
        theme: &Theme,
        screen_w: f32,
        screen_h: f32,
    ) {
        let s = self.scale_factor;
        let palette = state.action_palette.open;
        let actions = state.active_actions();
        let (modal, visible) = if palette {
            action_palette_layout(
                screen_w,
                screen_h,
                s,
                actions.len(),
                state.action_palette.selected_index,
            )
        } else {
            preset_picker_layout(screen_w, screen_h, s, state.picker.selected_index)
        };

        self.build_picker_header(
            modal,
            theme,
            if palette {
                PickerKind::Action
            } else {
                PickerKind::Preset
            },
        );

        if visible.is_empty() {
            let row = picker_item_rect(modal, 0, s);
            self.push_modal_line(ChromeLineSpec {
                text: "Add actions in .daedalus.toml",
                left: row.x + 10.0 * s,
                top: row.y + 6.0 * s,
                max_w: (row.width - 20.0 * s).max(1.0),
                bounds: row,
                font_size_logical: CHROME_FONT_SIZE_LOGICAL,
                line_height_logical: CHROME_LINE_HEIGHT_LOGICAL,
                color: theme.text_muted,
                bold: false,
            });
        }
        for (row, idx) in visible.enumerate() {
            let item_rect = picker_item_rect(modal, row, s);
            let (selected, label) = if palette {
                let action = &actions[idx];
                let shortcut = action.shortcut.as_deref().unwrap_or("");
                (
                    idx == state.action_palette.selected_index,
                    format!("{} · {:?} {}", action.label, action.target, shortcut),
                )
            } else {
                (
                    idx == state.picker.selected_index,
                    AgentPreset::ALL[idx].label().to_owned(),
                )
            };
            let prefix = if selected { "▸ " } else { "  " };
            let label = format!("{prefix}{label}");
            self.push_modal_line(ChromeLineSpec {
                text: &label,
                left: item_rect.x + 10.0 * s,
                top: item_rect.y + (item_rect.height - CHROME_LINE_HEIGHT_LOGICAL * s) * 0.5,
                max_w: (item_rect.width - 20.0 * s).max(1.0),
                bounds: item_rect,
                font_size_logical: CHROME_FONT_SIZE_LOGICAL,
                line_height_logical: CHROME_LINE_HEIGHT_LOGICAL,
                color: if selected {
                    theme.text_primary
                } else {
                    theme.text_muted
                },
                bold: selected,
            });
        }
    }

    pub(super) fn build_worktree_modal_items(
        &mut self,
        state: &AppState,
        theme: &Theme,
        screen_w: f32,
        screen_h: f32,
    ) {
        let dpi = self.scale_factor;
        let modal = worktree_modal_rect(screen_w, screen_h, dpi);
        let s = worktree_content_scale(modal, dpi);
        let font_scale = s / dpi;
        let input_rect = worktree_input_rect(modal, s);

        self.push_modal_line(ChromeLineSpec {
            text: "New Git Worktree",
            left: modal.x + 14.0 * s,
            top: modal.y + 10.0 * s,
            max_w: (modal.width - 28.0 * s).max(1.0),
            bounds: modal,
            font_size_logical: MODAL_TITLE_FONT_SIZE_LOGICAL * font_scale,
            line_height_logical: MODAL_TITLE_LINE_HEIGHT_LOGICAL * font_scale,
            color: theme.text_primary,
            bold: true,
        });

        let text_top = input_rect.y + (input_rect.height - CHROME_LINE_HEIGHT_LOGICAL * s) * 0.5;
        if state.worktree_modal.branch_input.is_empty() || state.worktree_modal.missing_branch() {
            self.push_modal_line(ChromeLineSpec {
                text: "|",
                left: input_rect.x + 10.0 * s,
                top: text_top,
                max_w: 12.0 * s,
                bounds: input_rect,
                font_size_logical: CHROME_FONT_SIZE_LOGICAL * font_scale,
                line_height_logical: CHROME_LINE_HEIGHT_LOGICAL * font_scale,
                color: theme.border_focused,
                bold: true,
            });
            self.push_modal_line(ChromeLineSpec {
                text: if state.worktree_modal.missing_branch() {
                    "Enter a branch name"
                } else {
                    "feat/..."
                },
                left: input_rect.x + 20.0 * s,
                top: text_top,
                max_w: (input_rect.width - 28.0 * s).max(1.0),
                bounds: input_rect,
                font_size_logical: CHROME_FONT_SIZE_LOGICAL * font_scale,
                line_height_logical: CHROME_LINE_HEIGHT_LOGICAL * font_scale,
                color: if state.worktree_modal.missing_branch() {
                    theme.status_err
                } else {
                    theme.text_muted
                },
                bold: false,
            });
        } else {
            let usable_w = (input_rect.width - 24.0 * s).max(16.0);
            let approx_char_w = (CHROME_FONT_SIZE_LOGICAL * s * 0.62).max(1.0);
            let max_chars = ((usable_w / approx_char_w).floor() as usize)
                .saturating_sub(1)
                .max(4);
            let truncated =
                truncate_left_with_ellipsis(&state.worktree_modal.branch_input, max_chars);
            let typed = format!("{truncated}|");
            self.push_modal_line(ChromeLineSpec {
                text: &typed,
                left: input_rect.x + 10.0 * s,
                top: text_top,
                max_w: (input_rect.width - 20.0 * s).max(1.0),
                bounds: input_rect,
                font_size_logical: CHROME_FONT_SIZE_LOGICAL * font_scale,
                line_height_logical: CHROME_LINE_HEIGHT_LOGICAL * font_scale,
                color: theme.text_primary,
                bold: false,
            });
        }

        let (cancel, create) = worktree_button_rects(modal, dpi);
        let enabled = state.can_create_worktree();
        let hints_fit = create.width >= 128.0 * s;
        for (rect, label, color) in [
            (
                cancel,
                if hints_fit { "Cancel (Esc)" } else { "Cancel" },
                theme.text_primary,
            ),
            (
                create,
                if state.connection != crate::state::ConnectionState::Connected {
                    "Waiting for daemon"
                } else if hints_fit {
                    "Create (Enter)"
                } else {
                    "Create"
                },
                if enabled {
                    theme.border_focused
                } else {
                    theme.text_muted
                },
            ),
        ] {
            self.push_modal_line(ChromeLineSpec {
                text: label,
                left: rect.x + 8.0 * s,
                top: rect.y + (rect.height - CHROME_LINE_HEIGHT_LOGICAL * s) * 0.5,
                max_w: (rect.width - 16.0 * s).max(1.0),
                bounds: rect,
                font_size_logical: CHROME_BTN_FONT_SIZE_LOGICAL * font_scale,
                line_height_logical: CHROME_LINE_HEIGHT_LOGICAL * font_scale,
                color,
                bold: enabled && rect == create,
            });
        }
    }

    pub(super) fn build_close_modal_items(
        &mut self,
        state: &AppState,
        theme: &Theme,
        screen_w: f32,
        screen_h: f32,
    ) {
        let s = self.scale_factor;
        let cm = &state.close_modal;
        let modal = close_workspace_modal_rect(screen_w, screen_h, s);

        let metrics = close_modal_metrics(modal, s);
        let dpi = s;
        let s = metrics.scale;
        let font_scale = s / dpi;
        self.push_modal_line(ChromeLineSpec {
            text: "Close Workspace",
            left: modal.x + 16.0 * s,
            top: modal.y + if metrics.compact { 2.0 } else { 10.0 } * s,
            max_w: (modal.width - 32.0 * s).max(1.0),
            bounds: modal,
            font_size_logical: MODAL_TITLE_FONT_SIZE_LOGICAL * font_scale,
            line_height_logical: MODAL_TITLE_LINE_HEIGHT_LOGICAL * font_scale,
            color: theme.text_primary,
            bold: true,
        });

        let summary_line = match &cm.branch {
            Some(branch) => format!("{}  ({branch})", cm.workspace_name),
            None => cm.workspace_name.clone(),
        };
        self.push_modal_line(ChromeLineSpec {
            text: &summary_line,
            left: modal.x + 16.0 * s,
            top: modal.y + (metrics.header_height + if metrics.compact { 0.0 } else { 8.0 }) * s,
            max_w: (modal.width - 32.0 * s).max(1.0),
            bounds: modal,
            font_size_logical: CHROME_FONT_SIZE_LOGICAL * font_scale,
            line_height_logical: CHROME_LINE_HEIGHT_LOGICAL * font_scale,
            color: theme.text_primary,
            bold: true,
        });

        if cm.is_dirty {
            let warn_rect = close_modal_dirty_banner_rect(modal, dpi);
            self.push_modal_line(ChromeLineSpec {
                text: if metrics.compact {
                    "Uncommitted changes"
                } else {
                    "⚠ Worktree has uncommitted changes"
                },
                left: warn_rect.x + 10.0 * s,
                top: warn_rect.y + (warn_rect.height - CHROME_SUB_LINE_HEIGHT_LOGICAL * s) * 0.5,
                max_w: (warn_rect.width - 20.0 * s).max(1.0),
                bounds: warn_rect,
                font_size_logical: CHROME_SUB_FONT_SIZE_LOGICAL * font_scale,
                line_height_logical: CHROME_SUB_LINE_HEIGHT_LOGICAL * font_scale,
                color: theme.status_busy,
                bold: true,
            });
        }

        let mut push_checkbox_text =
            |rect: Rect, checked: bool, label: &str, color: theme::Rgba| {
                let mark = if checked { "[x]" } else { "[ ]" };
                let line = format!("{mark} {label}");
                self.push_modal_line(ChromeLineSpec {
                    text: &line,
                    left: rect.x + 10.0 * s,
                    top: rect.y + (rect.height - CHROME_LINE_HEIGHT_LOGICAL * s) * 0.5,
                    max_w: (rect.width - 20.0 * s).max(1.0),
                    bounds: rect,
                    font_size_logical: CHROME_FONT_SIZE_LOGICAL * font_scale,
                    line_height_logical: CHROME_LINE_HEIGHT_LOGICAL * font_scale,
                    color,
                    bold: checked,
                });
            };

        let compose_rect = close_modal_compose_rect(modal, cm, dpi);
        push_checkbox_text(
            compose_rect,
            cm.compose_down,
            "Stop Docker Compose (c)",
            if cm.compose_down {
                theme.text_primary
            } else {
                theme.text_muted
            },
        );

        if let Some(wt_rect) = close_modal_worktree_rect(modal, cm, dpi) {
            push_checkbox_text(
                wt_rect,
                cm.remove_worktree,
                "Remove git worktree (w)",
                if cm.remove_worktree {
                    theme.status_busy
                } else {
                    theme.text_muted
                },
            );
        }

        if let Some(force_rect) = close_modal_force_rect(modal, cm, dpi) {
            push_checkbox_text(
                force_rect,
                cm.force_remove_worktree,
                if metrics.compact {
                    "Force removal (f)"
                } else {
                    "Force remove dirty worktree (f)"
                },
                if cm.force_remove_worktree {
                    theme.status_err
                } else {
                    theme.text_muted
                },
            );
        }

        let button_pad = if metrics.compact { 5.0 } else { 12.0 } * s;
        let cancel_rect = close_modal_cancel_rect(modal, dpi);
        self.push_modal_line(ChromeLineSpec {
            text: if metrics.compact {
                "Cancel (Esc)"
            } else {
                "Cancel"
            },
            left: cancel_rect.x + button_pad,
            top: cancel_rect.y + (cancel_rect.height - CHROME_LINE_HEIGHT_LOGICAL * s) * 0.5,
            max_w: (cancel_rect.width - 2.0 * button_pad).max(1.0),
            bounds: cancel_rect,
            font_size_logical: CHROME_FONT_SIZE_LOGICAL * font_scale,
            line_height_logical: CHROME_LINE_HEIGHT_LOGICAL * font_scale,
            color: theme.text_primary,
            bold: false,
        });

        let confirm_rect = close_modal_confirm_rect(modal, dpi);
        self.push_modal_line(ChromeLineSpec {
            text: if cm.submitting {
                "Closing…"
            } else if metrics.compact {
                "Close (Enter)"
            } else {
                "Confirm Close"
            },
            left: confirm_rect.x + button_pad,
            top: confirm_rect.y + (confirm_rect.height - CHROME_LINE_HEIGHT_LOGICAL * s) * 0.5,
            max_w: (confirm_rect.width - 2.0 * button_pad).max(1.0),
            bounds: confirm_rect,
            font_size_logical: CHROME_FONT_SIZE_LOGICAL * font_scale,
            line_height_logical: CHROME_LINE_HEIGHT_LOGICAL * font_scale,
            color: if cm.remove_worktree {
                theme.status_err
            } else {
                theme.border_focused
            },
            bold: true,
        });

        if metrics.compact || cm.submitting {
            return;
        }
        self.push_modal_line(ChromeLineSpec {
            text: "Enter: Confirm · Esc: Cancel",
            left: modal.x + 16.0 * s,
            top: modal.bottom() - 20.0 * s,
            max_w: (modal.width - 32.0 * s).max(1.0),
            bounds: modal,
            font_size_logical: CHROME_SUB_FONT_SIZE_LOGICAL * font_scale,
            line_height_logical: CHROME_SUB_LINE_HEIGHT_LOGICAL * font_scale,
            color: theme.text_muted,
            bold: false,
        });
    }

    pub(super) fn build_search_bar_items(
        &mut self,
        search: &SearchState,
        state: &AppState,
        layout: &WorkspaceLayout,
        theme: &Theme,
    ) {
        let Some(pane_rect) = layout
            .panes
            .iter()
            .find(|p| state.pane_id_for_role(p.role) == Some(search.pane_id))
            .or_else(|| layout.pane(state.focused_role))
        else {
            return;
        };
        let s = self.scale_factor;
        let bar = search_bar_rect(pane_rect, s);
        let counter = search.result_label().unwrap_or_default();
        let pad = (8.0 * s).min(bar.width * 0.25);
        let content_width = bar.width - 2.0 * pad;
        let counter_w = (counter.chars().count() as f32 * CHROME_SUB_FONT_SIZE_LOGICAL * s * 0.65)
            .min(content_width * 0.45);
        let gap = if counter.is_empty() { 0.0 } else { pad };
        let query_max_w = (content_width - counter_w - gap).max(0.0);
        let approx_char_w = (CHROME_FONT_SIZE_LOGICAL * s * 0.62).max(1.0);
        let prefix = "Search: ";
        let max_query_chars = ((query_max_w / approx_char_w).floor() as usize)
            .saturating_sub(prefix.chars().count() + 1)
            .max(3);
        let display_query = truncate_left_with_ellipsis(&search.query, max_query_chars);
        let query_line = format!("{prefix}{display_query}|");

        let query_bounds = Rect::new(bar.x + pad, bar.y, query_max_w, bar.height);
        self.push_modal_line(ChromeLineSpec {
            text: &query_line,
            left: bar.x + pad,
            top: bar.y + (bar.height - CHROME_LINE_HEIGHT_LOGICAL * s) * 0.5,
            max_w: query_max_w,
            bounds: query_bounds,
            font_size_logical: CHROME_FONT_SIZE_LOGICAL,
            line_height_logical: CHROME_LINE_HEIGHT_LOGICAL,
            color: theme.text_primary,
            bold: false,
        });

        if counter.is_empty() {
            return;
        }
        let counter_left = bar.right() - pad - counter_w;
        let counter_bounds = Rect::new(counter_left, bar.y, counter_w, bar.height);
        self.push_modal_line(ChromeLineSpec {
            text: &counter,
            left: counter_left,
            top: bar.y + (bar.height - CHROME_SUB_LINE_HEIGHT_LOGICAL * s) * 0.5,
            max_w: counter_bounds.width,
            bounds: counter_bounds,
            font_size_logical: CHROME_SUB_FONT_SIZE_LOGICAL,
            line_height_logical: CHROME_SUB_LINE_HEIGHT_LOGICAL,
            color: theme.text_muted,
            bold: false,
        });
    }
}

#[cfg(test)]
mod picker_tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn picker_shortcuts_fit_the_compact_header_above_the_first_row() {
        for scale in [1., 2.] {
            let (modal, _) = preset_picker_layout(320. * scale, 144. * scale, scale, 3);
            let row = picker_item_rect(modal, 0, scale);
            for kind in [
                PickerKind::Preset,
                PickerKind::Action,
                PickerKind::Recipient,
            ] {
                let [title, hint] = kind.header(modal, &Theme::dark(), scale);
                assert!(title.top + title.line_height_logical * scale <= hint.top);
                assert!(hint.top + hint.line_height_logical * scale <= row.y);
                for line in [title, hint] {
                    assert!(
                        line.text.width() as f32 * line.font_size_logical * 0.65 * scale
                            <= line.max_w,
                        "{}",
                        line.text
                    );
                }
            }
            assert!(
                PickerKind::Preset.header(modal, &Theme::dark(), scale)[1]
                    .text
                    .contains("Shift+Enter")
            );
        }
    }
}
