use super::{Control, Rect, SetupReview};

pub fn modal_rect(w: f32, h: f32, dpi: f32) -> Rect {
    let width = (w - 12. * dpi).max(1.).min(700. * dpi);
    let height = (h - 12. * dpi).max(1.).min(500. * dpi);
    Rect::new((w - width) / 2., (h - height) / 2., width, height)
}

pub fn region(m: Rect, x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect::new(
        m.x + m.width * x,
        m.y + m.height * y,
        m.width * w,
        m.height * h,
    )
}

impl SetupReview {
    pub(super) fn ordered_controls(&self) -> Vec<Control> {
        let mut controls = Vec::new();
        for index in 0..3 {
            controls.extend([Control::Command(index), Control::Launch(index)]);
        }
        controls.push(Control::Agent);
        if self.tool().is_some() {
            controls.push(Control::Tool);
            if self.install().is_some() {
                controls.push(Control::Install);
            }
            if self.guide().is_some() {
                controls.push(Control::Guide);
            }
        }
        if self.dependency_install().is_some() {
            controls.push(Control::Dependencies);
        }
        controls.extend([
            Control::Back,
            Control::Cancel,
            Control::Check,
            Control::Open,
        ]);
        controls
    }

    pub fn controls(&self, modal: Rect) -> Vec<(Control, Rect)> {
        self.ordered_controls()
            .into_iter()
            .map(|control| {
                let (x, y, width, height) = match control {
                    Control::Command(index) => (0.035, 0.225 + index as f32 * 0.135, 0.70, 0.068),
                    Control::Launch(index) => (0.753, 0.225 + index as f32 * 0.135, 0.212, 0.068),
                    Control::Agent => (0.753, 0.153, 0.212, 0.056),
                    Control::Tool => (0.035, 0.615, 0.38, 0.071),
                    Control::Install => (0.43, 0.615, 0.24, 0.071),
                    Control::Guide => (0.69, 0.615, 0.275, 0.071),
                    Control::Dependencies => (0.035, 0.747, 0.93, 0.058),
                    Control::Back => (0.035, 0.875, 0.135, 0.09),
                    Control::Cancel => (0.183, 0.875, 0.155, 0.09),
                    Control::Check => (0.355, 0.875, 0.245, 0.09),
                    Control::Open => (0.62, 0.875, 0.345, 0.09),
                };
                (control, region(modal, x, y, width, height))
            })
            .collect()
    }
}
