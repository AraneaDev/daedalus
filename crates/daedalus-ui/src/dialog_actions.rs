use super::DaedalusApp;
use daedalus_ui::bridge::UiCommand;
use daedalus_ui::input::UiAction;

impl DaedalusApp {
    pub(super) fn confirm_workspace_action(&mut self) {
        let selected = self.state.action_palette.selected_index;
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
                let count = self.state.active_actions().len();
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
