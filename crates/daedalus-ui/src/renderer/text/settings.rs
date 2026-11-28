use super::{
    AppState, CHROME_FONT_SIZE_LOGICAL, CHROME_SUB_FONT_SIZE_LOGICAL, ChromeLineSpec, Rect,
    TextRenderer, Theme,
};

impl TextRenderer {
    pub(super) fn build_settings_items(&mut self, state: &AppState, theme: &Theme, w: f32, h: f32) {
        let dpi = self.scale_factor;
        let modal = state.settings.modal_rect(w, h, dpi);
        let scale = dpi
            .min(state.settings.content_scale(modal))
            .min(modal.width / 420.0);
        let dialog = &state.settings;
        let glyph_width =
            self.cell_w * (CHROME_FONT_SIZE_LOGICAL * scale) / (self.font_size_logical * dpi);
        let mut line = |text: &str, rect: Rect, size: f32, color, bold, centered| {
            let inset = 11.0 * scale;
            let spec = ChromeLineSpec {
                text,
                left: rect.x + inset,
                top: rect.y + (rect.height - size * 1.4 * scale) * 0.5,
                max_w: (rect.width - 2.0 * inset).max(1.0),
                bounds: rect,
                font_size_logical: size * scale / dpi,
                line_height_logical: size * 1.4 * scale / dpi,
                color,
                bold,
            };
            if centered {
                self.push_modal_button(spec);
            } else {
                self.push_modal_line(spec);
            }
        };
        let unit = dialog.content_scale(modal);
        let row = |y: f32, height: f32| {
            Rect::new(
                modal.x + modal.width * 0.025,
                modal.y + y,
                modal.width * 0.95,
                height,
            )
        };
        let heading = row(0.0, 48.0 * unit);
        let count_width = (105.0 * scale).min(heading.width * 0.30);
        let custom_heading = dialog.is_custom_page().then(|| dialog.heading());
        line(
            if let Some(title) = &custom_heading {
                title
            } else if dialog.is_pane_widgets_page() {
                "Workspace widgets"
            } else {
                "Workspace assignments"
            },
            Rect::new(
                heading.x,
                heading.y,
                heading.width - count_width,
                heading.height,
            ),
            super::MODAL_TITLE_FONT_SIZE_LOGICAL,
            theme.text_primary,
            true,
            false,
        );
        line(
            &dialog.position_label(),
            Rect::new(
                heading.right() - count_width,
                heading.y,
                count_width,
                heading.height,
            ),
            super::CHROME_SUB_FONT_SIZE_LOGICAL,
            theme.text_muted,
            false,
            true,
        );
        for (c, r) in dialog.controls(modal) {
            let active = dialog.focus == c || dialog.page_selected(c);

            if let Some(caption) = c
                .field_label()
                .or_else(|| (c == crate::settings::Control::Role).then(|| "Pane".to_owned()))
            {
                let caption_width = if dialog.is_custom_page() {
                    // Line up the editor's values even when neighboring captions differ in length.
                    // The standalone import path and JSON field carry longer descriptions.
                    let width = match c {
                        crate::settings::Control::CustomRoot
                        | crate::settings::Control::CustomDefinition => {
                            caption.chars().count() as f32 * 8.0 + 24.0
                        }
                        _ => 160.0,
                    };
                    (width * scale).min(r.width * 0.68)
                } else {
                    (100.0 * scale).min(r.width * 0.38)
                };
                let value = Rect::new(r.x + caption_width, r.y, r.width - caption_width, r.height);
                let columns = ((value.width - 22.0 * scale) / glyph_width).max(1.0) as usize;
                let display = dialog.visible_label(c, columns + caption.len() + 3);
                let value_text = display
                    .strip_prefix(&format!("{caption}   "))
                    .unwrap_or(&display);
                line(
                    &caption,
                    Rect::new(r.x, r.y, caption_width, r.height),
                    CHROME_SUB_FONT_SIZE_LOGICAL,
                    theme.text_muted,
                    false,
                    false,
                );
                line(
                    value_text,
                    value,
                    CHROME_FONT_SIZE_LOGICAL,
                    theme.text_primary,
                    active,
                    false,
                );
                continue;
            }
            let label = dialog.label(c);
            let read_only = dialog.is_read_only_text(c);
            let enabled = dialog.enabled(c);
            let color = if read_only {
                theme.text_primary
            } else if !enabled {
                theme.text_muted
            } else if c.primary() {
                theme.bg_window
            } else if active {
                theme.border_focused
            } else {
                theme.text_primary
            };
            line(
                &label,
                r,
                CHROME_FONT_SIZE_LOGICAL,
                color,
                active || (enabled && c.primary()),
                !read_only,
            );
        }
        line(
            dialog.hint(),
            row(modal.height - 87.0 * unit, 32.0 * unit),
            CHROME_SUB_FONT_SIZE_LOGICAL,
            if dialog.error.is_some() {
                theme.status_err
            } else {
                theme.text_muted
            },
            false,
            false,
        );
    }
}
