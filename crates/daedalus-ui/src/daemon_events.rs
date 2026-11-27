use super::{DaedalusApp, play_notification_ping};
use daedalus_proto::{PaneId, PaneRole, ServerEvent, WorkspaceId, WorkspaceInfo};
use daedalus_ui::{
    bridge::{UiCommand, UiEvent},
    state::ConnectionState,
};

impl DaedalusApp {
    pub(super) fn handle_daemon_event(&mut self, event: UiEvent) {
        let previous_workspace = self.state.active_workspace_id;
        let search_generation = self.state.search.as_ref().map(|search| search.generation);
        self.state
            .recap
            .observe(&event, &self.state.workspaces, &self.state.panes);
        if let Some(id) = self.history_workspace_for_event(&event) {
            self.history.changed(id);
        }
        self.sync_history_visit();
        match event {
            UiEvent::WorkbenchCapability(supported) => self.workbench_capable = supported,
            UiEvent::Workbench {
                workspace_id,
                operation,
                response,
            } => self.workbench_response(workspace_id, operation, *response),
            UiEvent::ImagePreview {
                pane_id,
                token,
                result,
            } => {
                if self.state.pending_image_preview == Some((pane_id, token)) {
                    self.state.pending_image_preview = None;
                    if self.state.focused_pane_id() == Some(pane_id) {
                        match result {
                            Ok(image) if image.valid() => {
                                self.state.image_preview = Some((pane_id, image));
                            }
                            Ok(_) => self.show_toast("Invalid preview received.", true),
                            Err(error) => self.show_toast(error, true),
                        }
                    }
                }
            }
            UiEvent::JournalCapability(supported) => self.history.connection(supported),
            UiEvent::DaemonCapabilities {
                persistence_health,
                home_dir,
                project_setups,
                recent_projects,
            } => {
                self.state.persistence_health = persistence_health;
                self.daemon_home = home_dir;
                self.project_setups.set_daemon_backed(project_setups);
                if !recent_projects {
                    // Rows from the previous daemon would name folders this one never saw.
                    self.state.workspace_form.set_recent(Vec::new());
                }
            }
            UiEvent::RecentProjectsLoaded(projects) => {
                self.hide_open_recent_projects();
                self.state.workspace_form.set_recent(projects);
            }
            UiEvent::ProjectSetupChanged {
                root_dir,
                assignments,
            } => {
                if self.workbench_capable {
                    self.send_command(UiCommand::ReconcileWorkspaceMembership);
                } else {
                    self.project_setup_changed(&root_dir, assignments);
                }
            }
            UiEvent::ProjectSetupMigrationFinished {
                workspace_id,
                request_id,
                result,
            } => {
                if self
                    .project_setups
                    .finish_migration(workspace_id, request_id, result.is_ok())
                {
                    self.refresh_workspace_setup(workspace_id);
                    if let Err(message) = result {
                        self.show_toast(message, true);
                    }
                }
            }
            UiEvent::WorkspaceSettingsSaved {
                workspace_id,
                request_id,
                result,
            } => {
                self.workspace_settings_saved(workspace_id, request_id, result);
            }
            UiEvent::ActivityRead { token, page } => {
                use daedalus_ui::activity_history::PageUpdate;
                match self.history.received(token, &page) {
                    Some(PageUpdate::Recap { first, gap }) => {
                        self.state
                            .recap
                            .journal(&page, first, gap, &self.state.panes)
                    }
                    Some(PageUpdate::Foreground(count)) => self
                        .state
                        .recap
                        .foreground_activity(page.workspace_id, count),
                    None => {}
                }
            }
            UiEvent::ActivityReadFailed(token) => self.history.failed(token),
            UiEvent::Connected => {
                if self.state.connection == ConnectionState::Reconnecting {
                    self.state.workbench = None;
                }
                self.state.connection = ConnectionState::Connected;
                if self.state.workspace_list_loaded {
                    self.refresh_inactive_layout_saves();
                    self.ensure_active_workspace_panes();
                }
                self.schedule_resize_flush();
            }
            UiEvent::Disconnected => {
                self.state.persistence_health = None;
                self.state.pending_image_preview = None;
                self.pause_layout_saves();
                self.seeding_workbench.clear();
                self.watched_workbench = None;
                self.widget_refresh_at = None;
                self.setup_disconnected();
                self.workspace_settings_disconnected();
                self.pane_widget_save_disconnected();
                if self.state.settings.custom.interrupt_previews() && self.state.settings.open {
                    self.state.settings.error =
                        Some("Connection lost. Run the preview again when reconnected.".into());
                }
                self.history.connection(false);
                self.state.connection = ConnectionState::Reconnecting;
                self.state.workspace_list_loaded = false;
                let form = &mut self.state.workspace_form;
                form.finish_submission(
                    form.session(),
                    Some("Connection lost. Check the workspace list before retrying.".into()),
                );
                self.state.close_modal.submitting = false;
                self.state.last_sent_sizes.clear();
                self.state.spawning_workspaces.clear();
                self.schedule_resize_flush();
            }
            UiEvent::WorkspaceCloseFinished(id) => {
                if self.state.close_modal.workspace_id == Some(id) {
                    self.state.close_modal.submitting = false;
                }
            }
            UiEvent::WorkspacesLoaded(workspaces) => self.apply_workspace_list(workspaces, true),
            UiEvent::WorkspaceMembershipLoaded(workspaces)
            | UiEvent::WorkspaceCloseFailed(workspaces) => {
                self.apply_workspace_list(workspaces, false);
            }
            UiEvent::SetupInspected { session, result } => self.setup_inspected(session, result),
            UiEvent::SetupInstallerStarted {
                workspace_id,
                pane_id,
            } => self.setup_installer_started(workspace_id, pane_id),
            UiEvent::SetupInstallerFinished(pane, code) => {
                self.setup_installer_finished(pane, code)
            }
            UiEvent::SetupInstallerFailed {
                workspace_id,
                message,
            } => self.setup_failed(workspace_id, message),
            UiEvent::SetupApplied {
                workspace_id,
                error,
            } => self.setup_applied(workspace_id, error),
            UiEvent::ManualWorkspaceCreated { session, workspace } => {
                self.setup_created(session, workspace);
            }
            UiEvent::ManualWorkspaceFailed { session, message } => {
                self.setup_creation_failed(session, message)
            }
            UiEvent::WorkspaceCreated(ws) => {
                let ws_id = ws.id;
                self.project_setups.absorb(std::slice::from_ref(&ws));
                self.state.upsert_workspace(ws);
                self.state.select_workspace(ws_id);
                self.state.close_search();
                self.schedule_resize_flush();
            }
            UiEvent::WorkspacePanesEnsured {
                workspace_id,
                complete,
            } => {
                self.state.finish_workspace_spawning(workspace_id, complete);
            }
            UiEvent::PaneSpawned {
                summary,
                select_tab,
            } => {
                self.state.register_pane(summary, select_tab);
                self.schedule_resize_flush();
            }
            UiEvent::AssignmentStarted {
                workspace_id,
                pane_id,
                assignment,
            } => self.assignment_started(workspace_id, pane_id, assignment),
            UiEvent::PaneClosed { pane_id } => {
                self.setup_installer_finished(pane_id, None);
                self.state.remove_session(pane_id);
                self.schedule_resize_flush();
            }
            UiEvent::Snapshot(snapshot) => {
                let pane_id = snapshot.pane_id;
                if let Some(pane_id) = self.state.apply_grid_snapshot(snapshot) {
                    self.send_command(UiCommand::RequestSnapshot { pane_id });
                } else {
                    self.request_scrollback_if_needed(pane_id);
                }
            }
            UiEvent::ScrollbackLoaded {
                pane_id,
                generation,
                total_scrollback,
                rows,
            } => {
                if let Some(pane) = self.state.panes.get_mut(&pane_id) {
                    pane.apply_scrollback(generation, total_scrollback, rows);
                }
            }
            UiEvent::SearchResults {
                pane_id,
                generation,
                total_scrollback,
                matches,
            } => {
                if self
                    .state
                    .apply_search_results(pane_id, generation, total_scrollback, matches)
                {
                    self.scroll_to_active_search_match();
                }
            }
            UiEvent::AgentStateChanged {
                workspace_id,
                pane_id,
                state,
            } => {
                self.state.apply_agent_state(workspace_id, pane_id, state);
            }
            UiEvent::WorkspaceRuntimeUpdated {
                workspace_id,
                bound_ports,
                containers,
                git_status,
                git_branch,
            } => {
                self.state.apply_workspace_runtime(
                    workspace_id,
                    bound_ports,
                    containers,
                    git_status,
                    git_branch,
                );
            }
            UiEvent::ServerEvent(ServerEvent::WorkbenchChanged { workspace_id, .. }) => {
                self.request_workbench(
                    workspace_id,
                    daedalus_proto::ClientMessage::GetWorkbench { workspace_id },
                );
            }
            UiEvent::ServerEvent(server_event) => self.apply_terminal_event(server_event),
            UiEvent::Toast { message, is_error } => {
                let had_banner = self.has_top_banner();
                self.state.show_toast(message, is_error);
                if !had_banner {
                    self.schedule_resize_flush();
                }
            }
        }

        if let Some(search) = &self.state.search
            && Some(search.generation) != search_generation
        {
            self.run_pane_search();
        }
        self.state.recap.select(self.state.active_workspace_id);
        if previous_workspace != self.state.active_workspace_id {
            self.reveal_active_workspace();
        }
        self.flush_due_resizes(false);
        self.request_redraw();
    }

    fn apply_workspace_list(&mut self, workspaces: Vec<WorkspaceInfo>, initialize: bool) {
        let previously_active = self.state.active_workspace_id;
        let new_panes = if initialize {
            Vec::new()
        } else {
            workspaces
                .iter()
                .flat_map(|workspace| &workspace.panes)
                .filter_map(|pane| (!self.state.panes.contains_key(&pane.id)).then_some(pane.id))
                .collect()
        };
        self.project_setups.absorb(&workspaces);
        for migration in self.project_setups.migrations(&workspaces) {
            self.send_setup_migration(migration);
        }
        self.restore_session_names(&workspaces);
        self.state.set_workspaces(workspaces);
        self.prune_layout_saves();
        self.reconcile_setup_installer();
        if initialize {
            self.refresh_inactive_layout_saves();
            self.offer_first_workspace();
        }
        if initialize || self.state.active_workspace_id != previously_active {
            self.ensure_active_workspace_panes();
        }
        for pane_id in new_panes {
            self.send_command(UiCommand::RequestSnapshot { pane_id });
        }
        self.schedule_resize_flush();
    }

    fn agent_output_pane(&self, event: &ServerEvent) -> Option<PaneId> {
        match event {
            ServerEvent::GridDelta(delta)
                if !delta.dirty_rows.is_empty()
                    && self
                        .state
                        .panes
                        .get(&delta.pane_id)
                        .is_some_and(|pane| pane.summary.role == PaneRole::Agent) =>
            {
                Some(delta.pane_id)
            }
            ServerEvent::ResyncRequired { pane_id } => Some(*pane_id),
            _ => None,
        }
    }

    fn history_workspace_for_event(&self, event: &UiEvent) -> Option<WorkspaceId> {
        match event {
            UiEvent::AgentStateChanged { workspace_id, .. }
            | UiEvent::WorkspaceRuntimeUpdated { workspace_id, .. }
            | UiEvent::ServerEvent(ServerEvent::ExternalStatusUpdated { workspace_id, .. }) => {
                Some(*workspace_id)
            }
            UiEvent::ServerEvent(event) => {
                let pane_id = match event {
                    ServerEvent::CommandFinished { pane_id, .. }
                    | ServerEvent::PaneExited { pane_id, .. } => Some(*pane_id),
                    _ => self.agent_output_pane(event),
                }?;
                self.state
                    .panes
                    .get(&pane_id)
                    .map(|pane| pane.summary.workspace_id)
            }
            _ => None,
        }
    }

    fn apply_terminal_event(&mut self, server_event: ServerEvent) {
        let ended = match &server_event {
            ServerEvent::PaneExited { pane_id, exit_code } => Some((*pane_id, *exit_code)),
            _ => None,
        };
        let changed_pane = self.agent_output_pane(&server_event);
        let watch_attention = match &server_event {
            ServerEvent::GridDelta(delta)
                if self.ui_config.notification_ping
                    && self
                        .state
                        .panes
                        .get(&delta.pane_id)
                        .is_some_and(|p| !p.unread_attention) =>
            {
                Some(delta.pane_id)
            }
            _ => None,
        };
        if let Some(resync_pane_id) = self.state.apply_server_event(server_event) {
            self.send_command(UiCommand::RequestSnapshot {
                pane_id: resync_pane_id,
            });
        }
        self.sync_history_visit();
        if let Some(id) = watch_attention
            && self
                .state
                .panes
                .get(&id)
                .is_some_and(|p| p.unread_attention)
            && let Some(window) = &self.window
        {
            play_notification_ping();
            window.request_user_attention(Some(winit::window::UserAttentionType::Informational));
        }
        if let Some((id, code)) = ended {
            self.setup_installer_finished(id, code);
        }
        if let Some(pane_id) = changed_pane {
            self.request_scrollback_if_needed(pane_id);
        }
    }
}
