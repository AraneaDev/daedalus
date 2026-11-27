use super::{DaedalusApp, FALLBACK_PANE_COLS, FALLBACK_PANE_ROWS};
use daedalus_proto::{Assignment, PaneRole, PaneStatus, WorkspaceId};
use daedalus_ui::bridge::{AssignmentLaunch, UiCommand};
use daedalus_ui::input::UiAction;
use daedalus_ui::state::{AgentPreset, ConnectionState, SessionAction};

impl DaedalusApp {
    pub(super) fn open_agent_picker(&mut self) {
        let current = self
            .state
            .pane_for_role(PaneRole::Agent)
            .and_then(|p| p.agent_preset)
            .unwrap_or(self.config.default_agent);
        self.state.open_picker_with(current);
        self.request_redraw();
    }

    pub(super) fn open_worktree_modal(&mut self) {
        self.state.open_worktree_modal();
        self.request_redraw();
    }

    pub(super) fn open_close_modal_for_workspace(&mut self, workspace_id: WorkspaceId) {
        if self.state.open_close_modal_for_workspace(workspace_id) {
            self.request_redraw();
        }
    }

    pub(super) fn confirm_close_workspace(&mut self) {
        let Some(id) = self.state.close_modal.workspace_id else {
            self.state.close_modal.close();
            self.request_redraw();
            return;
        };
        if self.state.close_modal.submitting {
            return;
        }
        if self.state.connection != ConnectionState::Connected {
            self.show_toast(
                "Reconnect to the daemon before closing this workspace.",
                true,
            );
            return;
        }
        let compose_down = self.state.close_modal.compose_down;
        let remove_worktree = self.state.close_modal.remove_worktree;
        let force_remove_worktree = self.state.close_modal.force_remove_worktree;
        self.state.close_modal.submitting = self.send_command(UiCommand::CloseWorkspace {
            id,
            compose_down,
            remove_worktree,
            force_remove_worktree,
        });
        self.request_redraw();
    }

    pub(super) fn submit_worktree_modal(&mut self) {
        if self.state.connection != ConnectionState::Connected {
            return;
        }
        let Some(branch) = self.state.worktree_modal.branch_for_submit() else {
            self.request_redraw();
            return;
        };
        let repo_dir = self
            .state
            .active_workspace()
            .map(|ws| ws.root_dir.clone())
            .unwrap_or_else(|| self.folder_suggestion());
        if self.send_command(UiCommand::CreateWorktreeWorkspace {
            repo_dir,
            branch,
            default_agent: self.config.default_agent,
        }) {
            self.state.worktree_modal.close();
        }
        self.request_redraw();
    }

    pub(super) fn apply_agent_preset(&mut self, preset: AgentPreset, add: bool) {
        self.state.picker.close();
        let Some(workspace_id) = self.state.active_workspace_id else {
            self.show_toast("No active workspace for Agent preset switch", true);
            return;
        };
        let old_pane_id = self.state.pane_id_for_role(PaneRole::Agent);
        let (cols, rows) = self
            .compute_current_layout()
            .and_then(|layout| layout.pane(PaneRole::Agent).map(|p| (p.cols, p.rows)))
            .unwrap_or((FALLBACK_PANE_COLS, FALLBACK_PANE_ROWS));

        self.send_command(if add {
            UiCommand::AddAgent {
                workspace_id,
                preset,
                cols,
                rows,
            }
        } else {
            UiCommand::ReplaceAgentPreset {
                workspace_id,
                old_pane_id,
                preset,
                cols,
                rows,
            }
        });
        self.state.focused_role = PaneRole::Agent;
        self.request_redraw();
    }

    fn session_launches(&self, workspace: WorkspaceId) -> Result<(Vec<Assignment>, bool), String> {
        let ws = self
            .state
            .active_workspace()
            .filter(|ws| ws.id == workspace)
            .ok_or("Select a workspace to manage its sessions.")?;
        let defaults =
            super::workspace_settings::initial_assignments(ws, self.config.default_agent);
        let (mut assignments, baseline) =
            self.project_setups.settings_draft(workspace, defaults)?;
        let saved = baseline
            .assignments
            .as_ref()
            .is_some_and(|entries| !entries.is_empty());
        if assignments.is_empty() {
            assignments.push(Assignment {
                id: "default-agent-shell".into(),
                name: "Shell".into(),
                role: PaneRole::Agent,
                argv: Vec::new(),
            });
        }
        Ok((assignments, saved))
    }

    pub(super) fn open_sessions(&mut self) {
        if self.state.connection != ConnectionState::Connected {
            self.show_toast("Reconnect to manage sessions.", true);
            return;
        }
        let Some(workspace) = self.state.active_workspace_id else {
            return;
        };
        let mut actions: Vec<_> = match self.session_launches(workspace) {
            Ok((assignments, saved)) => assignments
                .into_iter()
                .map(|assignment| SessionAction::Launch { assignment, saved })
                .collect(),
            Err(error) => {
                self.show_toast(error, true);
                Vec::new()
            }
        };
        if let Some(ws) = self.state.active_workspace() {
            actions.extend(
                self.state
                    .terminal_tabs(ws)
                    .into_iter()
                    .filter_map(|(id, title)| {
                        let status = self.state.panes.get(&id)?.summary.status;
                        (!matches!(status, PaneStatus::Exited { .. }))
                            .then_some(SessionAction::Stop(id, title))
                    }),
            );
        }
        self.pointer.end_drag();
        self.state.open_action_palette();
        self.state.action_palette.session_actions = Some(actions);
        self.request_redraw();
    }

    fn confirm_session_action(&mut self, action: SessionAction) {
        if self.state.connection != ConnectionState::Connected {
            self.show_toast("Reconnect to manage sessions.", true);
            return;
        }
        let Some(workspace) = self.state.active_workspace_id else {
            return;
        };
        let sent = match action {
            SessionAction::Launch { assignment, saved } => {
                let (assignments, currently_saved) = match self.session_launches(workspace) {
                    Ok(current) => current,
                    Err(error) => {
                        self.show_toast(error, true);
                        return;
                    }
                };
                if saved != currently_saved || !assignments.contains(&assignment) {
                    self.show_toast("Saved sessions changed. Open Launch again.", false);
                    return;
                }
                let launch = if saved {
                    AssignmentLaunch::ReviewedRunner
                } else {
                    AssignmentLaunch::Direct
                };
                self.launch_assignment(workspace, assignment, launch)
            }
            SessionAction::Stop(id, _) => {
                let active = self.state.active_workspace().is_some_and(|ws| {
                    self.state.workspace_panes(ws).any(|pane| {
                        pane.id == id && !matches!(pane.status, PaneStatus::Exited { .. })
                    })
                });
                if !active {
                    return;
                }
                self.send_command(UiCommand::ClosePane(id))
            }
        };
        if !sent {
            self.show_toast("Reconnect to manage sessions.", true);
        }
    }

    pub(super) fn confirm_workspace_action(&mut self) {
        let selected = self.state.action_palette.selected_index;
        if let Some(actions) = self.state.action_palette.session_actions.take() {
            self.state.action_palette.close();
            if let Some(action) = actions.into_iter().nth(selected) {
                self.confirm_session_action(action);
            }
            self.request_redraw();
            return;
        }
        if let (Some(workspace_id), Some(action)) = (
            self.state.active_workspace_id,
            self.state.active_actions().get(selected),
        ) {
            self.send_command(UiCommand::RunWorkspaceAction {
                workspace_id,
                action_id: action.id.clone(),
            });
        }
        self.state.action_palette.close();
        self.request_redraw();
    }

    pub(super) fn handle_dialog_action(&mut self, action: UiAction) {
        match action {
            UiAction::ToggleActionPalette => {
                if self.state.action_palette.open {
                    self.state.action_palette.close();
                } else {
                    self.pointer.end_drag();
                    self.state.open_action_palette();
                }
                self.request_redraw();
            }
            UiAction::ActionPaletteUp | UiAction::ActionPaletteDown => {
                let count = self.state.palette_len();
                if action == UiAction::ActionPaletteUp {
                    self.state.action_palette.move_up(count);
                } else {
                    self.state.action_palette.move_down(count);
                }
                self.request_redraw();
            }
            UiAction::ActionPaletteConfirm => self.confirm_workspace_action(),
            UiAction::ActionPaletteCancel => {
                self.state.action_palette.close();
                self.request_redraw();
            }
            UiAction::ToggleAgentPicker => {
                if self.state.picker.open {
                    self.state.picker.close();
                    self.request_redraw();
                } else {
                    self.open_agent_picker();
                }
            }
            UiAction::ToggleWorktreeModal => {
                self.state.toggle_worktree_modal();
                self.request_redraw();
            }
            UiAction::OpenCloseWorkspaceModal => {
                if let Some(ws_id) = self.state.active_workspace_id {
                    self.open_close_modal_for_workspace(ws_id);
                } else {
                    self.show_toast("No active workspace to close", true);
                }
            }
            UiAction::PickerUp => {
                self.state.picker.move_up();
                self.request_redraw();
            }
            UiAction::PickerDown => {
                self.state.picker.move_down();
                self.request_redraw();
            }
            UiAction::PickerConfirm | UiAction::PickerAdd => {
                let preset = self.state.picker.selected_preset();
                self.apply_agent_preset(preset, action == UiAction::PickerAdd);
            }
            UiAction::PickerCancel => {
                self.state.picker.close();
                self.request_redraw();
            }
            UiAction::WorktreeModalText(text) => {
                self.state.worktree_modal.push_text(&text);
                self.request_redraw();
            }
            UiAction::WorktreeModalSetText(text) => {
                self.state.worktree_modal.set_text(text);
                self.request_redraw();
            }
            UiAction::WorktreeModalBackspace => {
                self.state.worktree_modal.pop_char();
                self.request_redraw();
            }
            UiAction::WorktreeModalConfirm => self.submit_worktree_modal(),
            UiAction::WorktreeModalCancel => {
                self.state.worktree_modal.close();
                self.request_redraw();
            }
            UiAction::CloseModalToggleCompose => {
                self.state.close_modal.toggle_compose();
                self.request_redraw();
            }
            UiAction::CloseModalToggleWorktree => {
                self.state.close_modal.toggle_worktree();
                self.request_redraw();
            }
            UiAction::CloseModalToggleForce => {
                self.state.close_modal.toggle_force();
                self.request_redraw();
            }
            UiAction::CloseModalConfirm => self.confirm_close_workspace(),
            UiAction::CloseModalCancel => {
                if self.state.close_modal.submitting {
                    return;
                }
                self.state.close_modal.close();
                self.request_redraw();
            }
            _ => unreachable!("action routed to the wrong handler"),
        }
    }
}
