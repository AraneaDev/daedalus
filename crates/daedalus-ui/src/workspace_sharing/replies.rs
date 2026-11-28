use super::{DaedalusApp, import_review};
use daedalus_proto::workbench::WidgetActionResult;
use daedalus_proto::{ClientMessage, ResponsePayload, WorkspaceId};
use daedalus_ui::bridge::{UiCommand, WorkbenchOperation};

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
            ResponsePayload::WidgetAction(action) => {
                self.show_widget_action(id, settings, operation, action)
            }
            response => self.widget_editor_response(id, settings, operation, response),
        }
    }

    fn widget_editor_response(
        &mut self,
        id: WorkspaceId,
        settings: bool,
        operation: WorkbenchOperation,
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
            ResponsePayload::WidgetResult(result)
                if settings && matches!(operation, WorkbenchOperation::WidgetPreview(_)) =>
            {
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
                    self.state
                        .settings
                        .update_widget_detail_result(snapshot.result.clone());
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
            ResponsePayload::WorkspaceImportPreview(preview)
                if settings && matches!(operation, WorkbenchOperation::ImportPreview(_)) =>
            {
                let result = import_review::project(
                    format!(
                        "{} · Assignments {} · Widgets {} · Templates {}",
                        preview.proposed.name,
                        preview.proposed.assignments.len(),
                        preview.proposed.widgets.len(),
                        preview.proposed.templates.len()
                    ),
                    Some(&preview.current),
                    &preview.proposed,
                );
                let editor = &mut self.state.settings.custom;
                editor.show_import_review(result);
                editor.import = Some(preview);
            }
            ResponsePayload::WorkspaceCreationPreview(preview)
                if settings && matches!(operation, WorkbenchOperation::ImportPreview(_)) =>
            {
                let result = import_review::project(
                    format!(
                        "Create {} in {}",
                        preview.incoming.name,
                        preview.root_dir.display()
                    ),
                    None,
                    &preview.incoming,
                );
                let editor = &mut self.state.settings.custom;
                editor.show_import_review(result);
                editor.creation = Some(preview);
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

    fn show_widget_action(
        &mut self,
        id: WorkspaceId,
        settings: bool,
        operation: WorkbenchOperation,
        action: WidgetActionResult,
    ) {
        if matches!(operation, WorkbenchOperation::WidgetButton(_))
            && self.state.active_workspace_id != Some(id)
        {
            return;
        }
        match action {
            WidgetActionResult::RunnerStarted(pane) => {
                let pane_id = pane.id;
                let role = pane.role;
                if !self.state.panes.contains_key(&pane_id) {
                    self.state.register_pane(pane, false);
                    self.send_command(UiCommand::RequestSnapshot { pane_id });
                }
                self.state.select_role_pane(id, role, pane_id);
                self.schedule_resize_flush();
                self.show_toast("Runner started", false);
            }
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
                if let WorkbenchOperation::WidgetButton(widget) = &operation {
                    let index = self.state.workbench.as_ref().and_then(|(_, state)| {
                        state
                            .document
                            .widgets
                            .iter()
                            .position(|item| item.id == *widget)
                    });
                    if !result.details.is_empty()
                        && !self.state.settings.open
                        && let Some(index) = index
                    {
                        self.open_widget_details(Some(index));
                        if self.state.settings.custom.showing_details {
                            let editor = &mut self.state.settings.custom;
                            editor.action_output = true;
                            editor.preview = Some(result);
                            return;
                        }
                    }
                    self.show_toast(
                        result.label,
                        result.level == daedalus_proto::StatusLevel::Error,
                    );
                    return;
                }
                if settings
                    && self.state.settings.is_custom_page()
                    && let WorkbenchOperation::WidgetAction {
                        widget,
                        detail_session,
                    } = operation
                    && self
                        .state
                        .settings
                        .custom
                        .matches_detail_session(&widget, detail_session)
                {
                    let editor = &mut self.state.settings.custom;
                    editor.action_output = true;
                    editor.detail_page = 0;
                    editor.preview = Some(result);
                }
            }
        }
    }
}
