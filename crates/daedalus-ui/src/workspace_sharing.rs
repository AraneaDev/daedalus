use super::DaedalusApp;
use daedalus_proto::workbench::{WidgetActionResult, WidgetResult, WorkbenchState};
use daedalus_proto::{ClientMessage, ResponsePayload, WorkspaceId};
use daedalus_ui::bridge::UiCommand;
use daedalus_ui::state::ConnectionState;
use std::time::{Duration, Instant};

impl DaedalusApp {
    pub(super) fn request_workbench(&mut self, id: WorkspaceId, request: ClientMessage) {
        if !self.send_command(UiCommand::Workbench {
            workspace_id: id,
            request: Box::new(request),
        }) && self.state.settings.widget_workspace == Some(id)
        {
            self.state.settings.error =
                Some("Reconnect to the daemon to configure widgets.".into());
        }
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
        saved: bool,
        response: Result<ResponsePayload, String>,
    ) {
        let settings = self.state.settings.open && self.state.settings.widget_workspace == Some(id);
        let previous: Vec<_> = if settings {
            self.state.settings.control_order().collect()
        } else {
            Vec::new()
        };
        match response {
            Ok(ResponsePayload::Workbench(state)) => self.load_workbench(id, state, saved),
            Ok(ResponsePayload::WorkspaceRepoStatus(status))
                if self.state.active_workspace_id == Some(id) =>
            {
                let available = status == Some(true);
                if available && !self.state.workbench_repo_update {
                    self.show_toast("Shared workspace setup available in Settings → Custom widgets → Import / export.", false);
                }
                self.state.workbench_repo_update = available;
            }
            Ok(ResponsePayload::WorkbenchConflict(state)) => {
                self.load_workbench(id, state, false);
                if settings {
                    self.state.settings.error = Some(
                        "The workspace changed. Review the latest settings and try again.".into(),
                    );
                }
            }
            Ok(ResponsePayload::WidgetLibrary(library)) if settings => {
                self.state.settings.custom.library = library
            }
            Ok(ResponsePayload::WidgetLibraryConflict(library)) if settings => {
                self.state.settings.custom.library = library;
                self.state.settings.error = Some("The template library changed. Try again.".into());
            }
            Ok(ResponsePayload::WidgetResult(result)) if settings => {
                self.state.settings.custom.preview = Some(result)
            }
            Ok(ResponsePayload::WidgetResults(results))
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
            Ok(ResponsePayload::WorkspaceImportPreview(preview)) if settings => {
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
            Ok(ResponsePayload::WorkspaceCreationPreview(preview)) if settings => {
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
            Ok(ResponsePayload::WorkbenchCreated(workspace)) => {
                let new_id = workspace.id;
                self.project_setups.absorb(std::slice::from_ref(&workspace));
                self.state.upsert_workspace(workspace);
                self.state.select_workspace(new_id);
                self.state.settings.open = false;
                self.ensure_active_workspace_panes();
                self.schedule_resize_flush();
            }
            Ok(ResponsePayload::WidgetTemplate(template)) if settings => {
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
            Ok(ResponsePayload::WorkbenchToml(text)) if settings => {
                if let Some(path) = self.state.settings.custom.export_path.take() {
                    match std::fs::write(&path, text) {
                        Ok(()) => self.show_toast(format!("Exported {path}"), false),
                        Err(error) => self.state.settings.error = Some(error.to_string()),
                    }
                }
            }
            Ok(ResponsePayload::WidgetAction(action)) => self.show_widget_action(id, action),
            Err(error) if settings => self.state.settings.error = Some(error),
            Err(error) => self.show_toast(error, true),
            _ => {}
        }
        if settings && self.state.settings.open {
            self.state.settings.reconcile_controls(&previous);
        }
        self.request_redraw();
    }

    fn load_workbench(&mut self, id: WorkspaceId, state: WorkbenchState, saved: bool) {
        if state.revision == 0
            && let Some(legacy) = self.state.workspace_widgets.get(&id).cloned()
            && self.seeded_workbench.insert(id)
        {
            let mut document = state.document.clone();
            document.pane_widgets = Some(legacy.into());
            document.layout = self.pointer.splits.portable(!self.state.zoomed);
            self.request_workbench(
                id,
                ClientMessage::SeedLegacyWorkbench {
                    workspace_id: id,
                    document,
                },
            );
        } else if state.revision > 0
            && let Some(widgets) = state.document.pane_widgets.clone()
        {
            self.state.workspace_widgets.insert(id, widgets.into());
        }
        if self.state.settings.open && self.state.settings.widget_workspace == Some(id) {
            let editor = &mut self.state.settings.custom;
            editor.selected = editor
                .selected
                .min(state.document.widgets.len().saturating_sub(1));
            editor.state = Some(state.clone());
            if saved {
                editor.draft = None;
                editor.import = None;
                editor.preview = None;
            }
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
            if state.revision > 0
                && previous.is_none_or(|previous| previous.document.layout != state.document.layout)
            {
                self.state.zoomed = !state.document.layout.split_view;
                self.pointer.splits =
                    daedalus_ui::layout::PaneSplits::from_portable(&state.document.layout);
            }
            if state.revision > 0
                && previous.is_none_or(|previous| {
                    previous.document.assignments != state.document.assignments
                })
                && let Some(root) = self
                    .state
                    .active_workspace()
                    .map(|workspace| workspace.root_dir.clone())
            {
                self.project_setup_changed(&root, state.document.assignments.clone());
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
    }

    pub(super) fn save_workbench_layout(&mut self) {
        if let Some((id, state)) = self.state.workbench.as_ref()
            && Some(*id) == self.state.active_workspace_id
            && state.revision > 0
        {
            let mut document = state.document.clone();
            let layout = self.pointer.splits.portable(!self.state.zoomed);
            if document.layout != layout {
                document.layout = layout;
                self.request_workbench(
                    *id,
                    ClientMessage::UpdateWorkbench {
                        workspace_id: *id,
                        revision: state.revision,
                        document,
                    },
                );
            }
        }
    }

    pub(super) fn save_shared_pane_widgets(
        &mut self,
        id: WorkspaceId,
        widgets: daedalus_ui::workspace_widgets::WorkspaceWidgets,
    ) {
        if let Some((_, state)) = self
            .state
            .workbench
            .as_ref()
            .filter(|(workspace, _)| *workspace == id)
        {
            let mut document = state.document.clone();
            document.pane_widgets = Some(widgets.into());
            document.layout = self.pointer.splits.portable(!self.state.zoomed);
            self.request_workbench(
                id,
                ClientMessage::UpdateWorkbench {
                    workspace_id: id,
                    revision: state.revision,
                    document,
                },
            );
        }
    }

    pub(super) fn open_widget_details(&mut self, index: Option<usize>) {
        self.open_workspace_settings();
        if !self.state.settings.open {
            return;
        }
        self.state.settings.custom_page = true;
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
