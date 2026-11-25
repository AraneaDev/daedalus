use super::{Control, SettingsDialog};
use Control::*;
use daedalus_proto::{ClientMessage, WorkspaceId};

impl SettingsDialog {
    pub(super) fn detail_rows(&self, pages: Vec<Control>) -> Vec<Vec<Control>> {
        let mut rows = vec![pages];
        let lines = self.detail_line_count();
        let start = self.custom.detail_page * 8;
        rows.extend((start..(start + 8).min(lines)).map(|index| vec![CustomDetail(index)]));
        if lines > 8 {
            rows.push(vec![CustomDetailPrev, CustomDetailNext]);
        }
        if let Some(definition) = self.selected_widget_definition() {
            rows.extend(
                (0..definition.actions.len().min(4)).map(|index| vec![CustomViewAction(index)]),
            );
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
            CustomDetail(index) => self
                .custom
                .preview
                .as_ref()
                .and_then(|result| {
                    result
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
                        .nth(index)
                })
                .unwrap_or_default(),
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

    fn selected_widget_definition(&self) -> Option<&daedalus_proto::workbench::WidgetDefinition> {
        self.custom
            .state
            .as_ref()?
            .document
            .widget_definition(self.custom.selected()?)
    }

    pub(super) fn detail_line_count(&self) -> usize {
        self.custom.preview.as_ref().map_or(0, |result| {
            result
                .details
                .iter()
                .map(|(_, value)| value.lines().count())
                .sum()
        })
    }
}
