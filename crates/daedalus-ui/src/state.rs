use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use clap::ValueEnum;
use daedalus_proto::{
    AgentState, ContainerHealth, CursorState, ExternalStatus, FailureTrace, GitWorkspaceStatus,
    GridDelta, GridRow, GridSnapshot, PaneId, PaneRole, PaneStatus, PaneSummary, PortMapping,
    SearchMatch, ServerEvent, WorkspaceId, WorkspaceInfo,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::selection::Selection;

/// Directional focus navigation across the fixed 3-pane semantic layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FocusDirection {
    Left,
    Right,
    Up,
    Down,
}

/// First-class Agent pane CLI presets selectable via `--agent` or the in-app picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, ValueEnum)]
pub enum AgentPreset {
    #[default]
    Agy,
    Claude,
    Codex,
    Shell,
}

impl AgentPreset {
    pub const ALL: [Self; 4] = [Self::Agy, Self::Claude, Self::Codex, Self::Shell];

    pub const fn label(&self) -> &'static str {
        match self {
            Self::Agy => "agy — Antigravity CLI",
            Self::Claude => "claude — Claude Code",
            Self::Codex => "codex — OpenAI Codex CLI",
            Self::Shell => "shell — Interactive Shell",
        }
    }

    pub const fn short_name(&self) -> &'static str {
        match self {
            Self::Agy => "agy",
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Shell => "shell",
        }
    }

    pub fn command(&self) -> Option<Vec<String>> {
        match self {
            Self::Agy => Some(vec!["agy".to_string()]),
            Self::Claude => Some(vec!["claude".to_string()]),
            Self::Codex => Some(vec!["codex".to_string()]),
            Self::Shell => None,
        }
    }

    pub fn from_cli_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "agy" => Some(Self::Agy),
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "shell" => Some(Self::Shell),
            _ => None,
        }
    }
}

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

fn remove_last_grapheme(text: &mut String) -> bool {
    let Some((start, _)) = text.grapheme_indices(true).next_back() else {
        return false;
    };
    text.truncate(start);
    true
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
    /// Opens the modal populated with defaults for `ws`.
    pub fn open_for_workspace(&mut self, ws: &WorkspaceInfo) {
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
        *self = Self::default();
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

/// State of the per-pane scrollback search overlay (`Ctrl+Shift+F`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchState {
    pub pane_id: PaneId,
    pub query: String,
    pub case_sensitive: bool,
    pub generation: u64,
    pub total_scrollback: usize,
    pub matches: Vec<SearchMatch>,
    pub current_index: usize,
    results_received: bool,
}

static NEXT_SEARCH_GENERATION: AtomicU64 = AtomicU64::new(1);

impl SearchState {
    fn next_generation() -> u64 {
        NEXT_SEARCH_GENERATION.fetch_add(1, Ordering::Relaxed)
    }

    pub fn new(pane_id: PaneId) -> Self {
        Self {
            pane_id,
            query: String::new(),
            case_sensitive: false,
            generation: Self::next_generation(),
            total_scrollback: 0,
            matches: Vec::new(),
            current_index: 0,
            results_received: false,
        }
    }

    fn invalidate_results(&mut self) {
        self.generation = Self::next_generation();
        self.matches.clear();
        self.current_index = 0;
        self.results_received = false;
    }

    pub fn result_label(&self) -> Option<String> {
        if self.query.is_empty() || !self.results_received {
            return None;
        }
        if self.matches.is_empty() {
            return Some("No matches".into());
        }
        let (current, total) = self.display_counter();
        Some(format!("({current}/{total})"))
    }

    pub fn push_text(&mut self, text: &str) {
        if !text.is_empty() {
            self.query.push_str(text);
            self.invalidate_results();
        }
    }

    pub fn pop_char(&mut self) -> bool {
        let popped = remove_last_grapheme(&mut self.query);
        if popped {
            self.invalidate_results();
        }
        popped
    }

    pub fn set_results(&mut self, total_scrollback: usize, matches: Vec<SearchMatch>) {
        self.total_scrollback = total_scrollback;
        self.matches = matches;
        self.results_received = true;
        self.current_index = 0;
    }

    /// Applies search results only if `generation` matches the current query generation
    /// and the query is non-empty, ignoring stale out-of-order responses.
    pub fn apply_results(
        &mut self,
        generation: u64,
        total_scrollback: usize,
        matches: Vec<SearchMatch>,
    ) -> bool {
        if self.generation != generation || self.query.is_empty() {
            return false;
        }
        self.set_results(total_scrollback, matches);
        true
    }

    pub fn active_match(&self) -> Option<SearchMatch> {
        self.matches.get(self.current_index).copied()
    }

    pub fn step_next(&mut self) -> Option<SearchMatch> {
        if self.matches.is_empty() {
            self.current_index = 0;
            return None;
        }
        self.current_index = (self.current_index + 1) % self.matches.len();
        self.active_match()
    }

    pub fn step_prev(&mut self) -> Option<SearchMatch> {
        if self.matches.is_empty() {
            self.current_index = 0;
            return None;
        }
        self.current_index = if self.current_index == 0 {
            self.matches.len() - 1
        } else {
            self.current_index - 1
        };
        self.active_match()
    }

    pub fn display_counter(&self) -> (usize, usize) {
        if self.matches.is_empty() {
            (0, 0)
        } else {
            (self.current_index + 1, self.matches.len())
        }
    }

    /// Computes the desired `scrollback_offset` to center/reveal the active [`SearchMatch`]
    /// in a viewport of `viewport_rows` rows, where `abs_row` spans
    /// `0..total_scrollback + viewport_rows` and the live viewport starts at `total_scrollback`.
    pub fn target_scroll_offset(&self, viewport_rows: usize) -> Option<usize> {
        self.target_scroll_offset_with_live_start(viewport_rows, None)
    }

    /// Computes the desired `scrollback_offset` using an optional explicit `live_start_abs_row`
    /// (supporting panes where oldest scrollback rows have been evicted past the history cap).
    pub fn target_scroll_offset_with_live_start(
        &self,
        viewport_rows: usize,
        live_start_abs_row: Option<u64>,
    ) -> Option<usize> {
        let active = self.active_match()?;
        let vp = viewport_rows.max(1);
        let live_start = live_start_abs_row.unwrap_or(self.total_scrollback as u64);
        if active.abs_row >= live_start {
            return Some(0);
        }
        let rows_above_live = (live_start - active.abs_row) as usize;
        let offset = rows_above_live
            .saturating_add(vp / 2)
            .min(self.total_scrollback);
        Some(offset)
    }
}

static NEXT_SCROLLBACK_GENERATION: AtomicU64 = AtomicU64::new(1);

/// Client-side cached terminal state for a single daemon pane.
#[derive(Debug, Clone)]
pub struct PaneViewState {
    pub summary: PaneSummary,
    pub cols: u16,
    pub rows: u16,
    pub cursor: CursorState,
    pub viewport_rows: Vec<GridRow>,
    pub scrollback_len: usize,
    /// Offset of the rows currently displayed.
    pub scroll_offset: usize,
    /// Latest scroll target, including requests awaiting a daemon reply.
    pub requested_scroll_offset: usize,
    pending_scrollback: Option<u64>,
    pub(crate) history_generation: u64,
    pub scrolled_rows: Option<Vec<GridRow>>,
    pub dirty_row_indices: Vec<usize>,
    pub selection: Option<Selection>,
    pub agent_preset: Option<AgentPreset>,
    pub last_failure_trace: Option<FailureTrace>,
    pub last_seq: u64,
}

impl PaneViewState {
    pub fn from_summary(summary: PaneSummary) -> Self {
        let cols = summary.cols;
        let rows = summary.rows;
        Self {
            summary,
            cols,
            rows,
            cursor: CursorState::default(),
            viewport_rows: Vec::new(),
            scrollback_len: 0,
            scroll_offset: 0,
            requested_scroll_offset: 0,
            pending_scrollback: None,
            history_generation: 0,
            scrolled_rows: None,
            dirty_row_indices: Vec::new(),
            selection: None,
            agent_preset: None,
            last_failure_trace: None,
            last_seq: 0,
        }
    }

    fn update_history_generation(&mut self, generation: u64) -> bool {
        if generation == self.history_generation {
            return false;
        }
        self.history_generation = generation;
        if self.scroll_offset > 0 {
            self.selection = None;
        }
        self.requested_scroll_offset = 0;
        self.scroll_offset = 0;
        self.pending_scrollback = None;
        self.scrolled_rows = None;
        self.mark_all_visible_dirty();
        true
    }

    /// Replaces the cached viewport with a full `GridSnapshot` and marks all rows dirty.
    /// Returns true when saved history was erased since the last update.
    pub fn apply_snapshot(&mut self, snapshot: GridSnapshot) -> bool {
        let erased = self.update_history_generation(snapshot.history_generation);
        self.cols = snapshot.cols;
        self.rows = snapshot.rows;
        self.summary.cols = snapshot.cols;
        self.summary.rows = snapshot.rows;
        self.cursor = snapshot.cursor;
        self.last_seq = snapshot.seq;
        self.scrollback_len = snapshot.scrollback_len;
        self.viewport_rows = snapshot.rows_data;
        self.requested_scroll_offset = self.requested_scroll_offset.min(self.scrollback_len);
        self.scroll_offset = 0;
        self.pending_scrollback = None;
        self.scrolled_rows = None;
        self.mark_all_visible_dirty();
        erased
    }

    /// Patches only the rows present in `delta` and records their indices for incremental shaping.
    /// Returns true when saved history was erased since the last update.
    pub fn apply_delta(&mut self, delta: GridDelta) -> bool {
        if (self.last_seq != 0 && delta.seq <= self.last_seq)
            || delta.history_generation < self.history_generation
        {
            return false;
        }
        let erased = self.update_history_generation(delta.history_generation);
        if let Some(len) = delta.scrollback_len {
            self.scrollback_len = len;
        }
        let resized = self.cols != delta.cols || self.rows != delta.rows;
        self.cols = delta.cols;
        self.rows = delta.rows;
        self.summary.cols = delta.cols;
        self.summary.rows = delta.rows;
        self.cursor = delta.cursor;
        self.last_seq = delta.seq;

        let target_rows = delta.rows as usize;
        if self.viewport_rows.len() > target_rows {
            self.viewport_rows.truncate(target_rows);
        }

        for row in delta.dirty_rows {
            let idx = row.row_index as usize;
            if idx >= self.viewport_rows.len() {
                for fill_idx in self.viewport_rows.len()..=idx {
                    self.viewport_rows.push(GridRow {
                        row_index: fill_idx as u16,
                        abs_row: 0,
                        cells: Vec::new(),
                    });
                }
            }
            self.viewport_rows[idx] = row;
            if !self.dirty_row_indices.contains(&idx) {
                self.dirty_row_indices.push(idx);
            }
        }

        if resized {
            self.scroll_offset = 0;
            self.pending_scrollback = None;
            self.scrolled_rows = None;
            self.mark_all_visible_dirty();
        }
        erased
    }

    /// Claims a request for the desired history viewport, unless already loaded or pending.
    pub fn begin_scrollback_request(&mut self) -> Option<u64> {
        if self.requested_scroll_offset == self.scroll_offset || self.pending_scrollback.is_some() {
            return None;
        }
        let generation = NEXT_SCROLLBACK_GENERATION.fetch_add(1, Ordering::Relaxed);
        self.pending_scrollback = Some(generation);
        Some(generation)
    }

    pub fn apply_scrollback(
        &mut self,
        generation: u64,
        total_scrollback: usize,
        rows: Vec<GridRow>,
    ) {
        if self.pending_scrollback != Some(generation) {
            return;
        }
        self.pending_scrollback = None;
        self.scrollback_len = total_scrollback;
        self.requested_scroll_offset = self.requested_scroll_offset.min(total_scrollback);
        self.scroll_offset = self.requested_scroll_offset;
        if self.scroll_offset == 0 {
            self.scrolled_rows = None;
        } else {
            self.scrolled_rows = Some(rows);
        }
        self.mark_all_visible_dirty();
    }

    /// Returns the rows currently visible in the pane viewport (scrolled history or live viewport).
    pub fn visible_rows(&self) -> &[GridRow] {
        if self.scroll_offset > 0 {
            self.scrolled_rows.as_deref().unwrap_or_default()
        } else {
            &self.viewport_rows
        }
    }

    pub fn set_scroll_offset(&mut self, offset: usize) {
        if self.requested_scroll_offset != offset {
            self.requested_scroll_offset = offset;
            self.pending_scrollback = None;
            if offset == 0 {
                self.scroll_offset = 0;
                self.scrolled_rows = None;
                self.mark_all_visible_dirty();
            }
        }
    }

    /// Adjusts the requested offset by `delta` (positive = scroll up into history,
    /// negative = scroll down toward live output), clamped to `0..=scrollback_len`.
    pub fn adjust_scroll_offset(&mut self, delta: isize) -> usize {
        let candidate = if delta >= 0 {
            self.requested_scroll_offset.saturating_add(delta as usize)
        } else {
            self.requested_scroll_offset
                .saturating_sub(delta.unsigned_abs())
        };
        let clamped = candidate.min(self.scrollback_len);
        self.set_scroll_offset(clamped);
        self.requested_scroll_offset
    }

    /// Returns the pane to the live bottom viewport (`scroll_offset = 0`).
    pub fn reset_scroll(&mut self) {
        self.set_scroll_offset(0);
    }

    /// Drains and returns dirty row indices that need `glyphon` re-shaping.
    pub fn take_dirty_rows(&mut self) -> Vec<usize> {
        std::mem::take(&mut self.dirty_row_indices)
    }

    pub fn mark_all_visible_dirty(&mut self) {
        let len = self.visible_rows().len();
        self.dirty_row_indices = (0..len).collect();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Connecting,
    Connected,
    Reconnecting,
}

#[derive(Debug)]
pub struct FailureRecipients {
    pub workspace: WorkspaceId,
    pub runner: PaneId,
    pub agents: Vec<PaneId>,
    pub selected: usize,
}

impl FailureRecipients {
    pub fn advance(&mut self, steps: isize) {
        self.selected = self
            .selected
            .saturating_add_signed(steps)
            .min(self.agents.len() - 1);
    }
}

/// Top-level UI application state shared between input handling, daemon bridge events, and rendering.
#[derive(Debug)]
pub struct AppState {
    pub connection: ConnectionState,
    pub workspaces: Vec<WorkspaceInfo>,
    pub active_workspace_id: Option<WorkspaceId>,
    pub panes: HashMap<PaneId, PaneViewState>,
    pub focused_role: PaneRole,
    workspace_focus: HashMap<WorkspaceId, PaneRole>,
    selected_panes: HashMap<(WorkspaceId, PaneRole), PaneId>,
    pub sidebar_override: Option<bool>,
    pub sidebar_first: usize,
    pub zoomed: bool,
    pub remote_host: Option<String>,
    pub failure_recipients: Option<FailureRecipients>,
    pub workspace_form: crate::workspace_form::WorkspaceForm,
    pub settings: crate::settings::SettingsDialog,
    pub recap: crate::recap::Recap,
    pub session_names: HashMap<PaneId, String>,
    pub action_palette: ActionPaletteState,
    pub picker: PickerState,
    pub worktree_modal: WorktreeModalState,
    pub close_modal: CloseWorkspaceModalState,
    pub search: Option<SearchState>,
    pub spawning_workspaces: HashSet<WorkspaceId>,
    pub toast: Option<(String, bool, Instant)>,
    pub last_sent_sizes: HashMap<PaneId, (u16, u16)>,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    pub fn can_create_worktree(&self) -> bool {
        self.connection == ConnectionState::Connected
            && !self.worktree_modal.branch_input.trim().is_empty()
    }

    pub fn new() -> Self {
        Self {
            connection: ConnectionState::Connecting,
            workspaces: Vec::new(),
            active_workspace_id: None,
            panes: HashMap::new(),
            focused_role: PaneRole::Agent,
            sidebar_override: None,
            sidebar_first: 0,
            workspace_focus: HashMap::new(),
            selected_panes: HashMap::new(),
            zoomed: true,
            remote_host: None,
            failure_recipients: None,
            workspace_form: Default::default(),
            settings: Default::default(),
            recap: Default::default(),
            session_names: HashMap::new(),
            action_palette: ActionPaletteState::default(),
            picker: PickerState::default(),
            worktree_modal: WorktreeModalState::default(),
            close_modal: CloseWorkspaceModalState::default(),
            search: None,
            spawning_workspaces: HashSet::new(),
            toast: None,
            last_sent_sizes: HashMap::new(),
        }
    }

    /// Toggles the scrollback search overlay for `pane_id`, closing modals if opening.
    pub fn toggle_search(&mut self, pane_id: PaneId) {
        if self
            .search
            .as_ref()
            .is_some_and(|search| search.pane_id == pane_id)
        {
            self.search = None;
        } else {
            self.action_palette.close();
            self.picker.close();
            self.worktree_modal.close();
            self.close_modal.close();
            self.search = Some(SearchState::new(pane_id));
        }
    }

    pub fn close_search(&mut self) {
        self.search = None;
    }

    /// Applies `SearchResults` to `self.search` only when `pane_id` and `generation` match
    /// an active non-empty query.
    pub fn apply_search_results(
        &mut self,
        pane_id: PaneId,
        generation: u64,
        total_scrollback: usize,
        matches: Vec<SearchMatch>,
    ) -> bool {
        let Some(ref mut search) = self.search else {
            return false;
        };
        if search.pane_id != pane_id {
            return false;
        }
        search.apply_results(generation, total_scrollback, matches)
    }

    /// Returns an active workspace whose default panes need starting or resuming.
    pub fn active_dormant_workspace(&self) -> Option<&WorkspaceInfo> {
        self.active_workspace()
            .filter(|ws| ws.needs_default_panes() && !self.spawning_workspaces.contains(&ws.id))
    }

    /// Marks `workspace_id` as having an in-flight `EnsureWorkspacePanes` request.
    /// Returns `true` if it was not already marked.
    pub fn mark_workspace_spawning(&mut self, workspace_id: WorkspaceId) -> bool {
        self.spawning_workspaces.insert(workspace_id)
    }

    pub fn finish_workspace_spawning(&mut self, workspace_id: WorkspaceId, complete: bool) {
        if complete && let Some(ws) = self.workspaces.iter_mut().find(|ws| ws.id == workspace_id) {
            ws.default_panes_pending = Some(false);
        }
        self.spawning_workspaces.remove(&workspace_id);
    }

    /// Opens the Agent preset picker modal while ensuring other modals are closed.
    pub fn open_picker_with(&mut self, preset: AgentPreset) {
        self.worktree_modal.close();
        self.close_modal.close();
        self.action_palette.close();
        self.picker.open_with(preset);
    }

    /// Opens the Git worktree creator modal while ensuring other modals are closed.
    pub fn open_worktree_modal(&mut self) {
        self.action_palette.close();
        self.picker.close();
        self.close_modal.close();
        self.worktree_modal.open();
    }

    /// Toggles the Git worktree creator modal, closing other modals if opening.
    pub fn toggle_worktree_modal(&mut self) {
        if self.worktree_modal.open {
            self.worktree_modal.close();
        } else {
            self.open_worktree_modal();
        }
    }

    /// Opens the Close Workspace confirmation modal for `workspace_id`, closing other modals/search.
    pub fn open_close_modal_for_workspace(&mut self, workspace_id: WorkspaceId) -> bool {
        let Some(ws) = self
            .workspaces
            .iter()
            .find(|ws| ws.id == workspace_id)
            .cloned()
        else {
            return false;
        };
        self.action_palette.close();
        self.picker.close();
        self.worktree_modal.close();
        self.close_search();
        self.close_modal.open_for_workspace(&ws);
        true
    }

    /// Cycles the active workspace forward (`delta > 0`) or backward (`delta < 0`), wrapping cleanly.
    pub fn cycle_workspace(&mut self, delta: isize) -> Option<WorkspaceId> {
        let len = self.workspaces.len();
        if len == 0 {
            return None;
        }
        let next_idx = match self
            .active_workspace_id
            .and_then(|id| self.workspaces.iter().position(|ws| ws.id == id))
        {
            Some(idx) => {
                let offset = delta.rem_euclid(len as isize) as usize;
                (idx + offset) % len
            }
            None => {
                if delta >= 0 {
                    0
                } else {
                    len - 1
                }
            }
        };
        let next_id = self.workspaces[next_idx].id;
        self.select_workspace(next_id);
        Some(next_id)
    }

    pub fn select_workspace(&mut self, id: WorkspaceId) {
        if self.active_workspace_id == Some(id) {
            return;
        }
        if let Some(previous) = self.active_workspace_id {
            self.workspace_focus.insert(previous, self.focused_role);
        }
        self.focused_role = self
            .workspace_focus
            .get(&id)
            .copied()
            .unwrap_or(PaneRole::Agent);
        self.active_workspace_id = Some(id);
        self.focus_available_session();
        self.failure_recipients = None;
        self.recap.select(Some(id));
        self.settings.open = false;
        self.action_palette.close();
        self.close_search();
    }

    /// Repeating a role shortcut visits each live terminal assigned to that role.
    pub fn focus_role_shortcut(&mut self, role: PaneRole) {
        if self.focused_role == role
            && let Some(ws) = self.active_workspace()
            && let Some(current) = self.pane_id_for_role(role)
        {
            let mut candidates = ws.panes.iter().filter(|pane| pane.role == role);
            let first = candidates.clone().next().map(|pane| pane.id);
            candidates.find(|pane| pane.id == current);
            if let Some(next) = candidates.next().map(|pane| pane.id).or(first) {
                self.selected_panes.insert((ws.id, role), next);
            }
        }
        self.focused_role = role;
        self.close_search();
    }

    /// Returns the `Agent` pane's current [`AgentState`] for `ws`, if any.
    pub fn workspace_agent_state(&self, ws: &WorkspaceInfo) -> Option<AgentState> {
        ws.panes
            .iter()
            .rev()
            .find(|pane| pane.role == PaneRole::Agent)
            .and_then(|pane| {
                pane.agent_state.or_else(|| {
                    self.panes
                        .get(&pane.id)
                        .and_then(|view| view.summary.agent_state)
                })
            })
    }

    /// Updates `agent_state` for `pane_id` in both `self.workspaces` and `self.panes`.
    pub fn apply_agent_state(
        &mut self,
        workspace_id: WorkspaceId,
        pane_id: PaneId,
        state: AgentState,
    ) {
        if let Some(pane) = self.panes.get_mut(&pane_id) {
            pane.summary.agent_state = Some(state);
        }
        if let Some(ws) = self.workspaces.iter_mut().find(|w| w.id == workspace_id)
            && let Some(summary) = ws.panes.iter_mut().find(|p| p.id == pane_id)
        {
            summary.agent_state = Some(state);
        } else if let Some(summary) = self.workspace_pane_summary_mut(pane_id) {
            summary.agent_state = Some(state);
        }
    }

    /// Updates runtime ports, containers, and git metadata for `workspace_id`.
    pub fn apply_workspace_runtime(
        &mut self,
        workspace_id: WorkspaceId,
        bound_ports: Vec<PortMapping>,
        containers: Vec<ContainerHealth>,
        git_status: Option<GitWorkspaceStatus>,
        git_branch: Option<String>,
    ) {
        if let Some(ws) = self.workspaces.iter_mut().find(|w| w.id == workspace_id) {
            ws.bound_ports = bound_ports;
            ws.containers = containers;
            ws.git_status = git_status;
            ws.git_branch = git_branch;
        }
    }

    pub fn active_workspace(&self) -> Option<&WorkspaceInfo> {
        let id = self.active_workspace_id?;
        self.workspaces.iter().find(|ws| ws.id == id)
    }

    pub fn active_actions(&self) -> &[daedalus_proto::CustomAction] {
        self.active_workspace()
            .and_then(|ws| ws.manifest.as_ref())
            .map(|m| m.actions.as_slice())
            .unwrap_or_default()
    }

    pub fn open_action_palette(&mut self) {
        self.picker.close();
        self.worktree_modal.close();
        self.close_modal.close();
        self.close_search();
        self.action_palette.open();
    }

    pub fn active_workspace_mut(&mut self) -> Option<&mut WorkspaceInfo> {
        let id = self.active_workspace_id?;
        self.workspaces.iter_mut().find(|ws| ws.id == id)
    }

    pub fn pane_id_for_role(&self, role: PaneRole) -> Option<PaneId> {
        let ws = self.active_workspace()?;
        if let Some(id) = self.selected_panes.get(&(ws.id, role))
            && ws
                .panes
                .iter()
                .any(|pane| pane.id == *id && pane.role == role)
        {
            return Some(*id);
        }
        ws.panes
            .iter()
            .rev()
            .find(|pane| pane.role == role)
            .map(|pane| pane.id)
    }

    pub fn pane_for_role(&self, role: PaneRole) -> Option<&PaneViewState> {
        let id = self.pane_id_for_role(role)?;
        self.panes.get(&id)
    }

    pub fn pane_for_role_mut(&mut self, role: PaneRole) -> Option<&mut PaneViewState> {
        let id = self.pane_id_for_role(role)?;
        self.panes.get_mut(&id)
    }

    pub fn remove_session(&mut self, id: PaneId) {
        let focused = self.focused_pane_id() == Some(id);
        self.session_names.remove(&id);
        self.panes.remove(&id);
        self.last_sent_sizes.remove(&id);
        self.selected_panes.retain(|_, pane| *pane != id);
        if self
            .search
            .as_ref()
            .is_some_and(|search| search.pane_id == id)
        {
            self.close_search();
        }
        for ws in &mut self.workspaces {
            ws.panes.retain(|p| p.id != id);
        }
        if focused {
            self.focus_available_session();
        }
    }

    fn focus_available_session(&mut self) {
        if self.focused_pane_id().is_none()
            && let Some(pane) = self.active_workspace().and_then(|ws| ws.panes.first())
        {
            self.focused_role = pane.role;
        }
    }

    pub fn select_session(&mut self, workspace: WorkspaceId, role: PaneRole, pane: PaneId) {
        self.selected_panes.insert((workspace, role), pane);
        if self.active_workspace_id == Some(workspace) {
            self.focused_role = role;
            self.close_search();
        } else {
            self.workspace_focus.insert(workspace, role);
        }
    }

    pub fn focused_pane_id(&self) -> Option<PaneId> {
        self.pane_id_for_role(self.focused_role)
    }

    pub fn failure_recipient_choices(&self) -> Result<FailureRecipients, &'static str> {
        let ws = self
            .active_workspace()
            .ok_or("Workspace is not available")?;
        let runner = self
            .pane_id_for_role(PaneRole::Runner)
            .ok_or("Runner pane is not available")?;
        if !self.runner_has_failure() {
            return Err("No failure trace captured in Runner pane yet");
        }
        let agents: Vec<_> = ws
            .panes
            .iter()
            .filter(|pane| {
                pane.role == PaneRole::Agent && !matches!(pane.status, PaneStatus::Exited { .. })
            })
            .map(|pane| pane.id)
            .collect();
        if agents.is_empty() {
            return Err("No live agent is available");
        }
        let current = self.pane_id_for_role(PaneRole::Agent);
        let selected = agents
            .iter()
            .position(|id| Some(*id) == current)
            .unwrap_or(0);
        Ok(FailureRecipients {
            workspace: ws.id,
            runner,
            agents,
            selected,
        })
    }

    pub fn validate_failure_recipient(
        &self,
        choices: &FailureRecipients,
    ) -> Result<PaneId, &'static str> {
        let ws = self
            .active_workspace()
            .filter(|ws| ws.id == choices.workspace)
            .ok_or("The source workspace has changed")?;
        if !ws.panes.iter().any(|pane| {
            pane.role == PaneRole::Runner && pane.id == choices.runner && pane.has_failure_trace
        }) {
            return Err("The original Runner failure is no longer available");
        }
        choices
            .agents
            .get(choices.selected)
            .copied()
            .filter(|id| {
                ws.panes.iter().any(|pane| {
                    pane.id == *id
                        && pane.role == PaneRole::Agent
                        && !matches!(pane.status, PaneStatus::Exited { .. })
                })
            })
            .ok_or("The selected agent is no longer available")
    }

    pub fn failure_recipient_label(&self, agent: PaneId) -> String {
        let name = self
            .session_names
            .get(&agent)
            .map_or("Agent", String::as_str);
        format!("{name} · {}", agent.short())
    }

    pub fn runner_has_failure(&self) -> bool {
        self.pane_for_role(PaneRole::Runner)
            .is_some_and(|pane| pane.summary.has_failure_trace)
    }

    pub fn show_toast(&mut self, message: impl Into<String>, is_error: bool) {
        self.toast = Some((message.into(), is_error, Instant::now()));
    }

    /// Replaces the workspace list while preserving cached `PaneViewState` for still-existing panes.
    pub fn set_workspaces(&mut self, workspaces: Vec<WorkspaceInfo>) {
        let mut live_panes = HashSet::new();
        for ws in &workspaces {
            for summary in &ws.panes {
                live_panes.insert(summary.id);
                self.panes
                    .entry(summary.id)
                    .and_modify(|view| view.summary = summary.clone())
                    .or_insert_with(|| PaneViewState::from_summary(summary.clone()));
            }
        }
        self.panes.retain(|id, _| live_panes.contains(id));
        self.last_sent_sizes.retain(|id, _| live_panes.contains(id));
        if self
            .search
            .as_ref()
            .is_some_and(|search| !live_panes.contains(&search.pane_id))
        {
            self.search = None;
        }

        self.workspace_focus
            .retain(|id, _| workspaces.iter().any(|ws| ws.id == *id));
        self.selected_panes.retain(|_, id| live_panes.contains(id));
        self.workspaces = workspaces;
        self.spawning_workspaces
            .retain(|ws_id| self.workspaces.iter().any(|ws| ws.id == *ws_id));
        if self.close_modal.open
            && self
                .close_modal
                .workspace_id
                .is_none_or(|id| !self.workspaces.iter().any(|ws| ws.id == id))
        {
            self.close_modal.close();
        }
        let active_still_exists = self
            .active_workspace_id
            .is_some_and(|id| self.workspaces.iter().any(|ws| ws.id == id));
        if !active_still_exists {
            self.failure_recipients = None;
            self.action_palette.close();
            self.active_workspace_id = None;
            self.focused_role = PaneRole::Agent;
            if let Some(ws) = self.workspaces.first() {
                self.select_workspace(ws.id);
            }
        }
    }

    /// Inserts or updates a single workspace and synchronizes its pane summaries.
    pub fn upsert_workspace(&mut self, ws: WorkspaceInfo) {
        for summary in &ws.panes {
            self.panes
                .entry(summary.id)
                .and_modify(|view| view.summary = summary.clone())
                .or_insert_with(|| PaneViewState::from_summary(summary.clone()));
        }
        let ws_id = ws.id;
        if let Some(existing) = self.workspaces.iter_mut().find(|item| item.id == ws_id) {
            *existing = ws;
        } else {
            self.workspaces.push(ws);
        }
        if self.active_workspace_id.is_none() {
            self.active_workspace_id = Some(ws_id);
        }
    }

    /// Registers a newly spawned pane into both its workspace summary list and `self.panes`.
    pub fn register_pane(&mut self, summary: PaneSummary, agent_preset: Option<AgentPreset>) {
        if let Some(ws) = self
            .workspaces
            .iter_mut()
            .find(|ws| ws.id == summary.workspace_id)
        {
            if let Some(existing) = ws.panes.iter_mut().find(|p| p.id == summary.id) {
                *existing = summary.clone();
            } else {
                ws.panes.push(summary.clone());
            }
        }
        if agent_preset.is_some() {
            self.selected_panes
                .insert((summary.workspace_id, summary.role), summary.id);
        }
        let entry = self
            .panes
            .entry(summary.id)
            .and_modify(|view| view.summary = summary.clone())
            .or_insert_with(|| PaneViewState::from_summary(summary));
        if let Some(preset) = agent_preset {
            entry.agent_preset = Some(preset);
        }
    }

    fn invalidate_history_search(&mut self, pane_id: PaneId) {
        if let Some(search) = &mut self.search
            && search.pane_id == pane_id
        {
            search.invalidate_results();
            search.total_scrollback = self.panes[&pane_id].scrollback_len;
        }
    }

    /// Returns the pane to resync if newer updates arrived before the snapshot.
    pub fn apply_grid_snapshot(&mut self, snapshot: GridSnapshot) -> Option<PaneId> {
        let pane_id = snapshot.pane_id;
        let pane = self.panes.get_mut(&pane_id)?;
        if snapshot.seq < pane.last_seq || snapshot.history_generation < pane.history_generation {
            return Some(pane_id);
        }
        if pane.apply_snapshot(snapshot) {
            self.invalidate_history_search(pane_id);
        }
        None
    }

    /// Applies a `ServerEvent` from `daedalusd`.
    /// Returns `Some(pane_id)` when `ServerEvent::ResyncRequired` requires a fresh `GetSnapshot`.
    pub fn apply_server_event(&mut self, event: ServerEvent) -> Option<PaneId> {
        match event {
            ServerEvent::GridDelta(delta) => {
                let pane_id = delta.pane_id;
                if let Some(pane) = self.panes.get_mut(&pane_id)
                    && pane.apply_delta(delta)
                {
                    self.invalidate_history_search(pane_id);
                }
                None
            }
            ServerEvent::PaneStatusChanged {
                pane_id,
                status,
                cwd,
            } => {
                if let Some(pane) = self.panes.get_mut(&pane_id) {
                    pane.summary.status = status;
                    if cwd.is_some() {
                        pane.summary.cwd = cwd.clone();
                    }
                }
                if let Some(summary) = self.workspace_pane_summary_mut(pane_id) {
                    summary.status = status;
                    if cwd.is_some() {
                        summary.cwd = cwd;
                    }
                }
                None
            }
            ServerEvent::CommandFinished {
                pane_id,
                command: _,
                exit_code,
                has_failure_trace,
            } => {
                if let Some(pane) = self.panes.get_mut(&pane_id) {
                    pane.summary.last_exit_code = Some(exit_code);
                    pane.summary.has_failure_trace = has_failure_trace;
                    if !has_failure_trace {
                        pane.last_failure_trace = None;
                    }
                }
                if let Some(summary) = self.workspace_pane_summary_mut(pane_id) {
                    summary.last_exit_code = Some(exit_code);
                    summary.has_failure_trace = has_failure_trace;
                }
                None
            }
            ServerEvent::PaneExited { pane_id, exit_code } => {
                let status = PaneStatus::Exited { code: exit_code };
                if let Some(pane) = self.panes.get_mut(&pane_id) {
                    pane.summary.status = status;
                    if exit_code.is_some() {
                        pane.summary.last_exit_code = exit_code;
                    }
                }
                if let Some(summary) = self.workspace_pane_summary_mut(pane_id) {
                    summary.status = status;
                    if exit_code.is_some() {
                        summary.last_exit_code = exit_code;
                    }
                }
                None
            }
            ServerEvent::ResyncRequired { pane_id } => Some(pane_id),
            ServerEvent::WorkspaceRuntimeUpdated {
                workspace_id,
                bound_ports,
                containers,
                git_status,
                git_branch,
            } => {
                self.apply_workspace_runtime(
                    workspace_id,
                    bound_ports,
                    containers,
                    git_status,
                    git_branch,
                );
                None
            }
            ServerEvent::AgentStateChanged {
                workspace_id,
                pane_id,
                state,
            } => {
                self.apply_agent_state(workspace_id, pane_id, state);
                None
            }
            ServerEvent::ExternalStatusUpdated {
                workspace_id,
                status,
            } => {
                if let Some(ws) = self.workspaces.iter_mut().find(|w| w.id == workspace_id) {
                    upsert_external_status(&mut ws.external_statuses, status);
                }
                None
            }
        }
    }

    /// Moves focus directionally across the fixed 3-pane semantic layout:
    /// `Agent` (left) <-> `Service` (right-top) / `Runner` (right-bottom).
    pub fn focus_direction(&mut self, dir: FocusDirection, stacked: bool) -> PaneRole {
        let next = match (self.focused_role, dir) {
            (PaneRole::Agent, FocusDirection::Down) if stacked => PaneRole::Service,
            (PaneRole::Service, FocusDirection::Up) if stacked => PaneRole::Agent,
            (PaneRole::Agent, FocusDirection::Right) if !stacked => PaneRole::Service,
            (PaneRole::Service, FocusDirection::Left)
            | (PaneRole::Runner, FocusDirection::Left)
                if !stacked =>
            {
                PaneRole::Agent
            }
            (PaneRole::Service, FocusDirection::Down) => PaneRole::Runner,
            (PaneRole::Runner, FocusDirection::Up) => PaneRole::Service,
            (role, _) => role,
        };
        self.focused_role = next;
        next
    }

    fn workspace_pane_summary_mut(&mut self, pane_id: PaneId) -> Option<&mut PaneSummary> {
        self.workspaces
            .iter_mut()
            .flat_map(|ws| ws.panes.iter_mut())
            .find(|pane| pane.id == pane_id)
    }
}

const MAX_EXTERNAL_STATUSES: usize = 16;

fn upsert_external_status(statuses: &mut Vec<ExternalStatus>, status: ExternalStatus) {
    if let Some(pos) = statuses
        .iter()
        .position(|existing| existing.source == status.source)
    {
        statuses.remove(pos);
    }
    if status.label.trim().is_empty() {
        return;
    }
    if statuses.len() >= MAX_EXTERNAL_STATUSES {
        statuses.remove(0);
    }
    statuses.push(status);
}

#[cfg(test)]
mod tests {
    use super::*;
    use daedalus_proto::{Cell, CellFlags, Color};
    use std::path::PathBuf;

    fn make_row(row_index: u16, abs_row: u64, text: &str) -> GridRow {
        GridRow {
            row_index,
            abs_row,
            cells: text
                .chars()
                .map(|ch| Cell {
                    text: ch.to_string(),
                    fg: Color::Default,
                    bg: Color::Default,
                    flags: CellFlags::empty(),
                    width: 1,
                })
                .collect(),
        }
    }

    fn row_text(row: &GridRow) -> String {
        row.cells.iter().map(|c| c.text.as_str()).collect()
    }

    fn make_pane_summary(ws_id: WorkspaceId, role: PaneRole) -> PaneSummary {
        PaneSummary {
            id: PaneId::new(),
            workspace_id: ws_id,
            role,
            status: PaneStatus::Idle,
            cols: 80,
            rows: 24,
            cwd: Some(PathBuf::from("/tmp/repo")),
            last_exit_code: None,
            has_failure_trace: false,
            agent_state: None,
        }
    }

    fn make_workspace_with_three_panes() -> (WorkspaceInfo, PaneId, PaneId, PaneId) {
        let ws_id = WorkspaceId::new();
        let agent = make_pane_summary(ws_id, PaneRole::Agent);
        let service = make_pane_summary(ws_id, PaneRole::Service);
        let runner = make_pane_summary(ws_id, PaneRole::Runner);
        let ids = (agent.id, service.id, runner.id);
        let ws = WorkspaceInfo {
            project: Default::default(),
            default_panes_pending: None,
            id: ws_id,
            name: "demo".to_string(),
            root_dir: PathBuf::from("/tmp/repo"),
            slot_index: 0,
            injected_env: HashMap::new(),
            panes: vec![agent, service, runner],
            port_offset: 0,
            compose_project_name: None,
            git_branch: None,
            git_status: None,
            bound_ports: Vec::new(),
            containers: Vec::new(),
            external_statuses: Vec::new(),
            manifest: None,
        };
        (ws, ids.0, ids.1, ids.2)
    }

    #[test]
    fn closing_sessions_preserves_siblings_and_handles_the_last_pane() {
        let (workspace, agent, service, runner) = make_workspace_with_three_panes();
        let id = workspace.id;
        let mut state = AppState::new();
        state.set_workspaces(vec![workspace]);
        state.select_session(id, PaneRole::Runner, runner);
        state.toggle_search(runner);
        state.remove_session(runner);
        assert!(state.search.is_none());
        assert_eq!(state.focused_pane_id(), Some(agent));
        state.remove_session(agent);
        assert_eq!(state.focused_pane_id(), Some(service));
        state.remove_session(service);
        assert_eq!(state.focused_pane_id(), None);
        assert_eq!(state.active_workspace_id, Some(id));
    }

    #[test]
    fn forwarding_checks_source_and_selected_agent_again_at_confirmation() {
        let (mut ws, first, _, runner) = make_workspace_with_three_panes();
        ws.panes
            .iter_mut()
            .find(|p| p.id == runner)
            .unwrap()
            .has_failure_trace = true;
        let mut state = AppState::new();
        state.set_workspaces(vec![ws.clone()]);
        let single = state.failure_recipient_choices().unwrap();
        assert_eq!(single.agents, [first]);
        let other = make_pane_summary(ws.id, PaneRole::Agent);
        ws.panes.push(other.clone());
        state.set_workspaces(vec![ws.clone()]);
        state.select_session(ws.id, PaneRole::Agent, first);
        let mut choices = state.failure_recipient_choices().unwrap();
        assert_eq!(state.validate_failure_recipient(&choices), Ok(first));
        choices.advance(1);
        assert_eq!(state.validate_failure_recipient(&choices), Ok(other.id));
        ws.panes.last_mut().unwrap().status = PaneStatus::Exited { code: Some(0) };
        state.set_workspaces(vec![ws.clone()]);
        assert!(state.validate_failure_recipient(&choices).is_err());
        choices.advance(-1);
        ws.panes.retain(|p| p.id != runner);
        state.set_workspaces(vec![ws]);
        assert!(state.validate_failure_recipient(&choices).is_err());
        state.active_workspace_id = None;
        assert!(state.validate_failure_recipient(&choices).is_err());
    }

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
    fn search_reports_no_matches_only_after_the_current_reply() {
        let mut search = SearchState::new(PaneId::new());
        assert_eq!(search.result_label(), None);
        search.push_text("old");
        let previous = search.generation;
        search.push_text("er");
        assert!(!search.apply_results(previous, 0, vec![]));
        assert_eq!(search.result_label(), None);
        assert!(search.apply_results(search.generation, 0, vec![]));
        assert_eq!(search.result_label().as_deref(), Some("No matches"));
        search.pop_char();
        assert_eq!(search.result_label(), None);
        search.set_results(
            0,
            vec![SearchMatch {
                abs_row: 0,
                start_col: 1,
                end_col: 2,
            }],
        );
        assert_eq!(search.result_label().as_deref(), Some("(1/1)"));
    }

    #[test]
    fn snapshot_and_delta_patching_tracks_dirty_rows() {
        let mut state = AppState::new();
        let (ws, agent_id, _, _) = make_workspace_with_three_panes();
        state.set_workspaces(vec![ws]);

        let snapshot = GridSnapshot {
            pane_id: agent_id,
            seq: 1,
            cols: 10,
            rows: 3,
            cursor: CursorState {
                col: 2,
                row: 1,
                visible: true,
            },
            rows_data: vec![
                make_row(0, 10, "line zero"),
                make_row(1, 11, "line one"),
                make_row(2, 12, "line two"),
            ],
            history_generation: 0,
            scrollback_len: 10,
        };

        let pane = state.panes.get_mut(&agent_id).unwrap();
        pane.apply_snapshot(snapshot);
        assert_eq!(pane.dirty_row_indices, vec![0, 1, 2]);
        assert_eq!(pane.scrollback_len, 10);
        assert_eq!(pane.take_dirty_rows(), vec![0, 1, 2]);
        assert!(pane.dirty_row_indices.is_empty());

        let delta = GridDelta {
            pane_id: agent_id,
            seq: 2,
            cols: 10,
            rows: 3,
            cursor: CursorState {
                col: 5,
                row: 1,
                visible: true,
            },
            history_generation: 0,
            scrollback_len: None,
            dirty_rows: vec![make_row(1, 11, "PATCHED!")],
        };
        assert_eq!(
            state.apply_server_event(ServerEvent::GridDelta(delta)),
            None
        );

        let pane = state.panes.get(&agent_id).unwrap();
        assert_eq!(pane.dirty_row_indices, vec![1]);
        assert_eq!(row_text(&pane.visible_rows()[0]), "line zero");
        assert_eq!(row_text(&pane.visible_rows()[1]), "PATCHED!");
        assert_eq!(row_text(&pane.visible_rows()[2]), "line two");
        assert_eq!(pane.cursor.col, 5);
    }

    #[test]
    fn apply_delta_ignores_stale_or_duplicate_seq_after_snapshot() {
        let ws_id = WorkspaceId::new();
        let summary = make_pane_summary(ws_id, PaneRole::Agent);
        let mut pane = PaneViewState::from_summary(summary);

        // Initial delta when last_seq == 0 should still apply
        pane.apply_delta(GridDelta {
            pane_id: pane.summary.id,
            seq: 1,
            cols: 10,
            rows: 2,
            cursor: CursorState::default(),
            history_generation: 0,
            scrollback_len: None,
            dirty_rows: vec![make_row(0, 0, "initial")],
        });
        assert_eq!(pane.last_seq, 1);
        assert_eq!(row_text(&pane.visible_rows()[0]), "initial");

        // Newer snapshot advances last_seq to 5
        pane.apply_snapshot(GridSnapshot {
            pane_id: pane.summary.id,
            seq: 5,
            cols: 10,
            rows: 2,
            cursor: CursorState {
                col: 1,
                row: 0,
                visible: true,
            },
            rows_data: vec![make_row(0, 10, "fresh-row"), make_row(1, 11, "row-one")],
            history_generation: 0,
            scrollback_len: 0,
        });
        let _ = pane.take_dirty_rows();

        // Older (seq < last_seq) and duplicate (seq == last_seq) deltas must be ignored
        for stale_seq in [4, 5] {
            pane.apply_delta(GridDelta {
                pane_id: pane.summary.id,
                seq: stale_seq,
                cols: 10,
                rows: 2,
                cursor: CursorState {
                    col: 9,
                    row: 1,
                    visible: true,
                },
                history_generation: 0,
                scrollback_len: None,
                dirty_rows: vec![make_row(0, 10, "STALE")],
            });
            assert_eq!(pane.last_seq, 5);
            assert_eq!(row_text(&pane.visible_rows()[0]), "fresh-row");
            assert_eq!(pane.cursor.col, 1);
            assert!(pane.dirty_row_indices.is_empty());
        }

        // Newer delta (seq > last_seq) applies normally
        pane.apply_delta(GridDelta {
            pane_id: pane.summary.id,
            seq: 6,
            cols: 10,
            rows: 2,
            cursor: CursorState {
                col: 4,
                row: 0,
                visible: true,
            },
            history_generation: 0,
            scrollback_len: None,
            dirty_rows: vec![make_row(0, 10, "newer-row")],
        });
        assert_eq!(pane.last_seq, 6);
        assert_eq!(row_text(&pane.visible_rows()[0]), "newer-row");
        assert_eq!(pane.cursor.col, 4);
        assert_eq!(pane.dirty_row_indices, vec![0]);
    }

    #[test]
    fn resync_required_signals_pane_id() {
        let mut state = AppState::new();
        let pane_id = PaneId::new();
        let resync = state.apply_server_event(ServerEvent::ResyncRequired { pane_id });
        assert_eq!(resync, Some(pane_id));
    }

    #[test]
    fn scrollback_offset_clamps_and_switches_visible_rows() {
        let ws_id = WorkspaceId::new();
        let summary = make_pane_summary(ws_id, PaneRole::Runner);
        let mut pane = PaneViewState::from_summary(summary);
        pane.apply_snapshot(GridSnapshot {
            pane_id: pane.summary.id,
            seq: 1,
            cols: 20,
            rows: 2,
            cursor: CursorState::default(),
            rows_data: vec![make_row(0, 5, "live-0"), make_row(1, 6, "live-1")],
            history_generation: 0,
            scrollback_len: 5,
        });

        // Clamps above scrollback_len
        assert_eq!(pane.adjust_scroll_offset(100), 5);
        // Clamps below 0
        assert_eq!(pane.adjust_scroll_offset(-100), 0);

        // Scroll up 3 lines and load scrollback rows
        assert_eq!(pane.adjust_scroll_offset(3), 3);
        let generation = pane.begin_scrollback_request().unwrap();
        pane.apply_scrollback(
            generation,
            5,
            vec![make_row(0, 2, "hist-2"), make_row(1, 3, "hist-3")],
        );
        assert_eq!(row_text(&pane.visible_rows()[0]), "hist-2");
        assert_eq!(row_text(&pane.visible_rows()[1]), "hist-3");

        // Reset scroll returns to live viewport
        pane.reset_scroll();
        assert_eq!(pane.scroll_offset, 0);
        assert!(pane.scrolled_rows.is_none());
        assert_eq!(row_text(&pane.visible_rows()[0]), "live-0");
    }

    #[test]
    fn stale_scrollback_cannot_replace_a_newer_offset_or_live_viewport() {
        let summary = make_pane_summary(WorkspaceId::new(), PaneRole::Runner);
        let mut pane = PaneViewState::from_summary(summary);
        pane.set_scroll_offset(5);
        let older = pane.begin_scrollback_request().unwrap();
        pane.set_scroll_offset(10);
        let newer = pane.begin_scrollback_request().unwrap();
        pane.apply_scrollback(older, 3, vec![make_row(0, 1, "stale")]);
        assert_eq!(pane.requested_scroll_offset, 10);
        assert_eq!(pane.scroll_offset, 0);
        assert_eq!(pane.scrollback_len, 0);
        assert!(pane.visible_rows().is_empty());
        assert!(pane.begin_scrollback_request().is_none());
        pane.apply_scrollback(newer, 30, vec![make_row(0, 20, "current")]);
        assert_eq!(row_text(&pane.visible_rows()[0]), "current");

        pane.set_scroll_offset(15);
        let pending = pane.begin_scrollback_request().unwrap();
        assert_eq!(pane.scroll_offset, 10);
        assert_eq!(row_text(&pane.visible_rows()[0]), "current");
        pane.reset_scroll();
        pane.apply_scrollback(pending, 40, vec![make_row(0, 25, "late")]);
        assert_eq!(pane.scroll_offset, 0);
        assert_eq!(pane.scrollback_len, 30);
        assert!(pane.scrolled_rows.is_none());
    }

    #[test]
    fn history_reset_discards_displayed_rows_inflight_reads_and_search_results() {
        let mut app = AppState::new();
        let (ws, pane_id, _, _) = make_workspace_with_three_panes();
        app.set_workspaces(vec![ws]);
        app.toggle_search(pane_id);
        let search = app.search.as_mut().unwrap();
        search.push_text("hit");
        let stale_search = search.generation;
        search.set_results(
            12,
            vec![SearchMatch {
                abs_row: 1,
                start_col: 0,
                end_col: 3,
            }],
        );
        let pane = app.panes.get_mut(&pane_id).unwrap();
        pane.viewport_rows = vec![make_row(0, 12, "live")];
        pane.set_scroll_offset(4);
        let initial = pane.begin_scrollback_request().unwrap();
        pane.apply_scrollback(initial, 12, vec![make_row(0, 8, "old")]);
        pane.set_scroll_offset(5);
        let late = pane.begin_scrollback_request().unwrap();
        let reset = GridDelta {
            pane_id,
            seq: 10,
            cols: pane.cols,
            rows: pane.rows,
            cursor: pane.cursor,
            dirty_rows: vec![],
            history_generation: 1,
            scrollback_len: Some(0),
        };
        app.apply_server_event(ServerEvent::GridDelta(reset));
        let pane = app.panes.get_mut(&pane_id).unwrap();
        pane.apply_scrollback(late, 12, vec![make_row(0, 7, "wrong")]);
        assert_eq!(pane.requested_scroll_offset, 0);
        assert_eq!(pane.scroll_offset, 0);
        assert_eq!(pane.scrollback_len, 0);
        assert_eq!(row_text(&pane.visible_rows()[0]), "live");
        assert!(!app.apply_search_results(pane_id, stale_search, 12, vec![]));
        assert!(app.search.as_ref().unwrap().matches.is_empty());
    }

    #[test]
    fn resync_snapshot_invalidates_erased_history_even_if_new_lines_already_accumulated() {
        let mut app = AppState::new();
        let (ws, id, _, _) = make_workspace_with_three_panes();
        app.set_workspaces(vec![ws]);
        app.toggle_search(id);
        let search = app.search.as_mut().unwrap();
        search.push_text("old");
        let generation = search.generation;
        let pane = app.panes.get_mut(&id).unwrap();
        pane.set_scroll_offset(2);
        let pending = pane.begin_scrollback_request().unwrap();
        let reset = GridSnapshot {
            pane_id: id,
            seq: 4,
            cols: pane.cols,
            rows: pane.rows,
            cursor: pane.cursor,
            rows_data: vec![make_row(0, 50, "live")],
            scrollback_len: 5,
            history_generation: 1,
        };
        let mut stale = reset.clone();
        stale.history_generation = 0;
        stale.scrollback_len = 20;
        assert_eq!(app.apply_grid_snapshot(reset), None);
        assert_eq!(app.apply_grid_snapshot(stale), Some(id));
        assert!(!app.apply_search_results(id, generation, 20, vec![]));
        let pane = app.panes.get_mut(&id).unwrap();
        pane.apply_scrollback(pending, 20, vec![make_row(0, 1, "old")]);
        assert_eq!(pane.scrollback_len, 5);
        assert_eq!(pane.requested_scroll_offset, 0);
        assert_eq!(row_text(&pane.visible_rows()[0]), "live");
    }

    #[test]
    fn resized_scrollback_requires_fresh_rows_even_at_the_same_offset() {
        let summary = make_pane_summary(WorkspaceId::new(), PaneRole::Runner);
        let mut pane = PaneViewState::from_summary(summary);
        pane.set_scroll_offset(5);
        let original = pane.begin_scrollback_request().unwrap();
        pane.apply_delta(GridDelta {
            pane_id: pane.summary.id,
            seq: 1,
            cols: pane.cols,
            rows: pane.rows + 1,
            history_generation: 0,
            scrollback_len: None,
            dirty_rows: Vec::new(),
            cursor: CursorState::default(),
        });
        let resized = pane.begin_scrollback_request().unwrap();
        pane.apply_scrollback(original, 20, vec![make_row(0, 15, "wrong size")]);
        assert!(pane.scrolled_rows.is_none());
        pane.apply_scrollback(resized, 20, vec![make_row(0, 15, "resized")]);
        assert_eq!(row_text(&pane.visible_rows()[0]), "resized");
    }

    #[test]
    fn command_finished_updates_runner_failure_badge_and_workspace_summary() {
        let mut state = AppState::new();
        let (ws, _, _, runner_id) = make_workspace_with_three_panes();
        state.set_workspaces(vec![ws]);
        assert!(!state.runner_has_failure());

        state.apply_server_event(ServerEvent::CommandFinished {
            pane_id: runner_id,
            command: Some("cargo test".to_string()),
            exit_code: 101,
            has_failure_trace: true,
        });

        assert!(state.runner_has_failure());
        let runner = state.panes.get(&runner_id).unwrap();
        assert_eq!(runner.summary.last_exit_code, Some(101));
        assert!(runner.summary.has_failure_trace);

        let ws_runner = state
            .active_workspace()
            .unwrap()
            .panes
            .iter()
            .find(|p| p.id == runner_id)
            .unwrap();
        assert_eq!(ws_runner.last_exit_code, Some(101));
        assert!(ws_runner.has_failure_trace);

        // Subsequent passing command clears failure badge
        state.apply_server_event(ServerEvent::CommandFinished {
            pane_id: runner_id,
            command: Some("cargo test".to_string()),
            exit_code: 0,
            has_failure_trace: false,
        });
        assert!(!state.runner_has_failure());
        assert_eq!(
            state.panes.get(&runner_id).unwrap().summary.last_exit_code,
            Some(0)
        );
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

    #[test]
    fn stacked_pane_focus_follows_vertical_order() {
        let mut state = AppState::new();
        assert_eq!(
            state.focus_direction(FocusDirection::Down, true),
            PaneRole::Service
        );
        assert_eq!(
            state.focus_direction(FocusDirection::Down, true),
            PaneRole::Runner
        );
        assert_eq!(
            state.focus_direction(FocusDirection::Up, true),
            PaneRole::Service
        );
        assert_eq!(
            state.focus_direction(FocusDirection::Up, true),
            PaneRole::Agent
        );
    }

    #[test]
    fn directional_focus_navigates_three_pane_layout() {
        let mut state = AppState::new();
        assert_eq!(state.focused_role, PaneRole::Agent);

        assert_eq!(
            state.focus_direction(FocusDirection::Right, false),
            PaneRole::Service
        );
        assert_eq!(
            state.focus_direction(FocusDirection::Down, false),
            PaneRole::Runner
        );
        assert_eq!(
            state.focus_direction(FocusDirection::Up, false),
            PaneRole::Service
        );
        assert_eq!(
            state.focus_direction(FocusDirection::Left, false),
            PaneRole::Agent
        );
        // Moving left from Agent is a no-op
        assert_eq!(
            state.focus_direction(FocusDirection::Left, false),
            PaneRole::Agent
        );
    }

    #[test]
    fn workspace_runtime_updated_replaces_bound_ports_and_containers() {
        use daedalus_proto::{ContainerHealth, PortMapping, PortSource};

        let mut state = AppState::new();
        let (ws, _, _, _) = make_workspace_with_three_panes();
        let ws_id = ws.id;
        state.set_workspaces(vec![ws]);

        let ports = vec![
            PortMapping {
                host_port: 3100,
                container_port: Some(3000),
                service: Some("web".to_string()),
                source: PortSource::Docker,
            },
            PortMapping {
                host_port: 5532,
                container_port: None,
                service: None,
                source: PortSource::LocalProcess,
            },
        ];
        let containers = vec![ContainerHealth {
            name: "demo-wt1-web-1".to_string(),
            service: Some("web".to_string()),
            state: "running".to_string(),
            health: Some("healthy".to_string()),
            ports: vec![3100],
        }];

        assert_eq!(
            state.apply_server_event(ServerEvent::WorkspaceRuntimeUpdated {
                workspace_id: ws_id,
                bound_ports: ports.clone(),
                containers: containers.clone(),
                git_status: None,
                git_branch: None,
            }),
            None
        );

        let updated = state.active_workspace().unwrap();
        assert_eq!(updated.bound_ports, ports);
        assert_eq!(updated.containers, containers);

        // Unknown workspace ID is safely ignored
        assert_eq!(
            state.apply_server_event(ServerEvent::WorkspaceRuntimeUpdated {
                workspace_id: WorkspaceId::new(),
                bound_ports: Vec::new(),
                containers: Vec::new(),
                git_status: None,
                git_branch: None,
            }),
            None
        );
        assert_eq!(state.active_workspace().unwrap().bound_ports.len(), 2);
    }

    #[test]
    fn external_status_updated_upserts_by_source_and_caps_at_limit() {
        use daedalus_proto::StatusLevel;

        let mut state = AppState::new();
        let (ws, _, _, _) = make_workspace_with_three_panes();
        let ws_id = ws.id;
        state.set_workspaces(vec![ws]);

        let mk_status = |source: &str, level: StatusLevel, label: &str| ExternalStatus {
            source: source.to_string(),
            level,
            label: label.to_string(),
            detail: None,
            updated_at_ms: 1,
        };

        state.apply_server_event(ServerEvent::ExternalStatusUpdated {
            workspace_id: ws_id,
            status: mk_status("phrourion", StatusLevel::Warn, "3 dirty"),
        });
        state.apply_server_event(ServerEvent::ExternalStatusUpdated {
            workspace_id: ws_id,
            status: mk_status("cassandra", StatusLevel::Error, "retry"),
        });
        assert_eq!(state.active_workspace().unwrap().external_statuses.len(), 2);

        // Upsert existing source updates it without duplicating
        state.apply_server_event(ServerEvent::ExternalStatusUpdated {
            workspace_id: ws_id,
            status: mk_status("phrourion", StatusLevel::Ok, "clean"),
        });
        let statuses = &state.active_workspace().unwrap().external_statuses;
        assert_eq!(statuses.len(), 2);
        assert_eq!(statuses[0].source, "cassandra");
        assert_eq!(statuses[1].source, "phrourion");
        assert_eq!(statuses[1].label, "clean");
        assert_eq!(statuses[1].level, StatusLevel::Ok);

        // Exceeding MAX_EXTERNAL_STATUSES evicts oldest
        for i in 0..MAX_EXTERNAL_STATUSES {
            state.apply_server_event(ServerEvent::ExternalStatusUpdated {
                workspace_id: ws_id,
                status: mk_status(&format!("src-{i}"), StatusLevel::Info, "ok"),
            });
        }
        assert_eq!(
            state.active_workspace().unwrap().external_statuses.len(),
            MAX_EXTERNAL_STATUSES
        );
    }

    #[test]
    fn external_status_updated_clears_matching_source_on_empty_label() {
        use daedalus_proto::StatusLevel;

        let mut state = AppState::new();
        let (ws, _, _, _) = make_workspace_with_three_panes();
        let ws_id = ws.id;
        state.set_workspaces(vec![ws]);

        let mk_status = |source: &str, label: &str| ExternalStatus {
            source: source.to_string(),
            level: StatusLevel::Info,
            label: label.to_string(),
            detail: None,
            updated_at_ms: 1,
        };

        state.apply_server_event(ServerEvent::ExternalStatusUpdated {
            workspace_id: ws_id,
            status: mk_status("phrourion", "3 dirty"),
        });
        state.apply_server_event(ServerEvent::ExternalStatusUpdated {
            workspace_id: ws_id,
            status: mk_status("cassandra", "ok"),
        });
        assert_eq!(state.active_workspace().unwrap().external_statuses.len(), 2);

        // Empty-label clear signal removes matching source without leaving a blank badge
        state.apply_server_event(ServerEvent::ExternalStatusUpdated {
            workspace_id: ws_id,
            status: mk_status("phrourion", ""),
        });
        let statuses = &state.active_workspace().unwrap().external_statuses;
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].source, "cassandra");

        // Clearing an unknown source with whitespace label is a no-op
        state.apply_server_event(ServerEvent::ExternalStatusUpdated {
            workspace_id: ws_id,
            status: mk_status("unknown", "   "),
        });
        assert_eq!(state.active_workspace().unwrap().external_statuses.len(), 1);
    }

    #[test]
    fn worktree_modal_and_picker_are_mutually_exclusive() {
        let mut state = AppState::new();
        assert!(!state.picker.open);
        assert!(!state.worktree_modal.open);

        state.open_picker_with(AgentPreset::Claude);
        assert!(state.picker.open);
        assert!(!state.worktree_modal.open);

        // Opening worktree modal closes picker and resets branch input
        state.open_worktree_modal();
        assert!(!state.picker.open);
        assert!(state.worktree_modal.open);
        state.worktree_modal.push_char('f');
        state.worktree_modal.push_char('e');
        state.worktree_modal.pop_char();
        assert_eq!(state.worktree_modal.branch_input, "f");

        // Opening picker closes worktree modal and clears input
        state.open_picker_with(AgentPreset::Agy);
        assert!(state.picker.open);
        assert!(!state.worktree_modal.open);
        assert!(state.worktree_modal.branch_input.is_empty());

        state.toggle_worktree_modal();
        assert!(!state.picker.open);
        assert!(state.worktree_modal.open);
        state.toggle_worktree_modal();
        assert!(!state.worktree_modal.open);
    }

    #[test]
    fn search_state_steps_with_wrap_and_computes_scrollback_offset() {
        let pane_id = PaneId::new();
        let mut search = SearchState::new(pane_id);
        assert_eq!(search.display_counter(), (0, 0));
        assert_eq!(search.step_next(), None);
        assert_eq!(search.step_prev(), None);
        assert_eq!(search.target_scroll_offset(20), None);

        search.push_text("err");
        search.push_text("or");
        assert_eq!(search.query, "error");
        assert!(search.pop_char());
        assert_eq!(search.query, "erro");

        // Suppose total_scrollback = 100, viewport_rows = 20 -> live viewport is abs_row 100..120
        let matches = vec![
            SearchMatch {
                abs_row: 5,
                start_col: 0,
                end_col: 4,
            },
            SearchMatch {
                abs_row: 50,
                start_col: 2,
                end_col: 6,
            },
            SearchMatch {
                abs_row: 112,
                start_col: 1,
                end_col: 5,
            },
        ];
        search.set_results(100, matches.clone());
        assert_eq!(search.display_counter(), (1, 3));
        assert_eq!(search.active_match(), Some(matches[0]));
        // abs_row=5 near top clamps to total_scrollback=100 (window 0..20)
        assert_eq!(search.target_scroll_offset(20), Some(100));

        // Step next -> abs_row=50 centered in 20-row window: offset = (100 - 50) + 10 = 60 (window 40..60)
        assert_eq!(search.step_next(), Some(matches[1]));
        assert_eq!(search.display_counter(), (2, 3));
        assert_eq!(search.target_scroll_offset(20), Some(60));

        // Step next -> abs_row=112 in live viewport (100..120): offset = 0
        assert_eq!(search.step_next(), Some(matches[2]));
        assert_eq!(search.display_counter(), (3, 3));
        assert_eq!(search.target_scroll_offset(20), Some(0));

        // Wrap forward -> index 0; wrap backward -> index 2
        assert_eq!(search.step_next(), Some(matches[0]));
        assert_eq!(search.current_index, 0);
        assert_eq!(search.step_prev(), Some(matches[2]));
        assert_eq!(search.current_index, 2);
        assert_eq!(search.step_prev(), Some(matches[1]));
        assert_eq!(search.current_index, 1);

        // Editing query clears stale matches and resets index
        search.push_text("r");
        assert!(search.matches.is_empty());
        assert_eq!(search.display_counter(), (0, 0));
    }

    #[test]
    fn dormant_workspace_helpers_prevent_double_spawning() {
        let mut state = AppState::new();
        let ws_id = WorkspaceId::new();
        let dormant = WorkspaceInfo {
            project: Default::default(),
            default_panes_pending: None,
            id: ws_id,
            name: "dormant".to_string(),
            root_dir: PathBuf::from("/tmp/dormant"),
            slot_index: 0,
            injected_env: HashMap::new(),
            panes: Vec::new(),
            port_offset: 0,
            compose_project_name: None,
            git_branch: None,
            git_status: None,
            bound_ports: Vec::new(),
            containers: Vec::new(),
            external_statuses: Vec::new(),
            manifest: None,
        };
        state.set_workspaces(vec![dormant]);

        // Dormant active workspace is detected before spawning begins
        assert_eq!(state.active_dormant_workspace().map(|w| w.id), Some(ws_id));
        assert!(state.mark_workspace_spawning(ws_id));
        // Second mark returns false and active_dormant_workspace suppresses double-spawn
        assert!(!state.mark_workspace_spawning(ws_id));
        assert!(state.active_dormant_workspace().is_none());

        // Clearing spawning flag without panes still sees it as dormant again
        state.finish_workspace_spawning(ws_id, false);
        assert_eq!(state.active_dormant_workspace().map(|w| w.id), Some(ws_id));

        // Legacy daemons infer completion from the first pane.
        assert!(state.mark_workspace_spawning(ws_id));
        state.register_pane(make_pane_summary(ws_id, PaneRole::Agent), None);
        state.finish_workspace_spawning(ws_id, false);
        assert!(state.active_dormant_workspace().is_none());

        // A newer daemon can explicitly report a partially provisioned workspace.
        state.active_workspace_mut().unwrap().default_panes_pending = Some(true);
        assert_eq!(state.active_dormant_workspace().map(|w| w.id), Some(ws_id));
        state.mark_workspace_spawning(ws_id);
        state.finish_workspace_spawning(ws_id, false);
        assert!(state.active_dormant_workspace().is_some());
        state.finish_workspace_spawning(ws_id, true);
        state.active_workspace_mut().unwrap().panes.clear();
        assert!(state.active_dormant_workspace().is_none());
    }

    #[test]
    fn stale_search_results_are_ignored_when_generation_or_pane_mismatches() {
        let mut state = AppState::new();
        let (ws, agent_id, service_id, _) = make_workspace_with_three_panes();
        state.set_workspaces(vec![ws]);
        state.toggle_search(agent_id);

        let search = state.search.as_mut().unwrap();
        let gen_init = search.generation;
        search.push_text("err");
        let gen_err = search.generation;
        assert!(gen_err > gen_init);

        // User keeps typing before the response for "err" comes back
        search.push_text("or");
        let gen_error = search.generation;
        assert!(gen_error > gen_err);

        let stale_match = vec![SearchMatch {
            abs_row: 3,
            start_col: 0,
            end_col: 3,
        }];
        let fresh_match = vec![SearchMatch {
            abs_row: 8,
            start_col: 1,
            end_col: 6,
        }];

        // Stale generation from "err" is dropped
        assert!(!state.apply_search_results(agent_id, gen_err, 20, stale_match.clone()));
        assert!(state.search.as_ref().unwrap().matches.is_empty());

        // Results for a different pane are dropped
        assert!(!state.apply_search_results(service_id, gen_error, 20, fresh_match.clone()));
        assert!(state.search.as_ref().unwrap().matches.is_empty());

        // Matching pane_id + current generation applies cleanly
        assert!(state.apply_search_results(agent_id, gen_error, 20, fresh_match.clone()));
        assert_eq!(state.search.as_ref().unwrap().matches, fresh_match);

        // If user backspaces back to empty query, any in-flight response for earlier gen is dropped
        for _ in 0..5 {
            state.search.as_mut().unwrap().pop_char();
        }
        assert!(state.search.as_ref().unwrap().query.is_empty());
        assert!(!state.apply_search_results(agent_id, gen_error, 20, fresh_match));
        assert!(state.search.as_ref().unwrap().matches.is_empty());
    }

    #[test]
    fn backspace_removes_one_grapheme_in_search_and_branch_fields() {
        let mut search = SearchState::new(PaneId::new());
        let mut branch = WorktreeModalState::default();
        // Some input methods deliver a base and its combining mark separately.
        for committed in ["e", "\u{301}", "🇳🇱", "👩🏽‍💻"] {
            search.push_text(committed);
            branch.push_text(committed);
        }
        for expected in ["e\u{301}🇳🇱", "e\u{301}", ""] {
            let generation = search.generation;
            search.set_results(0, Vec::new());
            assert!(search.pop_char());
            branch.pop_char();
            assert_eq!(search.query, expected);
            assert_eq!(branch.branch_input, expected);
            assert_ne!(search.generation, generation);
            assert!(!search.apply_results(generation, 0, Vec::new()));
        }
        let empty_generation = search.generation;
        assert!(!search.pop_char());
        branch.pop_char();
        assert!(branch.branch_input.is_empty());
        assert_eq!(search.generation, empty_generation);
    }

    #[test]
    fn closing_and_reopening_search_rejects_earlier_session_results() {
        let mut state = AppState::new();
        let (ws, agent_id, _, _) = make_workspace_with_three_panes();
        state.set_workspaces(vec![ws]);

        // Session 1: open search, type "a", then close
        state.toggle_search(agent_id);
        state.search.as_mut().unwrap().push_text("a");
        let gen_session1 = state.search.as_ref().unwrap().generation;
        state.close_search();

        // Session 2: reopen search on same pane and type "z"
        state.toggle_search(agent_id);
        state.search.as_mut().unwrap().push_text("z");
        let gen_session2 = state.search.as_ref().unwrap().generation;
        assert_ne!(gen_session1, gen_session2);

        let old_match = vec![SearchMatch {
            abs_row: 1,
            start_col: 0,
            end_col: 1,
        }];
        let new_match = vec![SearchMatch {
            abs_row: 4,
            start_col: 2,
            end_col: 3,
        }];

        // Late response from session 1 is rejected even though query length matches
        assert!(!state.apply_search_results(agent_id, gen_session1, 10, old_match));
        assert!(state.search.as_ref().unwrap().matches.is_empty());

        // Current session 2 response applies cleanly
        assert!(state.apply_search_results(agent_id, gen_session2, 10, new_match.clone()));
        assert_eq!(state.search.as_ref().unwrap().matches, new_match);
    }

    #[test]
    fn pending_close_retains_options_until_workspace_disappears() {
        let mut state = AppState::new();
        let (mut workspace, _, _, _) = make_workspace_with_three_panes();
        workspace.compose_project_name = Some("test".into());
        let id = workspace.id;
        state.set_workspaces(vec![workspace.clone()]);
        assert!(state.open_close_modal_for_workspace(id));
        state.close_modal.toggle_compose();
        state.close_modal.submitting = true;
        state.close_modal.toggle_compose();
        state.set_workspaces(vec![workspace]);
        assert!(state.close_modal.open);
        assert!(!state.close_modal.compose_down);
        state.set_workspaces(Vec::new());
        assert!(!state.close_modal.open);
        assert!(!state.close_modal.submitting);
    }

    #[test]
    fn close_workspace_modal_defaults_toggles_and_mutual_exclusion() {
        let mut state = AppState::new();
        let (mut primary_ws, agent_id, _, _) = make_workspace_with_three_panes();
        primary_ws.name = "primary".to_string();
        primary_ws.git_branch = Some("master".to_string());
        primary_ws.git_status = Some(GitWorkspaceStatus {
            is_worktree: false,
            dirty: false,
            ahead: 0,
            behind: 0,
        });

        let (mut wt_ws, _, _, _) = make_workspace_with_three_panes();
        wt_ws.name = "feat-wt".to_string();
        wt_ws.compose_project_name = Some("daedalus-wt1".to_string());
        wt_ws.git_branch = Some("feat/wt".to_string());
        wt_ws.git_status = Some(GitWorkspaceStatus {
            is_worktree: true,
            dirty: true,
            ahead: 2,
            behind: 1,
        });
        let primary_id = primary_ws.id;
        let wt_id = wt_ws.id;

        state.set_workspaces(vec![primary_ws, wt_ws]);
        state.open_picker_with(AgentPreset::Claude);
        state.toggle_search(agent_id);

        // Opening close modal for worktree workspace closes search/modals and sets defaults
        assert!(state.open_close_modal_for_workspace(wt_id));
        assert!(!state.picker.open);
        assert!(state.search.is_none());
        assert!(state.close_modal.open);
        assert_eq!(state.close_modal.workspace_id, Some(wt_id));
        assert_eq!(state.close_modal.workspace_name, "feat-wt");
        assert_eq!(state.close_modal.branch.as_deref(), Some("feat/wt"));
        assert!(state.close_modal.is_worktree);
        assert!(state.close_modal.is_dirty);
        assert!(state.close_modal.has_compose);
        assert!(state.close_modal.compose_down);
        assert!(!state.close_modal.remove_worktree);
        assert!(!state.close_modal.force_remove_worktree);

        // Force cannot be toggled before remove_worktree is enabled
        state.close_modal.toggle_force();
        assert!(!state.close_modal.force_remove_worktree);

        // Enable remove_worktree, then enable force
        state.close_modal.toggle_worktree();
        assert!(state.close_modal.remove_worktree);
        state.close_modal.toggle_force();
        assert!(state.close_modal.force_remove_worktree);

        // Disabling remove_worktree also clears force_remove_worktree
        state.close_modal.toggle_worktree();
        assert!(!state.close_modal.remove_worktree);
        assert!(!state.close_modal.force_remove_worktree);

        // Toggle compose off/on
        state.close_modal.toggle_compose();
        assert!(!state.close_modal.compose_down);
        state.close_modal.toggle_compose();
        assert!(state.close_modal.compose_down);

        // Clean worktree can toggle remove_worktree, but cannot enable force_remove_worktree
        let (mut clean_wt, _, _, _) = make_workspace_with_three_panes();
        clean_wt.name = "clean-wt".to_string();
        clean_wt.git_status = Some(GitWorkspaceStatus {
            is_worktree: true,
            dirty: false,
            ahead: 0,
            behind: 0,
        });
        let clean_wt_id = clean_wt.id;
        state.upsert_workspace(clean_wt);
        assert!(state.open_close_modal_for_workspace(clean_wt_id));
        assert!(state.close_modal.is_worktree);
        assert!(!state.close_modal.is_dirty);
        state.close_modal.toggle_worktree();
        assert!(state.close_modal.remove_worktree);
        state.close_modal.toggle_force();
        assert!(!state.close_modal.force_remove_worktree);

        // Primary checkout cannot enable remove_worktree or force
        assert!(state.open_close_modal_for_workspace(primary_id));
        assert!(!state.close_modal.is_worktree);
        assert!(!state.close_modal.has_compose);
        assert!(!state.close_modal.compose_down);
        state.close_modal.toggle_worktree();
        assert!(!state.close_modal.remove_worktree);
        state.close_modal.toggle_force();
        assert!(!state.close_modal.force_remove_worktree);
    }

    #[test]
    fn agent_state_and_workspace_runtime_updates_sync_state() {
        let mut state = AppState::new();
        let (ws, agent_id, _, _) = make_workspace_with_three_panes();
        let ws_id = ws.id;
        state.set_workspaces(vec![ws]);

        assert_eq!(
            state.workspace_agent_state(state.active_workspace().unwrap()),
            None
        );

        state.apply_server_event(ServerEvent::AgentStateChanged {
            workspace_id: ws_id,
            pane_id: agent_id,
            state: AgentState::WaitingForApproval,
        });

        assert_eq!(
            state.panes.get(&agent_id).unwrap().summary.agent_state,
            Some(AgentState::WaitingForApproval)
        );
        assert_eq!(
            state.workspace_agent_state(state.active_workspace().unwrap()),
            Some(AgentState::WaitingForApproval)
        );

        let gs = GitWorkspaceStatus {
            is_worktree: true,
            dirty: true,
            ahead: 3,
            behind: 1,
        };
        state.apply_server_event(ServerEvent::WorkspaceRuntimeUpdated {
            workspace_id: ws_id,
            bound_ports: Vec::new(),
            containers: Vec::new(),
            git_status: Some(gs),
            git_branch: Some("feat/v0.0.3".to_string()),
        });
        let updated = state.active_workspace().unwrap();
        assert_eq!(updated.git_status, Some(gs));
        assert_eq!(updated.git_branch.as_deref(), Some("feat/v0.0.3"));
    }

    #[test]
    fn repeated_role_shortcut_selects_a_specific_session_and_restores_it() {
        let mut state = AppState::new();
        let (mut first, original, _, _) = make_workspace_with_three_panes();
        let second_agent = make_pane_summary(first.id, PaneRole::Agent);
        let newest = second_agent.id;
        first.panes.push(second_agent);
        let (other, _, _, _) = make_workspace_with_three_panes();
        state.set_workspaces(vec![first, other]);
        assert_eq!(state.focused_pane_id(), Some(newest));
        state.focus_role_shortcut(PaneRole::Agent);
        assert_eq!(state.focused_pane_id(), Some(original));
        state.cycle_workspace(1);
        state.focus_role_shortcut(PaneRole::Runner);
        state.cycle_workspace(-1);
        assert_eq!(state.focused_pane_id(), Some(original));
        state.focus_role_shortcut(PaneRole::Agent);
        assert_eq!(state.focused_pane_id(), Some(newest));
        state.cycle_workspace(1);
        assert_eq!(state.focused_role, PaneRole::Runner);
    }

    #[test]
    fn cycle_workspace_wraps_cleanly_forward_and_backward() {
        let mut state = AppState::new();
        assert_eq!(state.cycle_workspace(1), None);
        assert_eq!(state.cycle_workspace(-1), None);

        let (ws1, _, _, _) = make_workspace_with_three_panes();
        let (ws2, _, _, _) = make_workspace_with_three_panes();
        let (ws3, _, _, _) = make_workspace_with_three_panes();
        let id1 = ws1.id;
        let id2 = ws2.id;
        let id3 = ws3.id;
        state.set_workspaces(vec![ws1, ws2, ws3]);
        assert_eq!(state.active_workspace_id, Some(id1));

        assert_eq!(state.cycle_workspace(1), Some(id2));
        assert_eq!(state.cycle_workspace(1), Some(id3));
        assert_eq!(state.cycle_workspace(1), Some(id1));
        assert_eq!(state.cycle_workspace(-1), Some(id3));
        assert_eq!(state.cycle_workspace(-1), Some(id2));
        assert_eq!(state.cycle_workspace(-1), Some(id1));
    }
}
