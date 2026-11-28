use super::{Control, SettingsAction, SettingsDialog};
use crate::bridge::WorkbenchOperation;
use Control::*;
use daedalus_proto::ClientMessage;

mod definition;
mod details;
mod elements;
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
                vec![CustomInspect],
            ]);
        } else if self.custom_enabled(CustomNext) {
            rows.push(vec![CustomPrev, CustomNext]);
        }
        rows.push(vec![Cancel]);
        rows
    }

    pub(super) fn custom_label(&self, control: Control) -> Option<String> {
        if let Some(label) = self
            .definition_label(control)
            .or_else(|| self.element_label(control))
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
            CustomInspect => "View widget details".into(),
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
            _ => format!(
                "{}   {}",
                control.field_label()?,
                self.custom_value(control)?
            ),
        })
    }

    pub(super) fn custom_enabled(&self, control: Control) -> bool {
        if let Some(enabled) = self.element_enabled(control) {
            return enabled;
        }
        match control {
            CustomNew => self.custom.state.is_some(),
            CustomTemplate => !self.custom.library.templates.is_empty(),
            CustomPrev | CustomNext => self.custom.state.as_ref().is_some_and(|state| {
                let count = state.document.widgets.len();
                count > 1 || (count == 1 && self.custom.selected().is_none())
            }),
            CustomEdit | CustomInspect | CustomDuplicate | CustomRemove | CustomExportTemplate => {
                self.custom.selected().is_some()
            }
            CustomPrimaryAction => self.custom.draft.as_ref().is_some_and(|draft| {
                draft.definition.display.primary_action.is_some()
                    || !draft.available_actions().is_empty()
            }),
            CustomPin => self.custom.draft.is_some() || self.custom.selected().is_some(),
            CustomMove => self.custom.selected_index().is_some_and(|index| index > 0),
            CustomApplyImport => self.can_apply_import(),
            CustomResolve | CustomResolveIncoming => {
                !self.custom.is_reviewing_import() && self.next_import_conflict().is_some()
            }
            CustomDetail(_) => false,
            CustomDetailPrev => self.custom.detail_page > 0,
            CustomDetailNext => {
                (self.custom.detail_page + 1) * self.detail_page_size() < self.detail_line_count()
            }
            CustomActionPrev => self.custom.detail_action_page > 0,
            CustomActionNext => self.has_next_action_page(),
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
            CustomAccent => &draft.accent,
            CustomWidth => &draft.width,
            CustomIcon => &draft.icon,
            _ => return self.element_value(control),
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
        if elements::is_text_field(self.focus) {
            return self.element_field();
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
            CustomAccent => &mut draft.accent,
            CustomWidth => &mut draft.width,
            CustomIcon => &mut draft.icon,
            _ => return None,
        })
    }

    pub(super) fn apply_custom_control(&mut self, control: Control) -> SettingsAction {
        if control == CustomInspect {
            return self
                .custom
                .selected_index()
                .map_or(SettingsAction::None, SettingsAction::InspectWidget);
        }
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
                    ClientMessage::GetWidgetResults {
                        refresh: Some(_), ..
                    } => {
                        let Some(state) = self.custom.state.as_ref() else {
                            return SettingsAction::None;
                        };
                        WorkbenchOperation::WidgetRefresh(state.revision)
                    }
                    ClientMessage::InvokeWidgetAction { widget, .. } => {
                        let Some(detail_session) = self.custom.detail_session() else {
                            return SettingsAction::None;
                        };
                        WorkbenchOperation::WidgetAction {
                            widget: widget.clone(),
                            detail_session,
                        }
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
        if self.apply_element_control(control) {
            return Ok(None);
        }
        let editor = &mut self.custom;
        match control {
            CustomNew => {
                editor.create();
                self.focus = CustomName;
            }
            CustomEdit => {
                editor.close_details();
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
                editor.close_details();
                editor.advanced = false;
                editor.draft = None;
                editor.clear_import_review();
                self.focus = CustomNew;
            }
            CustomName | CustomExecutable | CustomArguments | CustomInterval | CustomPublish
            | CustomSummary | CustomDetails | CustomUrl | CustomAccent | CustomWidth
            | CustomIcon | CustomPath | CustomRoot | CustomDefinition => self.select_all = true,
            CustomPin => {
                if let Some(draft) = &mut editor.draft {
                    draft.widget.pinned = !draft.widget.pinned;
                } else {
                    return Ok(editor.update_selected(id, |widgets, index| {
                        widgets[index].pinned = !widgets[index].pinned;
                    }));
                }
            }
            CustomDuplicate => {
                return Ok(editor.update_selected(id, |widgets, index| {
                    let mut copy = widgets[index].clone();
                    copy.id = uuid::Uuid::new_v4().to_string();
                    copy.name.push_str(" copy");
                    widgets.insert(index + 1, copy);
                }));
            }
            CustomRemove => {
                return Ok(editor.update_selected(id, |widgets, index| {
                    widgets.remove(index);
                }));
            }
            CustomMove => {
                return Ok(editor.update_selected(id, |widgets, index| {
                    widgets.swap(index, index - 1);
                }));
            }
            CustomTemplate
            | CustomAdvanced
            | CustomApplyDefinition
            | CustomOutput
            | CustomPrimaryAction
            | CustomStyle
            | CustomAppearance
            | CustomAppearanceBack
            | CustomDensity
            | CustomSurface
            | CustomStatusPresentation
            | CustomPreview
            | CustomSave
            | CustomSaveTemplate => {
                return self.definition_command(id, control);
            }
            CustomDetailPrev | CustomDetailNext | CustomActionPrev | CustomActionNext
            | CustomRefresh | CustomViewAction(_) => {
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
