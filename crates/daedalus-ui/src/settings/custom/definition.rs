use super::{Control, SettingsDialog};
use Control::*;
use daedalus_proto::workbench::{CommandOutput, WidgetTemplate};
use daedalus_proto::{ClientMessage, WorkspaceId};

impl SettingsDialog {
    pub(super) fn definition_rows(&self, pages: Vec<Control>) -> Vec<Vec<Control>> {
        if self.custom.advanced {
            return vec![
                pages,
                vec![CustomDefinition],
                vec![CustomApplyDefinition],
                vec![CustomAdvanced, Cancel],
            ];
        }
        vec![
            pages,
            vec![CustomTemplate, CustomPin],
            vec![CustomName],
            vec![CustomExecutable, CustomPublish],
            vec![CustomArguments],
            vec![CustomInterval, CustomOutput],
            vec![CustomSummary],
            vec![CustomDetails],
            vec![CustomUrl],
            vec![CustomPreview, CustomSaveTemplate, CustomAdvanced],
            vec![CustomBack, CustomSave],
        ]
    }

    pub(super) fn definition_label(&self, control: Control) -> Option<String> {
        let draft = self.custom.draft.as_ref();
        Some(match control {
            CustomTemplate => format!(
                "Template · {} ↻",
                draft
                    .and_then(|draft| draft.template.as_ref())
                    .map(|template| template.name.as_str())
                    .or_else(|| draft.and_then(|draft| draft.widget.template.as_deref()))
                    .unwrap_or("Blank")
            ),
            CustomOutput => format!(
                "Output · {} ↻",
                match draft.map(|draft| &draft.output) {
                    Some(CommandOutput::Text { .. }) => "Text",
                    Some(CommandOutput::Json) => "JSON",
                    Some(CommandOutput::WidgetResult) => "Widget result",
                    _ => "Exit status",
                }
            ),
            CustomPreview => "Test preview".into(),
            CustomAdvanced => {
                if self.custom.advanced {
                    "Back to form".into()
                } else {
                    "Advanced".into()
                }
            }
            CustomApplyDefinition => "Apply definition to draft".into(),
            CustomSave => {
                if draft.is_some_and(|draft| draft.existing) {
                    "Save widget".into()
                } else {
                    "Add widget".into()
                }
            }
            CustomSaveTemplate => "Save as template".into(),
            _ => return None,
        })
    }

    pub(super) fn definition_command(
        &mut self,
        id: WorkspaceId,
        control: Control,
    ) -> Result<Option<ClientMessage>, String> {
        let editor = &mut self.custom;
        match control {
            CustomTemplate => editor.choose_template(),
            CustomAdvanced => {
                if editor.advanced {
                    editor.advanced = false;
                    self.focus = CustomAdvanced;
                } else if let Some(draft) = editor.draft.as_ref() {
                    let definition = draft
                        .build()
                        .ok()
                        .and_then(|widget| widget.definition)
                        .unwrap_or_else(|| draft.definition.clone());
                    editor.definition_json =
                        serde_json::to_string(&definition).map_err(|error| error.to_string())?;
                    editor.advanced = true;
                    self.focus = CustomDefinition;
                    self.select_all = true;
                }
            }
            CustomApplyDefinition => {
                let definition = serde_json::from_str(&editor.definition_json)
                    .map_err(|error| error.to_string())?;
                if let Some(draft) = editor.draft.as_mut() {
                    draft.replace_definition(definition);
                }
                editor.advanced = false;
                self.focus = CustomName;
            }
            CustomOutput => {
                if let Some(draft) = &mut editor.draft {
                    draft.change_source = true;
                    draft.output = match draft.output {
                        CommandOutput::ExitStatus => CommandOutput::Text { contains: None },
                        CommandOutput::Text { .. } => CommandOutput::Json,
                        CommandOutput::Json => CommandOutput::WidgetResult,
                        CommandOutput::WidgetResult => CommandOutput::ExitStatus,
                    };
                }
            }
            CustomPreview => {
                let widget = editor
                    .draft
                    .as_ref()
                    .ok_or("Open a widget to test it")?
                    .build()?;
                return Ok(Some(ClientMessage::PreviewWidget {
                    workspace_id: id,
                    name: widget.name,
                    definition: widget.definition.unwrap_or_default(),
                }));
            }
            CustomSave => return editor.save(id).map(Some),
            CustomSaveTemplate => {
                let widget = editor
                    .draft
                    .as_ref()
                    .ok_or("Open a widget to save it as a template")?
                    .build()?;
                let mut templates = editor.library.templates.clone();
                templates.push(WidgetTemplate {
                    id: uuid::Uuid::new_v4().to_string(),
                    name: widget.name,
                    definition: widget.definition.unwrap_or_default(),
                });
                return Ok(Some(ClientMessage::UpdateWidgetTemplates {
                    revision: editor.library.revision,
                    templates,
                }));
            }
            _ => {}
        }
        Ok(None)
    }
}
