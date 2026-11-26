use super::DaedalusApp;
use daedalus_proto::{ClientMessage, ResponsePayload, WorkspaceId};
use daedalus_ui::bridge::{UiCommand, WorkbenchOperation};
use daedalus_ui::state::ConnectionState;
use std::time::{Duration, Instant};

mod import_review;
mod layout_saves;
pub(super) use layout_saves::LayoutSave;
mod replies;
mod snapshot;

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
            self.state.settings.custom.accept_snapshot(state.clone());
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
        self.prepare_active_workbench_layout();
        if self.watched_workbench != Some(id) {
            self.watched_workbench = Some(id);
            self.state.workbench = None;
            self.state.widget_results.clear();
            self.state.workbench_repo_update = false;
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
        if matches!(operation, WorkbenchOperation::Migration) {
            self.seeding_workbench.remove(&id);
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
                let widgets = self
                    .state
                    .workspace_widgets
                    .get(&id)
                    .cloned()
                    .unwrap_or_default();
                self.state.settings.reset_pane_widget_draft(widgets);
                self.state.settings.widgets_saved = true;
            }
            Ok(ResponsePayload::WorkbenchConflict(state)) => {
                self.load_workbench(id, state);
                self.state.settings.error = Some(
                    "Workspace settings changed during the save. Try again to check your draft against the saved values.".into(),
                );
            }
            Err(error) => self.state.settings.error = Some(error),
            Ok(_) => self.state.settings.error = Some("Unexpected widget settings reply.".into()),
        }
    }

    pub(super) fn open_widget_details(&mut self, index: Option<usize>) {
        self.open_workspace_settings();
        if !self.state.settings.open {
            return;
        }
        self.state.settings.open_custom_page();
        self.state.settings.focus = daedalus_ui::settings::Control::CustomNew;
        if let Some(index) = index
            && self.state.settings.custom.open_details(index)
        {
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
