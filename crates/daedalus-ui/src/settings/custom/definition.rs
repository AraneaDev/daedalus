use super::{Control, SettingsDialog};
use crate::widget_editor::AppearanceInputError;
use Control::*;
use daedalus_proto::workbench::{
    CommandOutput, WidgetDensity, WidgetStatusPresentation, WidgetStyle, WidgetSurface,
    WidgetTemplate,
};
use daedalus_proto::{ClientMessage, WorkspaceId};

impl SettingsDialog {
    pub(super) fn definition_rows(&self, pages: Vec<Control>) -> Vec<Vec<Control>> {
        if let Some(draft) = &self.custom.draft
            && draft.editing_element.is_some()
        {
            return self.element_rows(pages, draft);
        }
        if self.custom.advanced {
            return vec![
                pages,
                vec![CustomDefinition],
                vec![CustomApplyDefinition],
                vec![CustomAdvanced, Cancel],
            ];
        }
        if self
            .custom
            .draft
            .as_ref()
            .is_some_and(|draft| draft.editing_appearance)
        {
            return vec![
                pages,
                vec![CustomStyle, CustomDensity],
                vec![CustomSurface, CustomStatusPresentation],
                vec![CustomAccent],
                vec![CustomIcon, CustomWidth],
                vec![CustomAppearanceBack, CustomSave],
            ];
        }
        let mut presentation = vec![CustomAppearance, CustomElements];
        if self
            .custom
            .draft
            .as_ref()
            .is_none_or(|draft| draft.elements.is_empty())
        {
            presentation.insert(1, CustomPrimaryAction);
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
            presentation,
            vec![CustomPreview, CustomSaveTemplate, CustomAdvanced],
            vec![CustomBack, CustomSave],
        ]
    }

    pub(super) fn definition_label(&self, control: Control) -> Option<String> {
        let draft = self.custom.draft.as_ref();
        let appearance = draft.map(|draft| &draft.definition.display.appearance);
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
            CustomAppearance => "Appearance".into(),
            CustomAppearanceBack => "Back to form".into(),
            CustomDensity => format!(
                "Density · {} ↻",
                match appearance
                    .map(|appearance| appearance.density)
                    .unwrap_or_default()
                {
                    WidgetDensity::Compact => "Compact",
                    WidgetDensity::Standard => "Standard",
                    WidgetDensity::Spacious => "Spacious",
                }
            ),
            CustomSurface => format!(
                "Surface · {} ↻",
                match appearance
                    .map(|appearance| appearance.surface)
                    .unwrap_or_default()
                {
                    WidgetSurface::Automatic => "Automatic",
                    WidgetSurface::Flat => "Flat",
                    WidgetSurface::Outline => "Outline",
                }
            ),
            CustomStatusPresentation => format!(
                "Status · {} ↻",
                match appearance
                    .map(|appearance| appearance.status)
                    .unwrap_or_default()
                {
                    WidgetStatusPresentation::LightAndText => "Light + text",
                    WidgetStatusPresentation::Light => "Light only",
                    WidgetStatusPresentation::Text => "Text only",
                }
            ),
            CustomStyle => format!(
                "Style · {} ↻",
                match draft
                    .map(|draft| draft.definition.display.style)
                    .unwrap_or_default()
                {
                    WidgetStyle::Badge => "Badge",
                    WidgetStyle::Button => "Button",
                    WidgetStyle::Led => "LED",
                }
            ),
            CustomPrimaryAction => format!(
                "On click · {} ↻",
                draft
                    .and_then(|draft| draft.primary_action_label())
                    .unwrap_or_else(|| "View details".into())
            ),
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
        if (matches!(control, CustomSave | CustomSaveTemplate | CustomPreview)
            || (control == CustomAdvanced && !editor.advanced))
            && let Some(draft) = &mut editor.draft
            && let Err(error) = draft.validated_appearance()
        {
            let field = match error {
                AppearanceInputError::Accent => CustomAccent,
                AppearanceInputError::Width => CustomWidth,
            };
            draft.editing_appearance = true;
            self.focus = field;
            self.select_all = true;
            self.invalid_field = Some(field);
            return Err(error.to_string());
        }
        match control {
            CustomTemplate => editor.choose_template(),
            CustomAppearance | CustomAppearanceBack => {
                if let Some(draft) = &mut editor.draft {
                    draft.editing_appearance = control == CustomAppearance;
                    self.focus = if draft.editing_appearance {
                        CustomStyle
                    } else {
                        CustomAppearance
                    };
                }
            }
            CustomDensity => {
                if let Some(draft) = &mut editor.draft {
                    let density = &mut draft.definition.display.appearance.density;
                    *density = match *density {
                        WidgetDensity::Compact => WidgetDensity::Standard,
                        WidgetDensity::Standard => WidgetDensity::Spacious,
                        WidgetDensity::Spacious => WidgetDensity::Compact,
                    };
                }
            }
            CustomSurface => {
                if let Some(draft) = &mut editor.draft {
                    let surface = &mut draft.definition.display.appearance.surface;
                    *surface = match *surface {
                        WidgetSurface::Automatic => WidgetSurface::Flat,
                        WidgetSurface::Flat => WidgetSurface::Outline,
                        WidgetSurface::Outline => WidgetSurface::Automatic,
                    };
                }
            }
            CustomStatusPresentation => {
                if let Some(draft) = &mut editor.draft {
                    let status = &mut draft.definition.display.appearance.status;
                    *status = match *status {
                        WidgetStatusPresentation::LightAndText => WidgetStatusPresentation::Light,
                        WidgetStatusPresentation::Light => WidgetStatusPresentation::Text,
                        WidgetStatusPresentation::Text => WidgetStatusPresentation::LightAndText,
                    };
                }
            }
            CustomStyle => {
                if let Some(draft) = &mut editor.draft {
                    let style = &mut draft.definition.display.style;
                    *style = match *style {
                        WidgetStyle::Badge => WidgetStyle::Button,
                        WidgetStyle::Button => WidgetStyle::Led,
                        WidgetStyle::Led => WidgetStyle::Badge,
                    };
                }
            }
            CustomPrimaryAction => {
                if let Some(draft) = &mut editor.draft {
                    draft.cycle_primary_action();
                }
            }
            CustomAdvanced => {
                if editor.advanced {
                    editor.advanced = false;
                    self.focus = CustomAdvanced;
                } else if let Some(draft) = editor.draft.as_ref() {
                    let definition = draft.build_definition()?;
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
