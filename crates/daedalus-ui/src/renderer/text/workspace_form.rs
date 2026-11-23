use super::{AppState, ChromeLineSpec, Rect, TextRenderer, Theme};
use crate::workspace_form::{controls, modal_rect};

impl TextRenderer {
    pub(super) fn build_workspace_form_items(
        &mut self,
        state: &AppState,
        theme: &Theme,
        w: f32,
        h: f32,
    ) {
        let dpi = self.scale_factor;
        let modal = modal_rect(w, h, dpi);
        let scale = dpi.min(modal.height / 236.0).min(modal.width / 330.0);
        let form = &state.workspace_form;
        let mut line = |text: &str, rect: Rect, size: f32, color, bold| {
            let inset = 4.0 * scale;
            self.push_modal_line(ChromeLineSpec {
                text,
                left: rect.x + inset,
                top: rect.y + (rect.height - size * 1.4 * scale) / 2.0,
                max_w: (rect.width - 2.0 * inset).max(1.0),
                bounds: rect,
                font_size_logical: size * scale / dpi,
                line_height_logical: size * 1.4 * scale / dpi,
                color,
                bold,
            });
        };
        let row = |y: f32, h: f32| {
            Rect::new(
                modal.x + modal.width * 0.04,
                modal.y + modal.height * y,
                modal.width * 0.92,
                modal.height * h,
            )
        };
        line(
            "New workspace",
            row(0.02, 0.17),
            17.0,
            theme.text_primary,
            true,
        );
        for (control, rect) in controls(modal) {
            let active = control == form.focus;
            let label = form.label(control);
            line(
                &label,
                rect,
                14.0,
                if active {
                    theme.border_focused
                } else {
                    theme.text_primary
                },
                active,
            );
        }
        let message = form
            .error
            .as_deref()
            .unwrap_or("Tab changes focus · Enter creates · Esc cancels");
        line(
            message,
            row(0.59, 0.16),
            12.0,
            if form.error.is_some() {
                theme.status_err
            } else {
                theme.text_muted
            },
            false,
        );
    }
}
