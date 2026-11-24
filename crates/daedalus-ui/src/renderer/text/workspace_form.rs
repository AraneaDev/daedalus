use super::{AppState, ChromeLineSpec, Rect, TextRenderer, Theme};

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// The dialog has room for two feedback lines between the folder field and the buttons.
fn error_lines(message: &str, columns: usize) -> Vec<String> {
    // This daemon validation includes the entire path already shown in the Folder field.
    let message = if message
        .starts_with("invalid request: root_dir does not exist or is not a directory: ")
    {
        "Choose an existing folder on the daemon host."
    } else {
        message
    };
    let columns = columns.max(1);
    if message.width() <= columns {
        return vec![message.to_owned()];
    }
    let mut width = 0;
    let mut end = 0;
    let mut space = None;
    for (index, grapheme) in message.grapheme_indices(true) {
        width += grapheme.width();
        if width > columns {
            break;
        }
        if grapheme.chars().all(char::is_whitespace) && index > 0 {
            space = Some(index);
        }
        end = index + grapheme.len();
    }
    let (first, rest) = message.split_at(space.unwrap_or(end));
    vec![
        first.trim_end().to_owned(),
        super::truncate_right_with_ellipsis(rest.trim_start(), columns),
    ]
}

impl TextRenderer {
    pub(super) fn build_workspace_form_items(
        &mut self,
        state: &AppState,
        theme: &Theme,
        w: f32,
        h: f32,
    ) {
        let dpi = self.scale_factor;
        let form = &state.workspace_form;
        let modal = form.modal_rect(w, h, dpi);
        let area = form.form_area(modal);
        let scale = dpi.min(area.height / 236.0).min(modal.width / 330.0);
        let glyph_width = self.cell_w * (14.0 * scale) / (self.font_size_logical * dpi);
        let mut line = |text: &str, rect: Rect, size: f32, color, bold, centered| {
            let inset = 4.0 * scale;
            let spec = ChromeLineSpec {
                text,
                left: rect.x + inset,
                top: rect.y + (rect.height - size * 1.4 * scale) / 2.0,
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
        let row = |y: f32, h: f32| {
            Rect::new(
                area.x + area.width * 0.04,
                area.y + area.height * y,
                area.width * 0.92,
                area.height * h,
            )
        };
        if let Some(band) = form.recent_heading(modal) {
            line(
                "Recent projects",
                band,
                super::MODAL_TITLE_FONT_SIZE_LOGICAL,
                theme.text_primary,
                true,
                false,
            );
        }
        line(
            form.heading(),
            row(0.02, 0.17),
            super::MODAL_TITLE_FONT_SIZE_LOGICAL,
            theme.text_primary,
            true,
            false,
        );
        for (control, rect) in form.controls(modal) {
            let active = control == form.focus;
            let columns = ((rect.width - 8.0 * scale) / glyph_width) as usize;
            let label = form.visible_label(control, columns);
            line(
                &label,
                rect,
                14.0,
                if !form.enabled(control) {
                    theme.text_muted
                } else if active {
                    theme.border_focused
                } else {
                    theme.text_primary
                },
                active,
                matches!(
                    control,
                    crate::workspace_form::Control::Cancel | crate::workspace_form::Control::Save
                ),
            );
        }
        let feedback = row(0.59, 0.16);
        if let Some(error) = &form.error {
            let columns = ((feedback.width - 8.0 * scale) / (glyph_width * 12.0 / 14.0)) as usize;
            let lines = error_lines(error, columns);
            let height = feedback.height / lines.len() as f32;
            for (index, text) in lines.iter().enumerate() {
                let rect = Rect::new(
                    feedback.x,
                    feedback.y + index as f32 * height,
                    feedback.width,
                    height,
                );
                line(text, rect, 12.0, theme.status_err, false, false);
            }
        } else {
            line(form.hint(), feedback, 12.0, theme.text_muted, false, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_creation_errors_keep_the_remedy_and_fit_unicode_in_two_lines() {
        let missing =
            "invalid request: root_dir does not exist or is not a directory: /very/deep/project";
        assert_eq!(
            error_lines(missing, 38).join(" "),
            "Choose an existing folder on the daemon host."
        );
        assert_eq!(
            error_lines("Reconnect to create this workspace.", 40),
            ["Reconnect to create this workspace."]
        );
        let lines = error_lines("失敗：日本語のフォルダー名が見つかりませんでした", 12);
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|line| line.width() <= 12));
        assert!(lines[1].ends_with('…'));
    }
}
