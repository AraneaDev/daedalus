use super::{AppState, ChromeLineSpec, Rect, TextRenderer, Theme, truncate_right_with_ellipsis};
use crate::setup_review::{Control, modal_rect, region, role_label};
use unicode_width::UnicodeWidthStr;

impl TextRenderer {
    pub(super) fn build_setup_review_items(
        &mut self,
        state: &AppState,
        theme: &Theme,
        w: f32,
        h: f32,
    ) {
        let review = &state.setup_review;
        let dpi = self.scale_factor;
        let modal = modal_rect(w, h, dpi);
        let scale = dpi.min(modal.width / 400.).min(modal.height / 405.);
        let glyph = self.cell_w / (self.font_size_logical * dpi);
        let mut line = |text: &str, rect: Rect, size: f32, color, bold| {
            let inset = 6. * scale;
            let max_w = (rect.width - 2. * inset).max(1.);
            let columns = (max_w / (glyph * size * scale)) as usize;
            let label = truncate_right_with_ellipsis(text, columns);
            self.push_modal_line(ChromeLineSpec {
                text: &label,
                bounds: rect,
                left: rect.x + inset,
                top: rect.y + (rect.height - size * 1.4 * scale) / 2.,
                max_w,
                font_size_logical: size * scale / dpi,
                line_height_logical: size * 1.4 * scale / dpi,
                color,
                bold,
            });
        };
        line(
            "Review environment",
            region(modal, 0.035, 0.012, 0.93, 0.072),
            19.,
            theme.text_primary,
            true,
        );
        line(
            &format!("{}  ·  {}", review.name, review.host()),
            region(modal, 0.035, 0.084, 0.93, 0.052),
            13.,
            theme.text_muted,
            false,
        );
        for i in 0..3 {
            let status = review.status(i);
            line(
                &format!("{}   ·   {}", role_label(i), status),
                region(modal, 0.035, 0.165 + i as f32 * 0.135, 0.65, 0.056),
                13.,
                if matches!(status, "Missing" | "Needs check" | "Incomplete command") {
                    theme.status_busy
                } else {
                    theme.text_primary
                },
                true,
            );
        }
        for (control, rect) in review.controls(modal) {
            let active = review.focus == control;
            let text = match control {
                Control::Command(i) if active => {
                    let max = ((rect.width - 12. * scale) / (glyph * 13. * scale)) as usize;
                    let value = review.command(i);
                    let before = super::truncate_left_with_ellipsis(
                        &value[..review.caret()],
                        max.saturating_sub(1),
                    );
                    let after = truncate_right_with_ellipsis(
                        &value[review.caret()..],
                        max.saturating_sub(before.width() + 1),
                    );
                    format!("{before}│{after}")
                }
                Control::Command(i) if review.command(i).is_empty() => "Add command…".into(),
                _ => review.label(control),
            };
            let color = if !review.enabled(control) {
                theme.text_muted
            } else if active || control == Control::Open {
                theme.border_focused
            } else {
                theme.text_primary
            };
            line(&text, rect, 13., color, active || control == Control::Open);
        }
        line(
            &review.detail(),
            region(modal, 0.035, 0.687, 0.93, 0.057),
            12.,
            theme.text_muted,
            false,
        );
        line(
            &review.feedback,
            region(modal, 0.035, 0.806, 0.93, 0.067),
            12.,
            theme.status_busy,
            false,
        );
    }
}
