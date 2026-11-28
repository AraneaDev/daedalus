use super::{Control, SettingsDialog};
use Control::*;
use daedalus_proto::workbench::{WidgetDefinition, WidgetElement, WidgetResult};
use daedalus_proto::{ClientMessage, StatusLevel, WorkspaceId};

const ACTION_PAGE_SIZE: usize = 8;

impl SettingsDialog {
    pub(super) fn detail_rows(&self, pages: Vec<Control>) -> Vec<Vec<Control>> {
        let mut rows = vec![pages];
        self.append_detail_rows(&mut rows);
        let actions = self.detail_actions();
        let columns = if self
            .selected_widget_definition()
            .is_some_and(|definition| !definition.display.elements.is_empty())
        {
            2
        } else {
            1
        };
        if let Some(page) = actions
            .chunks(ACTION_PAGE_SIZE)
            .nth(self.custom.detail_action_page)
        {
            rows.extend(
                page.chunks(columns)
                    .map(|row| row.iter().copied().map(CustomViewAction).collect()),
            );
        }
        if actions.len() > ACTION_PAGE_SIZE {
            rows.push(vec![CustomActionPrev, CustomActionNext]);
        }
        rows.push(vec![CustomRefresh, CustomEdit]);
        rows.push(vec![CustomBack, Cancel]);
        rows
    }

    pub(super) fn detail_label(&self, control: Control) -> Option<String> {
        Some(match control {
            CustomRefresh => "Refresh widget".into(),
            CustomDetailPrev => "‹ Previous lines".into(),
            CustomDetailNext => "Next lines ›".into(),
            CustomActionPrev => "‹ Previous actions".into(),
            CustomActionNext => "Next actions ›".into(),
            CustomDetail(index) if self.custom.sharing => self
                .custom
                .import_lines()
                .get(index)
                .cloned()
                .unwrap_or_default(),
            CustomDetail(index) => self.detail_line(index).unwrap_or_default(),
            CustomViewAction(index) => self
                .selected_widget_definition()
                .and_then(|definition| definition.actions.get(index))
                .map_or_else(String::new, |action| action.label.clone()),
            _ => return None,
        })
    }

    pub(super) fn detail_command(
        &mut self,
        id: WorkspaceId,
        control: Control,
    ) -> Result<Option<ClientMessage>, String> {
        let editor = &mut self.custom;
        match control {
            CustomDetailPrev => editor.detail_page = editor.detail_page.saturating_sub(1),
            CustomDetailNext => editor.detail_page += 1,
            CustomActionPrev => {
                editor.detail_action_page = editor.detail_action_page.saturating_sub(1)
            }
            CustomActionNext => editor.detail_action_page += 1,
            CustomRefresh => {
                editor.action_output = false;
                editor.detail_page = 0;
                let widget = editor.selected().ok_or("Select a widget to refresh")?;
                return Ok(Some(ClientMessage::GetWidgetResults {
                    workspace_id: id,
                    refresh: Some(widget.id.clone()),
                }));
            }
            CustomViewAction(index) => {
                let widget = editor.selected().ok_or("Select a widget")?;
                let definition = editor
                    .state
                    .as_ref()
                    .and_then(|state| state.document.widget_definition(widget))
                    .ok_or("No widget definition")?;
                let action = definition.actions.get(index).ok_or("Select an action")?;
                return Ok(Some(ClientMessage::InvokeWidgetAction {
                    workspace_id: id,
                    widget: widget.id.clone(),
                    action: action.id.clone(),
                }));
            }
            _ => {}
        }
        Ok(None)
    }

    fn selected_widget_definition(&self) -> Option<&WidgetDefinition> {
        self.custom
            .state
            .as_ref()?
            .document
            .widget_definition(self.custom.selected()?)
    }

    fn detail_actions(&self) -> Vec<usize> {
        let Some(definition) = self.selected_widget_definition() else {
            return Vec::new();
        };
        let mut indices = Vec::new();
        for element in &definition.display.elements {
            if let WidgetElement::Button { action } = element
                && let Some(index) = definition
                    .actions
                    .iter()
                    .position(|named| named.id == *action)
                && !indices.contains(&index)
            {
                indices.push(index);
            }
        }
        let primary = definition.primary_action();
        for (index, action) in definition.actions.iter().enumerate() {
            if (index < 4 || primary == Some(action)) && !indices.contains(&index) {
                indices.push(index);
            }
        }
        indices
    }

    pub(super) fn has_next_action_page(&self) -> bool {
        (self.custom.detail_action_page + 1) * ACTION_PAGE_SIZE < self.detail_actions().len()
    }

    fn composed_details(&self) -> impl Iterator<Item = &WidgetElement> {
        self.selected_widget_definition()
            .filter(|_| !self.custom.action_output)
            .into_iter()
            .flat_map(|definition| &definition.display.elements)
            .filter(|element| !matches!(element, WidgetElement::Button { .. }))
    }

    pub(in crate::settings) fn is_composed_detail(&self, index: usize) -> bool {
        self.composed_details().nth(index).is_some()
    }

    fn detail_line(&self, index: usize) -> Option<String> {
        let mut composed = self.composed_details();
        let count = self.composed_details().count();
        let result = self.custom.preview.as_ref();
        if index < count {
            return composed
                .nth(index)
                .and_then(|element| composed_detail(element, result));
        }
        result?
            .details
            .iter()
            .flat_map(|(label, value)| {
                value.lines().enumerate().map(move |(line, text)| {
                    if line == 0 {
                        format!("{label} · {text}")
                    } else {
                        text.to_owned()
                    }
                })
            })
            .nth(index - count)
    }

    pub fn update_widget_detail_result(&mut self, result: WidgetResult) {
        self.custom.preview = Some(result);
        if self.custom.detail_page > 0 {
            let last_page = self.detail_line_count().saturating_sub(1) / self.detail_page_size();
            self.custom.detail_page = self.custom.detail_page.min(last_page);
        }
    }

    pub(in crate::settings) fn detail_line_count(&self) -> usize {
        if self.custom.sharing {
            return self.custom.import_lines().len();
        }
        self.composed_details().count()
            + self.custom.preview.as_ref().map_or(0, |result| {
                result
                    .details
                    .iter()
                    .map(|(_, value)| value.lines().count())
                    .sum()
            })
    }

    pub(in crate::settings) fn detail_page_size(&self) -> usize {
        if self.custom.sharing { 6 } else { 8 }
    }

    pub(super) fn append_detail_rows(&self, rows: &mut Vec<Vec<Control>>) {
        let lines = self.detail_line_count();
        let page_size = self.detail_page_size();
        let start = self.custom.detail_page * page_size;
        rows.extend((start..(start + page_size).min(lines)).map(|index| vec![CustomDetail(index)]));
        if lines > page_size {
            rows.push(vec![CustomDetailPrev, CustomDetailNext]);
        }
    }
}

fn composed_detail(element: &WidgetElement, result: Option<&WidgetResult>) -> Option<String> {
    match element {
        WidgetElement::Label { text } => Some(text.clone()),
        WidgetElement::Status => Some(match result {
            Some(result) => {
                let level = match result.level {
                    StatusLevel::Info => "Info",
                    StatusLevel::Ok => "OK",
                    StatusLevel::Warn => "Warning",
                    StatusLevel::Error => "Error",
                };
                format!("Status · {} ({level})", result.label)
            }
            None => "Status · No status".into(),
        }),
        WidgetElement::Counter { field } | WidgetElement::Progress { field, .. } => {
            let label = field.label.as_deref().unwrap_or(&field.field);
            let value = result.and_then(|result| result.element_value(field));
            Some(match element {
                WidgetElement::Progress { max, .. } => value
                    .and_then(|value| value.parse::<u64>().ok())
                    .map_or_else(
                        || format!("{label} · Unavailable"),
                        |value| format!("{label} · {value} / {max}"),
                    ),
                _ => format!("{label} · {}", value.unwrap_or("Unavailable")),
            })
        }
        WidgetElement::Button { .. } => None,
    }
}
