use super::DaedalusApp;
use daedalus_proto::workbench::{WidgetActionResult, WidgetResult};
use daedalus_proto::{ClientMessage, ResponsePayload, WorkspaceId};
use daedalus_ui::bridge::WorkbenchOperation;

impl DaedalusApp {
    pub(super) fn workbench_auxiliary_response(
        &mut self,
        id: WorkspaceId,
        settings: bool,
        operation: WorkbenchOperation,
        response: ResponsePayload,
    ) {
        match response {
            ResponsePayload::WorkspaceRepoStatus(status)
                if self.state.active_workspace_id == Some(id) =>
            {
                let available = status == Some(true);
                if available && !self.state.workbench_repo_update {
                    self.show_toast("Shared workspace setup available in Settings → Custom widgets → Import / export.", false);
                }
                self.state.workbench_repo_update = available;
            }
            ResponsePayload::WorkspaceImportPreview(_)
            | ResponsePayload::WorkspaceCreationPreview(_)
            | ResponsePayload::WorkbenchCreated(_)
            | ResponsePayload::WorkbenchToml(_) => {
                self.workspace_sharing_response(settings, operation, response)
            }
            ResponsePayload::WidgetAction(action) => self.show_widget_action(id, action),
            response => self.widget_editor_response(id, settings, response),
        }
    }

    fn widget_editor_response(
        &mut self,
        id: WorkspaceId,
        settings: bool,
        response: ResponsePayload,
    ) {
        match response {
            ResponsePayload::WidgetLibrary(library) if settings => {
                self.state.settings.custom.library = library
            }
            ResponsePayload::WidgetLibraryConflict(library) if settings => {
                self.state.settings.custom.library = library;
                self.state.settings.error = Some("The template library changed. Try again.".into());
            }
            ResponsePayload::WidgetResult(result) if settings => {
                self.state.settings.custom.preview = Some(result)
            }
            ResponsePayload::WidgetResults(results)
                if self.state.active_workspace_id == Some(id) =>
            {
                if settings
                    && self.state.settings.custom.showing_details
                    && !self.state.settings.custom.action_output
                    && let Some(widget) = self.state.settings.custom.selected()
                    && let Some(snapshot) = results.iter().find(|snapshot| snapshot.id == widget.id)
                {
                    self.state.settings.custom.preview = Some(snapshot.result.clone());
                }
                self.state.widget_results = results;
            }
            ResponsePayload::WidgetTemplate(template) if settings => {
                let library = &self.state.settings.custom.library;
                if library
                    .templates
                    .iter()
                    .any(|existing| existing.id == template.id)
                {
                    self.state.settings.error =
                        Some(format!("Template '{}' already exists.", template.id));
                } else {
                    let mut templates = library.templates.clone();
                    templates.push(template);
                    self.request_workbench(
                        id,
                        ClientMessage::UpdateWidgetTemplates {
                            revision: library.revision,
                            templates,
                        },
                    );
                }
            }
            _ => {}
        }
    }

    fn workspace_sharing_response(
        &mut self,
        settings: bool,
        operation: WorkbenchOperation,
        response: ResponsePayload,
    ) {
        match response {
            ResponsePayload::WorkspaceImportPreview(preview) if settings => {
                let mut details = Vec::new();
                if preview.current.layout != preview.proposed.layout {
                    details.push((
                        "Layout".into(),
                        if preview.proposed.layout.split_view {
                            "Split view".into()
                        } else {
                            "Focused agent".into()
                        },
                    ));
                }
                details.extend(
                    preview
                        .proposed
                        .assignments
                        .iter()
                        .map(|assignment| (assignment.name.clone(), assignment.argv.join(" "))),
                );
                for widget in &preview.proposed.widgets {
                    if let Some(definition) = preview.proposed.widget_definition(widget) {
                        for source in &definition.sources {
                            if let daedalus_proto::workbench::WidgetSource::Command {
                                command,
                                refresh,
                                ..
                            } = &source.source
                            {
                                let schedule = refresh
                                    .interval_seconds
                                    .map(|seconds| format!(" · every {seconds}s"))
                                    .unwrap_or_default();
                                let completion = if refresh.on_runner_completion {
                                    " · on runner completion"
                                } else {
                                    ""
                                };
                                details.push((
                                    widget.name.clone(),
                                    format!("{}{schedule}{completion}", command.argv.join(" ")),
                                ));
                            }
                        }
                    }
                }
                self.state.settings.custom.preview = Some(WidgetResult {
                    label: format!(
                        "{} · Assignments {} · Widgets {} · Templates {}",
                        preview.proposed.name,
                        preview.proposed.assignments.len(),
                        preview.proposed.widgets.len(),
                        preview.proposed.templates.len()
                    ),
                    level: daedalus_proto::StatusLevel::Info,
                    details,
                });
                self.state.settings.custom.import = Some(preview);
            }
            ResponsePayload::WorkspaceCreationPreview(preview) if settings => {
                self.state.settings.custom.preview = Some(WidgetResult {
                    label: format!(
                        "Create {} in {}",
                        preview.incoming.name,
                        preview.root_dir.display()
                    ),
                    level: daedalus_proto::StatusLevel::Info,
                    details: preview
                        .incoming
                        .assignments
                        .iter()
                        .map(|assignment| (assignment.name.clone(), assignment.argv.join(" ")))
                        .collect(),
                });
                self.state.settings.custom.creation = Some(preview);
            }
            ResponsePayload::WorkbenchCreated(workspace) => {
                let new_id = workspace.id;
                self.project_setups.absorb(std::slice::from_ref(&workspace));
                self.state.upsert_workspace(workspace);
                self.state.select_workspace(new_id);
                self.state.settings.open = false;
                self.ensure_active_workspace_panes();
                self.schedule_resize_flush();
            }
            ResponsePayload::WorkbenchToml(text) => {
                if let WorkbenchOperation::Export(path) = operation {
                    match std::fs::write(&path, text) {
                        Ok(()) => self.show_toast(format!("Exported {}", path.display()), false),
                        Err(error) if settings => {
                            self.state.settings.error = Some(error.to_string())
                        }
                        Err(error) => self.show_toast(error.to_string(), true),
                    }
                }
            }
            _ => {}
        }
    }

    fn show_widget_action(&mut self, id: WorkspaceId, action: WidgetActionResult) {
        match action {
            WidgetActionResult::Pane(pane) => {
                if let Some(role) = self.state.panes.get(&pane).map(|state| state.summary.role) {
                    self.state.select_session(id, role, pane);
                    self.state.settings.open = false;
                }
            }
            WidgetActionResult::OpenUrl(url) => {
                if let Err(error) =
                    daedalus_ui::links::open_link(&daedalus_ui::links::DetectedLink::Url(url), None)
                {
                    self.show_toast(error.to_string(), true);
                }
            }
            WidgetActionResult::Output(result) => {
                self.state.settings.custom.action_output = true;
                self.state.settings.custom.detail_page = 0;
                self.state.settings.custom.preview = Some(result);
            }
        }
    }
}
