use super::DaedalusApp;
use daedalus_proto::workbench::WorkbenchState;
use daedalus_proto::{ClientMessage, ResponsePayload, WorkspaceId};
use daedalus_ui::bridge::{UiCommand, WorkbenchOperation};
use daedalus_ui::state::ConnectionState;
use daedalus_ui::workspace_widgets::{self, WorkspaceWidgets};
use std::time::{Duration, Instant};

mod import_review;
mod layout_saves;
pub(super) use layout_saves::LayoutSave;
mod replies;

impl DaedalusApp {
    pub(super) fn request_workbench(&mut self, id: WorkspaceId, request: ClientMessage) {
        self.request_workbench_as(id, WorkbenchOperation::Read, request);
    }

    pub(super) fn request_workbench_as(
        &mut self,
        id: WorkspaceId,
        operation: WorkbenchOperation,
        request: ClientMessage,
    ) -> bool {
        let widget_test = match operation {
            WorkbenchOperation::WidgetPreview(token) => Some(token),
            _ => None,
        };
        let queued = self.send_command(UiCommand::Workbench {
            workspace_id: id,
            operation,
            request: Box::new(request),
        });
        if !queued && self.state.settings.widget_workspace == Some(id) {
            if let Some(token) = widget_test {
                self.state.settings.custom.finish_widget_test(token);
            }
            self.state.settings.error =
                Some("Reconnect to the daemon to configure widgets.".into());
        }
        queued
    }

    pub(super) fn prepare_workbench_settings(&mut self, id: WorkspaceId) {
        self.state.settings.custom_supported = self.workbench_capable;
        if !self.workbench_capable {
            return;
        }
        if let Some((_, state)) = self
            .state
            .workbench
            .as_ref()
            .filter(|(workspace, _)| *workspace == id)
        {
            self.state.settings.custom.state = Some(state.clone());
        }
        self.request_workbench(id, ClientMessage::GetWorkbench { workspace_id: id });
        self.request_workbench(id, ClientMessage::ListWidgetTemplates);
    }

    pub(super) fn poll_workbench(&mut self) {
        if !self.workbench_capable || self.state.connection != ConnectionState::Connected {
            self.widget_refresh_at = None;
            return;
        }
        let Some(id) = self.state.active_workspace_id else {
            self.widget_refresh_at = None;
            return;
        };
        let now = Instant::now();
        if self.watched_workbench != Some(id) {
            self.watched_workbench = Some(id);
            self.state.workbench = None;
            self.state.widget_results.clear();
            self.state.workbench_repo_update = false;
            if !self.pointer.dragging_divider()
                && let Some(layout) = self.layout_saves.get(&id).map(|save| save.desired.clone())
            {
                self.show_workbench_layout(&layout);
                self.schedule_resize_flush();
            }
            self.request_workbench(id, ClientMessage::WatchWorkbench { workspace_id: id });
        }
        if self.widget_refresh_at.is_none_or(|at| now >= at) {
            self.widget_refresh_at = Some(now + Duration::from_secs(3));
            self.request_workbench(id, ClientMessage::CheckWorkspaceRepo { workspace_id: id });
            if self
                .state
                .workbench
                .as_ref()
                .is_some_and(|(workspace, state)| {
                    *workspace == id && state.revision > 0 && !state.document.widgets.is_empty()
                })
            {
                self.request_workbench(
                    id,
                    ClientMessage::GetWidgetResults {
                        workspace_id: id,
                        refresh: None,
                    },
                );
            }
        }
    }

    pub(super) fn workbench_response(
        &mut self,
        id: WorkspaceId,
        operation: WorkbenchOperation,
        response: Result<ResponsePayload, String>,
    ) {
        let settings = self.state.settings.open && self.state.settings.widget_workspace == Some(id);
        let applies = match operation {
            WorkbenchOperation::WidgetPreview(token) => {
                settings && self.state.settings.custom.finish_widget_test(token)
            }
            WorkbenchOperation::ImportPreview(token) => {
                settings && self.state.settings.custom.finish_import_review(token)
            }
            _ => true,
        };
        if !applies {
            return;
        }
        let previous: Vec<_> = if settings {
            self.state.settings.control_order().collect()
        } else {
            Vec::new()
        };
        if let WorkbenchOperation::Layout(token) = operation {
            self.layout_save_response(id, token, response);
        } else if let WorkbenchOperation::PaneWidgets(token) = operation {
            self.pane_widgets_response(id, token, response);
        } else {
            match response {
                Ok(ResponsePayload::Workbench(state)) => {
                    self.load_workbench(id, state);
                    if settings {
                        self.finish_workbench_operation(operation);
                    }
                }
                Ok(ResponsePayload::WorkbenchConflict(state)) => {
                    self.load_workbench(id, state);
                    let message =
                        "The workspace changed. Review the latest settings and try again.";
                    if settings {
                        self.state.settings.error = Some(message.into());
                    } else if !matches!(operation, WorkbenchOperation::Migration) {
                        self.show_toast(message, true);
                    }
                }
                Ok(response) => {
                    self.workbench_auxiliary_response(id, settings, operation, response)
                }
                Err(error) if settings => self.state.settings.error = Some(error),
                Err(error) => self.show_toast(error, true),
            }
        }
        if settings && self.state.settings.open {
            self.state.settings.reconcile_controls(&previous);
        }
        self.request_redraw();
    }

    fn finish_workbench_operation(&mut self, operation: WorkbenchOperation) {
        let editor = &mut self.state.settings.custom;
        match operation {
            WorkbenchOperation::WidgetEdit if editor.draft_matches_saved_widget() => {
                editor.draft = None;
                editor.clear_preview();
            }
            WorkbenchOperation::Import if editor.sharing => {
                editor.clear_import_review();
            }
            _ => {}
        }
    }

    fn pane_widgets_response(
        &mut self,
        id: WorkspaceId,
        token: u64,
        response: Result<ResponsePayload, String>,
    ) {
        if !self.state.settings.finish_pane_widget_save(id, token) {
            return;
        }
        match response {
            Ok(ResponsePayload::Workbench(state)) => {
                self.load_workbench(id, state);
                self.state.settings.widgets = self
                    .state
                    .workspace_widgets
                    .get(&id)
                    .cloned()
                    .unwrap_or_default();
                self.state.settings.widgets_saved = true;
            }
            Ok(ResponsePayload::WorkbenchConflict(state)) => {
                self.load_workbench(id, state);
                self.state.settings.error =
                    Some("The widget settings changed. Review the latest values and retry.".into());
            }
            Err(error) => self.state.settings.error = Some(error),
            Ok(_) => self.state.settings.error = Some("Unexpected widget settings reply.".into()),
        }
    }

    fn load_workbench(&mut self, id: WorkspaceId, state: WorkbenchState) {
        if !self
            .state
            .workspaces
            .iter()
            .any(|workspace| workspace.id == id)
        {
            return;
        }
        let settings = self.state.settings.open && self.state.settings.widget_workspace == Some(id);
        let current = if settings {
            self.state.settings.custom.state.as_ref()
        } else {
            self.state
                .workbench
                .as_ref()
                .filter(|(workspace, _)| *workspace == id)
                .map(|(_, state)| state)
        };
        let pending_base = self
            .layout_saves
            .get(&id)
            .and_then(|save| save.base.as_ref());
        if current
            .into_iter()
            .chain(pending_base)
            .any(|current| current.revision > state.revision)
        {
            self.flush_workbench_layout(id);
            return;
        }
        if state.revision == 0 {
            self.layout_saves.remove(&id);
        } else if let Some(save) = self.layout_saves.get_mut(&id) {
            save.base = Some(state.clone());
        }

        if state.revision == 0 && self.seeded_workbench.insert(id) {
            match workspace_widgets::load(&self.widget_directory, id) {
                Ok(Some(legacy)) => {
                    let mut document = state.document.clone();
                    document.pane_widgets = Some(legacy.into());
                    if self.state.active_workspace_id == Some(id) {
                        document.layout = self.pointer.splits.portable(!self.state.zoomed);
                    }
                    self.request_workbench_as(
                        id,
                        WorkbenchOperation::Migration,
                        ClientMessage::SeedLegacyWorkbench {
                            workspace_id: id,
                            document,
                        },
                    );
                }
                Ok(None) => {}
                Err(error) => self.show_toast(
                    format!("Could not read legacy widget settings: {error}"),
                    true,
                ),
            }
        }

        let previous_widgets = self.state.workspace_widgets.remove(&id).unwrap_or_default();
        let effective = state
            .document
            .pane_widgets
            .clone()
            .map(WorkspaceWidgets::from);
        if let Some(widgets) = &effective {
            self.state.workspace_widgets.insert(id, widgets.clone());
        }
        if settings {
            let dialog = &mut self.state.settings;
            if dialog.widgets == previous_widgets {
                dialog.widgets = effective.unwrap_or_default();
            }
            dialog.custom.accept_snapshot(state.clone());
        }
        if state.revision > 0 {
            self.project_setups
                .workspace_changed(id, state.document.assignments.clone());
            self.refresh_workspace_setup(id);
        }
        if self.state.active_workspace_id == Some(id) {
            if state.revision > 0
                && let Some(workspace) = self.state.active_workspace_mut()
            {
                workspace.name.clone_from(&state.document.name);
            }
            let previous = self
                .state
                .workbench
                .as_ref()
                .filter(|(workspace, _)| *workspace == id)
                .map(|(_, state)| state);
            if !self.pointer.dragging_divider() && state.revision > 0 {
                if let Some(layout) = self.layout_saves.get(&id).map(|save| save.desired.clone()) {
                    self.show_workbench_layout(&layout);
                } else if previous
                    .is_none_or(|previous| previous.document.layout != state.document.layout)
                {
                    self.show_workbench_layout(&state.document.layout);
                }
            }
            if state.revision > 0 {
                self.request_workbench(
                    id,
                    ClientMessage::GetWidgetResults {
                        workspace_id: id,
                        refresh: None,
                    },
                );
            }
            self.state.workbench = Some((id, state));
            self.schedule_resize_flush();
        }
        self.flush_workbench_layout(id);
    }

    pub(super) fn open_widget_details(&mut self, index: Option<usize>) {
        self.open_workspace_settings();
        if !self.state.settings.open {
            return;
        }
        self.state.settings.open_custom_page();
        self.state.settings.focus = daedalus_ui::settings::Control::CustomNew;
        if let Some(index) = index {
            self.state.settings.custom.selected = index;
            self.state.settings.custom.showing_details = true;
            self.state.settings.focus = daedalus_ui::settings::Control::CustomRefresh;
            self.state.settings.custom.preview =
                self.state.settings.custom.selected().and_then(|widget| {
                    self.state
                        .widget_results
                        .iter()
                        .find(|snapshot| snapshot.id == widget.id)
                        .map(|snapshot| snapshot.result.clone())
                });
        }
        self.request_redraw();
    }
}
