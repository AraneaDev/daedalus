use super::{AppState, Rgba, Theme, push_control, quads::QuadInstance};
use crate::layout::Rect;
use crate::layout::footer::{
    STATUS_DOT_SIZE_LOGICAL, WidgetFooterCellKind, WidgetFooterRow, widget_text_inset,
};
use daedalus_proto::StatusLevel;
use daedalus_proto::workbench::{
    NamedAction, WidgetDisplay, WidgetResult, WidgetStatusPresentation, WidgetStyle, WidgetSurface,
};

pub(super) struct FooterWidget<'a> {
    name: &'a str,
    display: Option<&'a WidgetDisplay>,
    pub(super) action: Option<&'a NamedAction>,
    result: Option<&'a WidgetResult>,
}

impl<'a> FooterWidget<'a> {
    pub(super) fn get(state: &'a AppState, index: Option<usize>) -> Option<Self> {
        let document = &state.workbench.as_ref()?.1.document;
        let widget = document.widgets.get(index?)?;
        let definition = document.widget_definition(widget);
        Some(Self {
            name: &widget.name,
            display: definition.map(|definition| &definition.display),
            action: definition
                .filter(|definition| definition.display.elements.is_empty())
                .and_then(|definition| definition.primary_action()),
            result: state
                .widget_results
                .iter()
                .find(|report| report.id == widget.id)
                .map(|report| &report.result),
        })
    }

    pub(super) fn style(&self) -> WidgetStyle {
        self.display
            .filter(|display| display.elements.is_empty())
            .map_or(WidgetStyle::default(), |display| display.style)
    }

    pub(super) fn title(&self) -> String {
        let action = self.action.map(|action| action.label.as_str());
        let status = self.result.map(|result| result.label.as_str());
        let style = self.style();
        let label = if self
            .display
            .is_some_and(|display| !display.elements.is_empty())
        {
            self.name
        } else {
            match style {
                WidgetStyle::Led => action.unwrap_or(self.name),
                _ => action.or(status).unwrap_or(self.name),
            }
        };
        let title = self
            .display
            .and_then(|display| display.icon.as_deref())
            .map_or_else(|| label.to_owned(), |icon| format!("{icon} {label}"));
        if style == WidgetStyle::Led
            && self.status_presentation() != WidgetStatusPresentation::Light
        {
            let status_text = self
                .result
                .map_or_else(|| "No status".into(), WidgetResult::status_text);
            if status == Some(title.as_str()) {
                status_text
            } else {
                format!("{status_text} · {title}")
            }
        } else {
            title
        }
    }

    pub(super) fn status_color(&self, theme: &Theme, neutral: Rgba) -> Rgba {
        status_color(self.result.map(|result| result.level), theme, neutral)
    }

    pub(super) fn text_inset(&self) -> f32 {
        widget_text_inset(
            self.display
                .map_or(Default::default(), |display| display.appearance.density),
        )
    }

    pub(super) fn status_presentation(&self) -> WidgetStatusPresentation {
        self.display
            .map_or(Default::default(), |display| display.appearance.status)
    }

    pub(super) fn shows_status_light(&self) -> bool {
        self.style() == WidgetStyle::Led
            && self.status_presentation() != WidgetStatusPresentation::Text
    }

    pub(super) fn paint(
        &self,
        quads: &mut Vec<QuadInstance>,
        bounds: Rect,
        row: Option<&WidgetFooterRow>,
        theme: &Theme,
        scale: f32,
    ) {
        let (surface, custom_accent) =
            self.display
                .map_or((WidgetSurface::Automatic, None), |display| {
                    let appearance = &display.appearance;
                    let accent = appearance
                        .accent_rgb()
                        .map(|[r, g, b]| Rgba::from_rgb_u8(r, g, b));
                    (appearance.surface, accent)
                });
        let accent = custom_accent.unwrap_or(theme.border_focused);
        let (fill, edge) = match (surface, self.style()) {
            (WidgetSurface::Flat, _) => (None, None),
            (WidgetSurface::Outline, _) => (None, Some(accent)),
            (WidgetSurface::Automatic, WidgetStyle::Button) => {
                (Some(accent.with_alpha(0.18)), Some(accent))
            }
            (WidgetSurface::Automatic, WidgetStyle::Led) => (None, custom_accent),
            (WidgetSurface::Automatic, _) => (Some(theme.bg_sidebar), custom_accent),
        };
        if let Some(fill) = fill {
            quads.push(QuadInstance::new(bounds, fill).rounded(5. * scale, 0., 0.));
        }
        if let Some(edge) = edge {
            quads.push(QuadInstance::new(bounds, edge).rounded(5. * scale, scale, 0.));
        }
        if let Some(row) = row {
            paint_cells(quads, row, theme, accent, scale);
        } else if self.shows_status_light() {
            paint_status_light(
                quads,
                bounds,
                self.text_inset() * scale,
                self.status_color(theme, theme.text_muted),
                scale,
            );
        }
    }
}

pub(super) fn status_color(level: Option<StatusLevel>, theme: &Theme, neutral: Rgba) -> Rgba {
    match level {
        Some(StatusLevel::Ok) => theme.status_ok,
        Some(StatusLevel::Warn) => theme.status_busy,
        Some(StatusLevel::Error) => theme.status_err,
        _ => neutral,
    }
}

fn paint_cells(
    quads: &mut Vec<QuadInstance>,
    row: &WidgetFooterRow,
    theme: &Theme,
    accent: Rgba,
    scale: f32,
) {
    let inset = row.text_inset * scale;
    for cell in &row.cells {
        let bounds = cell.bounds;
        match cell.kind {
            WidgetFooterCellKind::Button => push_control(
                quads,
                bounds,
                accent.with_alpha(0.18),
                accent,
                scale,
                5. * scale,
            ),
            WidgetFooterCellKind::Progress { value, max } => {
                let track = Rect::new(
                    bounds.x + inset,
                    bounds.bottom() - 4. * scale,
                    bounds.width - 2. * inset,
                    2. * scale,
                );
                quads.push(QuadInstance::new(track, theme.border_unfocused));
                if let Some(value) = value.filter(|_| max > 0) {
                    let width = track.width * (value.min(max) as f32 / max as f32);
                    if width > 0. {
                        let filled = Rect { width, ..track };
                        quads.push(QuadInstance::new(filled, accent));
                    }
                }
            }
            WidgetFooterCellKind::Status(level) if row.status != WidgetStatusPresentation::Text => {
                paint_status_light(
                    quads,
                    bounds,
                    inset,
                    status_color(level, theme, theme.text_muted),
                    scale,
                );
            }
            _ => {}
        }
    }
}

fn paint_status_light(
    quads: &mut Vec<QuadInstance>,
    bounds: Rect,
    inset: f32,
    color: Rgba,
    scale: f32,
) {
    let size = STATUS_DOT_SIZE_LOGICAL * scale;
    let dot = Rect::new(
        bounds.x + inset,
        bounds.y + (bounds.height - size) * 0.5,
        size,
        size,
    );
    quads.push(QuadInstance::new(dot, color).rounded(size * 0.5, 0., 0.));
}
