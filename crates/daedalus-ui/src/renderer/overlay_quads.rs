use super::{
    QuadInstance, Rect, Rgba, Theme, WorkspaceLayout, push_chrome_badge, push_control,
    push_inner_focus, push_rect_border,
};
use crate::layout::overlays::{
    PICKER_HEADER_LOGICAL_HEIGHT, WORKTREE_HEADER_LOGICAL_HEIGHT, action_palette_layout,
    close_modal_cancel_rect, close_modal_compose_rect, close_modal_confirm_rect,
    close_modal_dirty_banner_rect, close_modal_force_rect, close_modal_metrics,
    close_modal_worktree_rect, close_workspace_modal_rect, picker_item_rect, preset_picker_layout,
    search_bar_rect, search_controls, worktree_button_rects, worktree_content_scale,
    worktree_input_rect, worktree_modal_rect,
};
use crate::state::{ActiveOverlay, AppState};

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

struct OverlayQuads<'a> {
    quads: &'a mut Vec<QuadInstance>,
    state: &'a AppState,
    theme: &'a Theme,
    screen: (f32, f32),
    s: f32,
    border_w: f32,
}

pub(super) fn collect_overlay_quads(
    quads: &mut Vec<QuadInstance>,
    state: &AppState,
    layout: &WorkspaceLayout,
    theme: &Theme,
    screen: (f32, f32),
    s: f32,
    border_w: f32,
) {
    let mut output = OverlayQuads {
        quads,
        state,
        theme,
        screen,
        s,
        border_w,
    };
    match state.active_overlay() {
        Some(ActiveOverlay::FailureRecipients) => {
            if let Some(choices) = &state.failure_recipients {
                output.failure_recipients(choices);
            } else {
                output.inline(layout);
            }
        }
        Some(ActiveOverlay::SetupReview) => output.setup_review(),
        Some(ActiveOverlay::WorkspaceForm) => output.workspace_form(),
        Some(ActiveOverlay::Settings) => output.settings(),
        Some(overlay @ (ActiveOverlay::ActionPalette | ActiveOverlay::AgentPicker)) => {
            output.picker(overlay)
        }
        Some(ActiveOverlay::Worktree) => output.worktree(),
        Some(ActiveOverlay::CloseWorkspace) => output.close_workspace(),
        _ => output.inline(layout),
    }
}

impl OverlayQuads<'_> {
    fn failure_recipients(&mut self, choices: &crate::state::FailureRecipients) {
        let quads = &mut *self.quads;
        let theme = self.theme;
        let screen = self.screen;
        let s = self.s;
        let border_w = self.border_w;
        let (screen_w, screen_h) = screen;
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
    }

    fn setup_review(&mut self) {
        let quads = &mut *self.quads;
        let state = self.state;
        let theme = self.theme;
        let screen = self.screen;
        let s = self.s;
        let border_w = self.border_w;
        let (screen_w, screen_h) = screen;
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
    }

    fn workspace_form(&mut self) {
        let quads = &mut *self.quads;
        let state = self.state;
        let theme = self.theme;
        let screen = self.screen;
        let s = self.s;
        let border_w = self.border_w;
        let (screen_w, screen_h) = screen;
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
    }

    fn settings(&mut self) {
        let quads = &mut *self.quads;
        let state = self.state;
        let theme = self.theme;
        let screen = self.screen;
        let s = self.s;
        let border_w = self.border_w;
        let (screen_w, screen_h) = screen;
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
            if state.settings.is_read_only_text(control) {
                let inset = 11.0 * s;
                quads.push(QuadInstance::from_xywh(
                    rect.x + inset,
                    rect.bottom() - border_w,
                    (rect.width - 2.0 * inset).max(0.0),
                    border_w,
                    theme.border_unfocused,
                ));
                continue;
            }
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
            let fill = if control.primary() && state.settings.enabled(control) {
                theme.border_focused
            } else if state.settings.page_selected(control) {
                theme.border_focused.with_alpha(0.12)
            } else if control.field_label().is_some() {
                theme.bg_pane
            } else {
                theme.bg_header
            };
            push_control(quads, field, fill, border, border_w, 6.0 * s);
            if control.primary() && state.settings.focus == control {
                push_inner_focus(quads, field, border_w, theme.bg_window);
            }
        }
    }

    fn picker(&mut self, overlay: ActiveOverlay) {
        let quads = &mut *self.quads;
        let state = self.state;
        let theme = self.theme;
        let screen = self.screen;
        let s = self.s;
        let border_w = self.border_w;
        let (screen_w, screen_h) = screen;
        let (modal, selected) = if overlay == ActiveOverlay::ActionPalette {
            let idx = state.action_palette.selected_index;
            let (modal, visible) =
                action_palette_layout(screen_w, screen_h, s, state.palette_len(), idx);
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
    }

    fn worktree(&mut self) {
        let quads = &mut *self.quads;
        let state = self.state;
        let theme = self.theme;
        let screen = self.screen;
        let s = self.s;
        let border_w = self.border_w;
        let (screen_w, screen_h) = screen;
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
    }

    fn close_workspace(&mut self) {
        let quads = &mut *self.quads;
        let state = self.state;
        let theme = self.theme;
        let screen = self.screen;
        let s = self.s;
        let border_w = self.border_w;
        let (screen_w, screen_h) = screen;
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
    }

    fn inline(&mut self, layout: &WorkspaceLayout) {
        let Self {
            quads,
            state,
            theme,
            s,
            border_w,
            ..
        } = self;
        let (s, border_w) = (*s, *border_w);
        if let Some(ref search) = state.search
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
        } else if let Some(panel) = layout.preview_panel(state, s)
            && let Some((_, image)) = &state.image_preview
        {
            super::image_preview::draw(quads, panel, image, theme, s, border_w);
        } else if let Some(button) = layout.preview_button(state, s) {
            push_chrome_badge(quads, button, theme, border_w, theme.border_focused);
        }
    }
}
