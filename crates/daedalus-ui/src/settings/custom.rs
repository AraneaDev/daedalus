use super::{Control, SettingsAction, SettingsDialog};
use crate::bridge::WorkbenchOperation;
use Control::*;
use daedalus_proto::ClientMessage;

mod definition;
mod details;
mod sharing;

impl SettingsDialog {
    pub(super) fn custom_rows(&self, pages: Vec<Control>) -> Vec<Vec<Control>> {
        if self.custom.showing_details {
            return self.detail_rows(pages);
        }
        if self.custom.sharing {
            return self.sharing_rows(pages);
        }
        if self.custom.draft.is_some() {
            return self.definition_rows(pages);
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
        if let Some(label) = self
            .definition_label(control)
            .or_else(|| self.detail_label(control))
            .or_else(|| self.sharing_label(control))
        {
            return Some(label);
        }
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
            CustomApplyImport => self.can_apply_import(),
            CustomResolve | CustomResolveIncoming => {
                !self.custom.is_reviewing_import() && self.next_import_conflict().is_some()
            }
            CustomExportTemplate => self.custom.selected().is_some(),
            CustomDetail(_) => false,
            CustomDetailPrev => self.custom.detail_page > 0,
            CustomDetailNext => {
                (self.custom.detail_page + 1) * self.detail_page_size() < self.detail_line_count()
            }
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
            self.custom.clear_import_review();
            return Some(&mut self.custom.path);
        }
        if self.focus == CustomRoot {
            self.custom.clear_import_review();
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
            Ok(Some(request)) => {
                let operation = match &request {
                    ClientMessage::PreviewWidget {
                        name, definition, ..
                    } => {
                        let Some(token) = self.custom.start_widget_test(name, definition) else {
                            return SettingsAction::None;
                        };
                        WorkbenchOperation::WidgetPreview(token)
                    }
                    ClientMessage::PreviewWorkspaceImport { .. }
                    | ClientMessage::PreviewWorkspaceCreation { .. }
                    | ClientMessage::ResolveWorkspaceImport { .. } => {
                        WorkbenchOperation::ImportPreview(self.custom.start_import_review())
                    }
                    _ if control == CustomSave => WorkbenchOperation::WidgetEdit,
                    _ if control == CustomApplyImport => WorkbenchOperation::Import,
                    _ if matches!(control, CustomExport | CustomExportTemplate) => {
                        WorkbenchOperation::Export(std::path::PathBuf::from(
                            self.custom.path.trim(),
                        ))
                    }
                    _ => WorkbenchOperation::Read,
                };
                SettingsAction::Workbench {
                    operation,
                    request: Box::new(request),
                }
            }
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
                editor.clear_import_review();
            }
            CustomBack => {
                editor.sharing = false;
                editor.showing_details = false;
                editor.advanced = false;
                editor.draft = None;
                editor.clear_import_review();
                self.focus = CustomNew;
            }
            CustomName | CustomExecutable | CustomArguments | CustomInterval | CustomPublish
            | CustomSummary | CustomDetails | CustomUrl | CustomPath | CustomRoot
            | CustomDefinition => self.select_all = true,
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
            CustomTemplate
            | CustomAdvanced
            | CustomApplyDefinition
            | CustomOutput
            | CustomPreview
            | CustomSave
            | CustomSaveTemplate => {
                return self.definition_command(id, control);
            }
            CustomDetailPrev | CustomDetailNext | CustomRefresh | CustomViewAction(_) => {
                return self.detail_command(id, control);
            }
            CustomImport
            | CustomRepo
            | CustomExport
            | CustomImportTemplate
            | CustomExportTemplate
            | CustomResolve
            | CustomResolveIncoming
            | CustomApplyImport => {
                return self.sharing_command(id, control);
            }
            _ => {}
        }
        Ok(None)
    }
}
