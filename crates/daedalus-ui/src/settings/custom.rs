use super::{Control, SettingsAction, SettingsDialog};
use Control::*;
use daedalus_proto::ClientMessage;
use daedalus_proto::workbench::{
    CommandOutput, ConflictChoice, WidgetTemplate, WorkspaceImportSource,
};

impl SettingsDialog {
    pub(super) fn custom_rows(&self, pages: Vec<Control>) -> Vec<Vec<Control>> {
        if self.custom.showing_details {
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
            return rows;
        }
        if self.custom.sharing {
            let mut rows = vec![
                pages,
                vec![CustomPath],
                vec![CustomRoot],
                vec![CustomImport, CustomExport],
                vec![CustomRepo],
                vec![CustomImportTemplate, CustomExportTemplate],
            ];
            if self.custom.import.is_some() || self.custom.creation.is_some() {
                if let Some(detail) = &self.custom.preview {
                    rows.extend(
                        (0..detail.details.len().min(6)).map(|index| vec![CustomDetail(index)]),
                    );
                }
                if self.next_import_conflict().is_some() {
                    rows.push(vec![CustomResolve, CustomResolveIncoming]);
                }
                rows.push(vec![CustomApplyImport]);
            }
            rows.push(vec![CustomBack, Cancel]);
            return rows;
        }
        if self.custom.draft.is_some() {
            if self.custom.advanced {
                return vec![
                    pages,
                    vec![CustomDefinition],
                    vec![CustomApplyDefinition],
                    vec![CustomAdvanced, Cancel],
                ];
            }
            return vec![
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
            ];
        }
        let mut rows = vec![pages, vec![CustomNew, CustomSharing]];
        if self.custom.selected().is_some() {
            rows.extend([
                vec![CustomPrev, CustomEdit, CustomNext],
                vec![CustomDuplicate, CustomMove, CustomPin, CustomRemove],
            ]);
        }
        rows.push(vec![Cancel]);
        rows
    }

    pub(super) fn custom_label(&self, control: Control) -> Option<String> {
        let draft = self.custom.draft.as_ref();
        Some(match control {
            CustomWidgetsPage => "Custom widgets".into(),
            CustomNew => "+ New widget".into(),
            CustomEdit => self.custom.selected().map_or_else(
                || "Edit widget".into(),
                |widget| format!("Edit {}", widget.name),
            ),
            CustomPrev => "‹ Previous".into(),
            CustomNext => "Next ›".into(),
            CustomDuplicate => "Duplicate".into(),
            CustomRemove => "Remove widget".into(),
            CustomMove => "Move earlier".into(),
            CustomPin => {
                if draft
                    .map(|draft| draft.widget.pinned)
                    .or_else(|| self.custom.selected().map(|widget| widget.pinned))
                    .unwrap_or(false)
                {
                    "Unpin".into()
                } else {
                    "Pin to bar".into()
                }
            }
            CustomSharing => "Import / export".into(),
            CustomBack => "Back".into(),
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
            CustomSave => {
                if draft.is_some_and(|draft| draft.existing) {
                    "Save widget".into()
                } else {
                    "Add widget".into()
                }
            }
            CustomSaveTemplate => "Save as template".into(),
            CustomImport => "Preview workspace import".into(),
            CustomExport => "Export workspace".into(),
            CustomImportTemplate => "Import widget template".into(),
            CustomExportTemplate => "Export selected template".into(),
            CustomRepo => "Preview shared repo file".into(),
            CustomApplyImport => "Apply workspace".into(),
            CustomResolve => format!(
                "Keep local · {}",
                self.import_conflict()
                    .and_then(|conflict| conflict.local.as_deref())
                    .unwrap_or("Remove")
            ),
            CustomResolveIncoming => format!(
                "Use repo · {}",
                self.import_conflict()
                    .and_then(|conflict| conflict.incoming.as_deref())
                    .unwrap_or("Remove")
            ),
            _ if matches!(
                control,
                CustomName
                    | CustomExecutable
                    | CustomArguments
                    | CustomInterval
                    | CustomPublish
                    | CustomSummary
                    | CustomDetails
                    | CustomUrl
                    | CustomPath
                    | CustomRoot
                    | CustomDefinition
            ) =>
            {
                format!(
                    "{}   {}",
                    control.field_label()?,
                    self.custom_value(control).unwrap_or_default()
                )
            }
            _ => return None,
        })
    }

    pub(super) fn custom_enabled(&self, control: Control) -> bool {
        match control {
            CustomNew => self.custom.state.is_some(),
            CustomTemplate => !self.custom.library.templates.is_empty(),
            CustomPrev | CustomNext => self
                .custom
                .state
                .as_ref()
                .is_some_and(|state| state.document.widgets.len() > 1),
            CustomMove => self.custom.selected > 0,
            CustomApplyImport => self.next_import_conflict().is_none(),
            CustomExportTemplate => self.custom.selected().is_some(),
            CustomDetail(_) => false,
            CustomDetailPrev => self.custom.detail_page > 0,
            CustomDetailNext => (self.custom.detail_page + 1) * 8 < self.detail_line_count(),
            _ => true,
        }
    }

    pub(super) fn custom_value(&self, control: Control) -> Option<&str> {
        if control == CustomPath {
            return Some(&self.custom.path);
        }
        if control == CustomRoot {
            return Some(&self.custom.new_root);
        }
        if control == CustomDefinition {
            return Some(&self.custom.definition_json);
        }
        let draft = self.custom.draft.as_ref()?;
        Some(match control {
            CustomName => &draft.widget.name,
            CustomExecutable => &draft.executable,
            CustomArguments => &draft.arguments,
            CustomInterval => &draft.interval,
            CustomPublish => &draft.publish_key,
            CustomSummary => &draft.summary,
            CustomDetails => &draft.details,
            CustomUrl => &draft.url,
            _ => return None,
        })
    }

    pub(super) fn custom_field(&mut self) -> Option<&mut String> {
        if self.focus == CustomPath {
            return Some(&mut self.custom.path);
        }
        if self.focus == CustomRoot {
            return Some(&mut self.custom.new_root);
        }
        if self.focus == CustomDefinition {
            return Some(&mut self.custom.definition_json);
        }
        let draft = self.custom.draft.as_mut()?;
        if matches!(
            self.focus,
            CustomExecutable | CustomArguments | CustomInterval | CustomPublish
        ) {
            draft.change_source = true;
        }
        Some(match self.focus {
            CustomName => &mut draft.widget.name,
            CustomExecutable => &mut draft.executable,
            CustomArguments => &mut draft.arguments,
            CustomInterval => &mut draft.interval,
            CustomPublish => &mut draft.publish_key,
            CustomSummary => &mut draft.summary,
            CustomDetails => &mut draft.details,
            CustomUrl => &mut draft.url,
            _ => return None,
        })
    }

    pub(super) fn apply_custom_control(&mut self, control: Control) -> SettingsAction {
        match self.custom_command(control) {
            Ok(Some(request)) => SettingsAction::Workbench(Box::new(request)),
            Ok(None) => SettingsAction::None,
            Err(message) => {
                self.error = Some(message);
                SettingsAction::None
            }
        }
    }

    fn custom_command(&mut self, control: Control) -> Result<Option<ClientMessage>, String> {
        let id = self.widget_workspace.ok_or("No workspace is open")?;
        let editor = &mut self.custom;
        let selected = editor.selected;
        match control {
            CustomNew => {
                editor.create();
                self.focus = CustomName;
            }
            CustomEdit => {
                editor.showing_details = false;
                editor.edit();
                self.focus = CustomName;
                self.select_all = true;
            }
            CustomPrev => editor.move_selection(-1),
            CustomNext => editor.move_selection(1),
            CustomSharing => {
                editor.sharing = true;
                editor.preview = None;
            }
            CustomBack => {
                editor.sharing = false;
                editor.showing_details = false;
                editor.advanced = false;
                editor.draft = None;
                editor.preview = None;
                editor.import = None;
                editor.creation = None;
                self.focus = CustomNew;
            }
            CustomTemplate => editor.choose_template(),
            CustomName | CustomExecutable | CustomArguments | CustomInterval | CustomPublish
            | CustomSummary | CustomDetails | CustomUrl | CustomPath | CustomRoot
            | CustomDefinition => self.select_all = true,
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
            CustomPin => {
                if let Some(draft) = &mut editor.draft {
                    draft.widget.pinned = !draft.widget.pinned;
                } else {
                    return Ok(editor.update(id, |doc| {
                        if let Some(widget) = doc.widgets.get_mut(selected) {
                            widget.pinned = !widget.pinned;
                        }
                    }));
                }
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
            CustomDuplicate => {
                return Ok(editor.update(id, |doc| {
                    if let Some(widget) = doc.widgets.get(selected) {
                        let mut copy = widget.clone();
                        copy.id = uuid::Uuid::new_v4().to_string();
                        copy.name.push_str(" copy");
                        doc.widgets.insert(selected + 1, copy);
                    }
                }));
            }
            CustomRemove => {
                return Ok(editor.update(id, |doc| {
                    if selected < doc.widgets.len() {
                        doc.widgets.remove(selected);
                    }
                }));
            }
            CustomMove => {
                return Ok(editor.update(id, |doc| {
                    if selected > 0 && selected < doc.widgets.len() {
                        doc.widgets.swap(selected, selected - 1);
                    }
                }));
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
            CustomDetailPrev => editor.detail_page = editor.detail_page.saturating_sub(1),
            CustomDetailNext => editor.detail_page += 1,
            CustomSave => return editor.save(id).map(Some),
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
            CustomImport | CustomRepo => {
                let source = if control == CustomRepo {
                    WorkspaceImportSource::Repo
                } else {
                    WorkspaceImportSource::Text(read_file(&editor.path)?)
                };
                editor.import = None;
                editor.creation = None;
                editor.resolutions.clear();
                if !editor.new_root.trim().is_empty() {
                    return Ok(Some(ClientMessage::PreviewWorkspaceCreation {
                        root_dir: editor.new_root.trim().into(),
                        source,
                        linked: control == CustomRepo,
                    }));
                }
                return Ok(Some(ClientMessage::PreviewWorkspaceImport {
                    workspace_id: id,
                    source,
                    linked: control == CustomRepo,
                }));
            }
            CustomExport => {
                editor.export_path = Some(file_path(&editor.path)?.into());
                return Ok(Some(ClientMessage::ExportWorkspace { workspace_id: id }));
            }
            CustomImportTemplate => {
                return Ok(Some(ClientMessage::DecodeWidgetTemplate {
                    text: read_file(&editor.path)?,
                }));
            }
            CustomExportTemplate => {
                let widget = editor
                    .selected()
                    .cloned()
                    .ok_or("Select a widget to export")?;
                let definition = editor
                    .state
                    .as_ref()
                    .and_then(|state| state.document.widget_definition(&widget))
                    .cloned()
                    .ok_or("No widget definition")?;
                editor.export_path = Some(file_path(&editor.path)?.into());
                return Ok(Some(ClientMessage::EncodeWidgetTemplate {
                    template: WidgetTemplate {
                        id: widget.id,
                        name: widget.name,
                        definition,
                    },
                }));
            }
            CustomResolve | CustomResolveIncoming => {
                if let Some(path) = editor
                    .import
                    .as_ref()
                    .and_then(|preview| {
                        preview
                            .conflicts
                            .iter()
                            .find(|conflict| !editor.resolutions.contains_key(&conflict.path))
                    })
                    .map(|conflict| conflict.path.clone())
                {
                    editor.resolutions.insert(
                        path,
                        if control == CustomResolve {
                            ConflictChoice::Local
                        } else {
                            ConflictChoice::Incoming
                        },
                    );
                }
            }
            CustomApplyImport => {
                if let Some(preview) = &editor.creation {
                    return Ok(Some(ClientMessage::ApplyWorkspaceCreation {
                        preview: preview.clone(),
                    }));
                }
                let preview = editor.import.as_ref().ok_or("Preview the import first")?;
                return Ok(Some(ClientMessage::ApplyWorkspaceImport {
                    workspace_id: id,
                    revision: preview.revision,
                    incoming: preview.incoming.clone(),
                    linked: preview.linked,
                    resolutions: editor.resolutions.clone(),
                }));
            }
            _ => {}
        }
        Ok(None)
    }

    pub(super) fn next_import_conflict(&self) -> Option<&str> {
        self.import_conflict()
            .map(|conflict| conflict.path.as_str())
    }

    fn import_conflict(&self) -> Option<&daedalus_proto::workbench::MergeConflict> {
        self.custom
            .import
            .as_ref()?
            .conflicts
            .iter()
            .find(|conflict| !self.custom.resolutions.contains_key(&conflict.path))
    }

    fn selected_widget_definition(&self) -> Option<&daedalus_proto::workbench::WidgetDefinition> {
        self.custom
            .state
            .as_ref()?
            .document
            .widget_definition(self.custom.selected()?)
    }

    fn detail_line_count(&self) -> usize {
        self.custom.preview.as_ref().map_or(0, |result| {
            result
                .details
                .iter()
                .map(|(_, value)| value.lines().count())
                .sum()
        })
    }
}

fn file_path(path: &str) -> Result<&str, String> {
    let path = path.trim();
    if path.is_empty() {
        Err("Enter a TOML file path on this computer.".into())
    } else {
        Ok(path)
    }
}

fn read_file(path: &str) -> Result<String, String> {
    std::fs::read_to_string(file_path(path)?).map_err(|error| error.to_string())
}
