use super::{CONTROLS, Control, WorkspaceForm};
use crate::layout::Rect;

/// Logical height of the new-project form without the recent list.
const FORM_HEIGHT: f32 = 236.0;
/// Logical heights of the recent list: its heading band, each row, and the gap below it.
const RECENT_HEADING: f32 = 40.0;
const RECENT_ROW: f32 = 34.0;
const RECENT_GAP: f32 = 6.0;

fn region(area: Rect, x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect::new(
        area.x + x * area.width,
        area.y + y * area.height,
        w * area.width,
        h * area.height,
    )
}

impl WorkspaceForm {
    fn recent_height(&self) -> f32 {
        if self.recent().is_empty() {
            0.0
        } else {
            RECENT_HEADING + RECENT_ROW * self.recent().len() as f32 + RECENT_GAP
        }
    }

    pub fn modal_rect(&self, w: f32, h: f32, dpi: f32) -> Rect {
        let width = (w - 16.0 * dpi).max(1.0).min(520.0 * dpi);
        let height = (h - 16.0 * dpi)
            .max(1.0)
            .min((FORM_HEIGHT + self.recent_height()) * dpi);
        Rect::new((w - width) / 2.0, (h - height) / 2.0, width, height)
    }

    /// The new-project part of the dialog, below the recent list when there is one.
    pub fn form_area(&self, modal: Rect) -> Rect {
        let height = modal.height * FORM_HEIGHT / (FORM_HEIGHT + self.recent_height());
        Rect::new(modal.x, modal.bottom() - height, modal.width, height)
    }

    /// The band that holds the "Recent projects" heading, when the list is shown.
    pub fn recent_heading(&self, modal: Rect) -> Option<Rect> {
        (!self.recent().is_empty()).then(|| {
            let unit = (modal.height - self.form_area(modal).height) / self.recent_height();
            Rect::new(
                modal.x + 0.04 * modal.width,
                modal.y,
                0.92 * modal.width,
                RECENT_HEADING * unit,
            )
        })
    }

    pub fn controls(&self, modal: Rect) -> Vec<(Control, Rect)> {
        let form = self.form_area(modal);
        let unit = (modal.height - form.height) / self.recent_height().max(1.0);
        let rows = (0..self.recent().len()).map(|i| {
            let top = RECENT_HEADING + RECENT_ROW * i as f32 + 2.0;
            (
                Control::Recent(i),
                Rect::new(
                    modal.x + 0.04 * modal.width,
                    modal.y + top * unit,
                    0.92 * modal.width,
                    (RECENT_ROW - 4.0) * unit,
                ),
            )
        });
        let fields = CONTROLS.into_iter().zip([
            (0.04, 0.21, 0.92, 0.18),
            (0.04, 0.40, 0.92, 0.18),
            (0.04, 0.77, 0.44, 0.17),
            (0.52, 0.77, 0.44, 0.17),
        ]);
        rows.chain(fields.map(|(control, (x, y, w, h))| (control, region(form, x, y, w, h))))
            .collect()
    }
}
