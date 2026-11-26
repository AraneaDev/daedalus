use super::{AgentPreset, WorkspaceView, remove_last_grapheme};
use daedalus_proto::WorkspaceId;

#[derive(Debug, Clone, Default)]
pub struct ActionPaletteState {
    pub open: bool,
    pub selected_index: usize,
}

impl ActionPaletteState {
    pub fn open(&mut self) {
        self.open = true;
        self.selected_index = 0;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn move_up(&mut self, len: usize) {
        self.selected_index = if len == 0 {
            0
        } else if self.selected_index == 0 {
            len - 1
        } else {
            (self.selected_index - 1).min(len - 1)
        };
    }

    pub fn move_down(&mut self, len: usize) {
        self.selected_index = if len == 0 {
            0
        } else {
            (self.selected_index + 1) % len
        };
    }
}

/// State of the interactive Agent preset picker modal (`Ctrl+Shift+P`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PickerState {
    pub open: bool,
    pub selected_index: usize,
}

impl PickerState {
    pub fn toggle(&mut self) {
        self.open = !self.open;
        if self.selected_index >= AgentPreset::ALL.len() {
            self.selected_index = 0;
        }
    }

    pub fn open_with(&mut self, preset: AgentPreset) {
        self.open = true;
        self.selected_index = AgentPreset::ALL
            .iter()
            .position(|&p| p == preset)
            .unwrap_or(0);
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn move_up(&mut self) {
        let len = AgentPreset::ALL.len();
        self.selected_index = if self.selected_index == 0 {
            len - 1
        } else {
            self.selected_index - 1
        };
    }

    pub fn move_down(&mut self) {
        self.selected_index = (self.selected_index + 1) % AgentPreset::ALL.len();
    }

    pub fn selected_preset(&self) -> AgentPreset {
        AgentPreset::ALL[self.selected_index % AgentPreset::ALL.len()]
    }
}

/// State of the centered "New Git Worktree" branch-input modal (`Ctrl+Shift+W` / `+ Worktree`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorktreeModalState {
    session: u64,
    pub open: bool,
    pub branch_input: String,
    show_missing_branch: bool,
}

impl WorktreeModalState {
    pub fn session(&self) -> u64 {
        self.session
    }

    pub fn open(&mut self) {
        self.session += 1;
        self.open = true;
        self.branch_input.clear();
        self.show_missing_branch = false;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.branch_input.clear();
        self.show_missing_branch = false;
    }

    pub fn toggle(&mut self) {
        if self.open {
            self.close();
        } else {
            self.open();
        }
    }

    pub fn push_char(&mut self, ch: char) {
        self.branch_input.push(ch);
        self.show_missing_branch = false;
    }

    pub fn push_text(&mut self, text: &str) {
        self.branch_input.push_str(text);
        self.show_missing_branch = false;
    }

    pub fn set_text(&mut self, text: String) {
        self.branch_input = text;
        self.show_missing_branch = false;
    }

    pub fn missing_branch(&self) -> bool {
        self.show_missing_branch
    }

    pub fn branch_for_submit(&mut self) -> Option<String> {
        let branch = self.branch_input.trim();
        self.show_missing_branch = branch.is_empty();
        if self.show_missing_branch {
            None
        } else {
            Some(branch.to_owned())
        }
    }

    pub fn pop_char(&mut self) {
        remove_last_grapheme(&mut self.branch_input);
    }
}

/// State of the centered "Close Workspace" confirmation & worktree cleanup modal (`Ctrl+Shift+D` or sidebar `×`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CloseWorkspaceModalState {
    session: u64,
    pub submitting: bool,
    pub open: bool,
    pub workspace_id: Option<WorkspaceId>,
    pub workspace_name: String,
    pub branch: Option<String>,
    pub is_worktree: bool,
    pub is_dirty: bool,
    pub has_compose: bool,
    pub compose_down: bool,
    pub remove_worktree: bool,
    pub force_remove_worktree: bool,
}

impl CloseWorkspaceModalState {
    pub fn session(&self) -> u64 {
        self.session
    }

    /// Opens the modal populated with defaults for `ws`.
    pub fn open_for_workspace(&mut self, ws: &WorkspaceView) {
        self.session += 1;
        self.submitting = false;
        self.open = true;
        self.workspace_id = Some(ws.id);
        self.workspace_name = ws.name.clone();
        self.branch = ws.git_branch.clone();
        self.is_worktree = ws.git_status.map(|g| g.is_worktree).unwrap_or(false);
        self.is_dirty = ws.git_status.map(|g| g.dirty).unwrap_or(false);
        self.has_compose = ws.compose_project_name.is_some();
        self.compose_down = ws.compose_project_name.is_some();
        self.remove_worktree = false;
        self.force_remove_worktree = false;
    }

    pub fn close(&mut self) {
        *self = Self {
            session: self.session,
            ..Self::default()
        };
    }

    pub fn toggle_compose(&mut self) {
        if !self.submitting {
            self.compose_down = !self.compose_down;
        }
    }

    pub fn toggle_worktree(&mut self) {
        if self.submitting || !self.is_worktree {
            return;
        }
        self.remove_worktree = !self.remove_worktree;
        if !self.remove_worktree {
            self.force_remove_worktree = false;
        }
    }

    pub fn toggle_force(&mut self) {
        if !self.submitting && self.is_worktree && self.is_dirty && self.remove_worktree {
            self.force_remove_worktree = !self.force_remove_worktree;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_worktree_submission_keeps_the_field_open_until_corrected() {
        let mut form = WorktreeModalState::default();
        form.open();
        assert!(!form.missing_branch());
        assert_eq!(form.branch_for_submit(), None);
        assert!(form.open && form.missing_branch());
        form.push_text("topic");
        assert!(!form.missing_branch());
        assert_eq!(form.branch_for_submit().as_deref(), Some("topic"));
        form.close();
        form.open();
        assert!(!form.missing_branch());
    }

    #[test]
    fn action_palette_wraps_for_nonempty_and_empty_workspaces() {
        let mut palette = ActionPaletteState::default();
        palette.open();
        palette.move_up(32);
        assert_eq!(palette.selected_index, 31);
        palette.move_down(32);
        assert_eq!(palette.selected_index, 0);
        palette.move_up(0);
        palette.move_down(0);
        assert_eq!(palette.selected_index, 0);
        palette.close();
        assert!(!palette.open);
    }

    #[test]
    fn picker_state_wraps_around_and_presets_match_spec() {
        let mut picker = PickerState::default();
        assert!(!picker.open);
        assert_eq!(picker.selected_preset(), AgentPreset::Agy);

        picker.toggle();
        assert!(picker.open);

        // Wrap up from 0 -> Shell (index 3)
        picker.move_up();
        assert_eq!(picker.selected_index, 3);
        assert_eq!(picker.selected_preset(), AgentPreset::Shell);
        assert_eq!(picker.selected_preset().command(), None);

        // Wrap down from 3 -> Agy (index 0)
        picker.move_down();
        assert_eq!(picker.selected_index, 0);
        assert_eq!(picker.selected_preset(), AgentPreset::Agy);
        assert_eq!(
            picker.selected_preset().command(),
            Some(vec!["agy".to_string()])
        );

        picker.move_down();
        assert_eq!(picker.selected_preset(), AgentPreset::Claude);
        assert_eq!(picker.selected_preset().short_name(), "claude");
        assert_eq!(picker.selected_preset().label(), "claude — Claude Code");

        assert_eq!(
            AgentPreset::from_cli_name("CODEX"),
            Some(AgentPreset::Codex)
        );
        assert_eq!(AgentPreset::from_cli_name("unknown"), None);
    }
}
