use super::{Rect, WIDGET_BUTTON_GAP_LOGICAL, WIDGET_BUTTON_MIN_LOGICAL_WIDTH, WorkspaceLayout};
use daedalus_proto::StatusLevel;
use daedalus_proto::workbench::{
    WidgetDefinition, WidgetDensity, WidgetElement, WidgetResult, WidgetSnapshot,
    WidgetStatusPresentation, WorkspaceDocument,
};
use unicode_width::UnicodeWidthStr;

pub(crate) const STATUS_DOT_SIZE_LOGICAL: f32 = 8.;
const WIDGET_BUTTON_MAX_LOGICAL_WIDTH: f32 = 190.;
const CHROME_GLYPH_LOGICAL_WIDTH: f32 = 9.;
const UNAVAILABLE: &str = "Unavailable";

pub(crate) fn widget_text_inset(density: WidgetDensity) -> f32 {
    match density {
        WidgetDensity::Compact => 4.,
        WidgetDensity::Standard => 6.,
        WidgetDensity::Spacious => 9.,
    }
}

pub(crate) fn widget_status_text_inset(inset: f32) -> f32 {
    2. * inset + STATUS_DOT_SIZE_LOGICAL
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WidgetFooterCellKind {
    Text,
    Status(Option<StatusLevel>),
    Progress { value: Option<u64>, max: u64 },
    Button,
}

impl WidgetFooterCellKind {
    pub(crate) fn leading_text_inset(self, inset: f32, status: WidgetStatusPresentation) -> f32 {
        match (self, status) {
            (Self::Status(_), WidgetStatusPresentation::LightAndText) => {
                widget_status_text_inset(inset)
            }
            (Self::Status(_), WidgetStatusPresentation::Light) => inset + STATUS_DOT_SIZE_LOGICAL,
            _ => inset,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct WidgetFooterCell {
    pub bounds: Rect,
    pub text: String,
    pub kind: WidgetFooterCellKind,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WidgetFooterRow {
    pub widget: usize,
    pub cells: Vec<WidgetFooterCell>,
    pub(crate) text_inset: f32,
    pub(crate) status: WidgetStatusPresentation,
}

struct FooterSlot {
    widget: Option<usize>,
    width: f32,
    preferred_width: Option<f32>,
    row: Option<RowContent>,
}

impl FooterSlot {
    fn flexible(&self) -> bool {
        self.preferred_width.is_none() && self.row.is_none()
    }
}

struct RowContent {
    cells: Vec<CellContent>,
    gutter: f32,
    text_inset: f32,
    status: WidgetStatusPresentation,
}

impl RowContent {
    fn build(definition: &WidgetDefinition, result: Option<&WidgetResult>) -> Option<Self> {
        let display = &definition.display;
        if display.elements.is_empty() {
            return None;
        }
        let density = display.appearance.density;
        let text_inset = widget_text_inset(density);
        let status = display.appearance.status;
        let gutter = match density {
            WidgetDensity::Compact => 2.,
            WidgetDensity::Standard => 3.,
            WidgetDensity::Spacious => 5.,
        };
        let cells = display
            .elements
            .iter()
            .enumerate()
            .map(|(index, element)| {
                CellContent::build(definition, result, index, element, text_inset, status)
            })
            .collect();
        Some(Self {
            cells,
            gutter,
            text_inset,
            status,
        })
    }

    fn width(&self, scale: f32) -> f32 {
        let gutter = self.gutter * scale;
        self.cells
            .iter()
            .map(|cell| cell.min_logical_width * scale)
            .sum::<f32>()
            + 2. * gutter
            + (self.cells.len() as f32 - 1.) * gutter
    }

    fn into_row(self, widget: usize, bounds: Rect, scale: f32) -> WidgetFooterRow {
        let gutter = self.gutter * scale;
        let mut left = bounds.x + gutter;
        let cells = self
            .cells
            .into_iter()
            .map(|cell| {
                let width = cell.min_logical_width * scale;
                let bounds = Rect::new(left, bounds.y, width, bounds.height);
                left += width + gutter;
                WidgetFooterCell {
                    bounds,
                    text: cell.text,
                    kind: cell.kind,
                }
            })
            .collect();
        WidgetFooterRow {
            widget,
            cells,
            text_inset: self.text_inset,
            status: self.status,
        }
    }
}

struct CellContent {
    text: String,
    kind: WidgetFooterCellKind,
    min_logical_width: f32,
}

impl CellContent {
    fn build(
        definition: &WidgetDefinition,
        result: Option<&WidgetResult>,
        index: usize,
        element: &WidgetElement,
        text_inset: f32,
        status: WidgetStatusPresentation,
    ) -> Self {
        use WidgetFooterCellKind as Kind;
        // Only configured strings and fixed allowances determine the dimensions.
        // Refreshing a report must leave button hit targets in place.
        let (text, kind, reserved_text_width) = match element {
            WidgetElement::Label { text } => (text.clone(), Kind::Text, text_width(text)),
            WidgetElement::Status => {
                let level = result.map(|result| result.level);
                let text = result.map_or_else(|| "No status".to_owned(), WidgetResult::status_text);
                let width = if status == WidgetStatusPresentation::Light {
                    0.
                } else {
                    18. * CHROME_GLYPH_LOGICAL_WIDTH
                };
                (text, Kind::Status(level), width)
            }
            WidgetElement::Counter { field } => {
                let value = result
                    .and_then(|result| result.element_value(field))
                    .unwrap_or(UNAVAILABLE);
                let label = field.label.as_deref().unwrap_or(&field.field);
                (
                    format!("{label}: {value}"),
                    Kind::Text,
                    text_width(label) + text_width(": Unavailable"),
                )
            }
            WidgetElement::Progress { field, max } => {
                let value = result
                    .and_then(|result| result.element_value(field))
                    .and_then(|value| value.parse::<u64>().ok())
                    .filter(|_| *max > 0);
                let label = field.label.as_deref().unwrap_or(&field.field);
                let text = value.map_or_else(
                    || format!("{label}: {UNAVAILABLE}"),
                    |value| format!("{label}: {value}/{max}"),
                );
                let value_width = text_width(&format!(": {max}/{max}"));
                (
                    text,
                    Kind::Progress { value, max: *max },
                    text_width(label) + value_width.max(text_width(": Unavailable")),
                )
            }
            WidgetElement::Button { action } => {
                let (text, kind) = definition.element_action(index).map_or_else(
                    || (action.as_str(), Kind::Text),
                    |action| (action.label.as_str(), Kind::Button),
                );
                (text.to_owned(), kind, text_width(text))
            }
        };
        let minimum = match kind {
            Kind::Button => 46.,
            Kind::Progress { .. } => 66.,
            _ => 24.,
        };
        Self {
            text: text.split_whitespace().collect::<Vec<_>>().join(" "),
            kind,
            min_logical_width: (reserved_text_width
                + kind.leading_text_inset(text_inset, status)
                + text_inset)
                .max(minimum),
        }
    }
}

fn text_width(text: &str) -> f32 {
    UnicodeWidthStr::width(text) as f32 * CHROME_GLYPH_LOGICAL_WIDTH
}

impl WorkspaceLayout {
    pub fn widget_row(&self, index: Option<usize>) -> Option<&WidgetFooterRow> {
        let index = index?;
        self.widget_rows.iter().find(|row| row.widget == index)
    }

    pub fn set_widget_footer(
        &mut self,
        bar: Rect,
        document: &WorkspaceDocument,
        reports: &[WidgetSnapshot],
        scale: f32,
    ) {
        self.widget_buttons.clear();
        self.widget_rows.clear();
        let pinned: Vec<_> = document
            .widgets
            .iter()
            .enumerate()
            .filter(|(_, widget)| widget.pinned)
            .collect();
        self.widget_bar = (!pinned.is_empty()).then_some(bar);
        let gap = WIDGET_BUTTON_GAP_LOGICAL * scale;
        let start = self
            .cargo_buttons(scale)
            .last()
            .map_or(bar.x + gap, |(_, rect)| rect.right() + gap);
        let room = (bar.right() - start - gap).max(0.);
        let minimum = WIDGET_BUTTON_MIN_LOGICAL_WIDTH * scale;
        let max = ((room + gap) / (minimum + gap)) as usize;
        if max == 0 || pinned.is_empty() {
            return;
        }
        let overflow = pinned.len() > max;
        let visible = if overflow { max - 1 } else { pinned.len() };
        let count = visible + usize::from(overflow);
        let width_budget = room - (count as f32 - 1.) * gap;
        let mut spare = (width_budget - count as f32 * minimum).max(0.);
        let mut slots = Vec::with_capacity(count);

        // Reserve each compact slot and the minimum size for any composition that fits.
        for (index, widget) in pinned.into_iter().take(visible) {
            let mut slot = FooterSlot {
                widget: Some(index),
                width: minimum,
                preferred_width: None,
                row: None,
            };
            if let Some(definition) = document.widget_definition(widget) {
                slot.preferred_width = definition
                    .display
                    .appearance
                    .width
                    .map(|width| f32::from(width) * scale);
                let result = reports
                    .iter()
                    .find(|report| report.id == widget.id)
                    .map(|report| &report.result);
                if let Some(row) = RowContent::build(definition, result) {
                    let width = row.width(scale).max(minimum);
                    let extra = width - minimum;
                    if extra <= spare {
                        spare -= extra;
                        slot.width = width;
                        slot.row = Some(row);
                    }
                }
            }
            slots.push(slot);
        }
        if overflow {
            slots.push(FooterSlot {
                widget: None,
                width: minimum,
                preferred_width: None,
                row: None,
            });
        }

        // Apply preferences only after the other slots and compositions have room.
        for slot in &mut slots {
            if let Some(preferred_width) = slot.preferred_width {
                let extra = (preferred_width - slot.width).clamp(0., spare);
                slot.width += extra;
                spare -= extra;
            }
        }
        let flexible_count = slots.iter().filter(|slot| slot.flexible()).count().max(1);
        let flexible_width =
            (minimum + spare / flexible_count as f32).min(WIDGET_BUTTON_MAX_LOGICAL_WIDTH * scale);
        let mut x = start;
        for slot in slots {
            let width = if slot.flexible() {
                flexible_width
            } else {
                slot.width
            };
            let bounds = Rect::new(x, bar.y + gap, width, bar.height - gap * 2.);
            self.widget_buttons.push((slot.widget, bounds));
            if let (Some(widget), Some(row)) = (slot.widget, slot.row) {
                self.widget_rows.push(row.into_row(widget, bounds, scale));
            }
            x += width + gap;
        }
    }
}
