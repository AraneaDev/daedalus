use super::{AppState, ChromeLineSpec, Rect, TextRenderer, Theme};
use crate::settings::{controls, modal_rect};

impl TextRenderer {
    pub(super) fn build_settings_items(&mut self, state: &AppState, theme: &Theme, w: f32, h: f32) {
        let dpi = self.scale_factor;
        let modal = modal_rect(w, h, dpi);
        let scale = dpi.min(modal.height / 324.0).min(modal.width / 420.0);
        let dialog = &state.settings;
        let mut line = |text: &str, rect: Rect, size: f32, color, bold| {
            let inset = 5.0 * scale;
            self.push_modal_line(ChromeLineSpec {
                text,
                left: rect.x + inset,
                top: rect.y + (rect.height - size * 1.4 * scale) * 0.5,
                max_w: (rect.width - 2.0 * inset).max(1.0),
                bounds: rect,
                font_size_logical: size * scale / dpi,
                line_height_logical: size * 1.4 * scale / dpi,
                color,
                bold,
            });
        };
        let row = |y: f32, height: f32| {
            Rect::new(
                modal.x + modal.width * 0.025,
                modal.y + y * modal.height,
                modal.width * 0.95,
                height * modal.height,
            )
        };
        line(
            &dialog.heading(),
            row(0.01, 0.13),
            17.0,
            theme.text_primary,
            true,
        );
        for (c, r) in controls(modal) {
            let active = dialog.focus == c;
            let prefix = if active { "▸ " } else { "" };
            let label = dialog.visible_label(c, (r.width / (9.0 * scale)) as usize);
            let color = if !dialog.enabled(c) {
                theme.text_muted
            } else if active {
                theme.border_focused
            } else {
                theme.text_primary
            };
            line(&format!("{prefix}{label}"), r, 14.0, color, active);
        }
        line(
            r#"Argv example   ["cargo", "test", "--workspace"]"#,
            row(0.66, 0.08),
            12.0,
            theme.text_muted,
            false,
        );
        let msg = dialog
            .error
            .as_deref()
            .unwrap_or("Tab moves focus · Enter activates · Ctrl+A replaces field");
        line(
            msg,
            row(0.74, 0.09),
            12.0,
            if dialog.error.is_some() {
                theme.status_err
            } else {
                theme.text_muted
            },
            false,
        );
    }
}
