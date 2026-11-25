use super::{Control, SettingsDialog};
use Control::*;
use daedalus_proto::workbench::{ConflictChoice, WidgetTemplate, WorkspaceImportSource};
use daedalus_proto::{ClientMessage, WorkspaceId};

impl SettingsDialog {
    pub(super) fn sharing_rows(&self, pages: Vec<Control>) -> Vec<Vec<Control>> {
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
        rows
    }

    pub(super) fn sharing_label(&self, control: Control) -> Option<String> {
        Some(match control {
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
            _ => return None,
        })
    }

    pub(super) fn sharing_command(
        &mut self,
        id: WorkspaceId,
        control: Control,
    ) -> Result<Option<ClientMessage>, String> {
        let editor = &mut self.custom;
        match control {
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
                file_path(&editor.path)?;
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
                file_path(&editor.path)?;
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

    pub(in crate::settings) fn next_import_conflict(&self) -> Option<&str> {
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
