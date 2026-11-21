use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use clap::ValueEnum;
use daedalus_proto::{
    CursorState, ExternalStatus, FailureTrace, GridDelta, GridRow, GridSnapshot, PaneId, PaneRole,
    PaneStatus, PaneSummary, SearchMatch, ServerEvent, WorkspaceId, WorkspaceInfo,
};

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
    pub open: bool,
    pub branch_input: String,
}

impl WorktreeModalState {
    pub fn open(&mut self) {
        self.open = true;
        self.branch_input.clear();
    }

    pub fn close(&mut self) {
        self.open = false;
        self.branch_input.clear();
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
    }

    pub fn push_text(&mut self, text: &str) {
        self.branch_input.push_str(text);
    }

    pub fn pop_char(&mut self) {
        self.branch_input.pop();
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
        }
    }

    pub fn push_text(&mut self, text: &str) {
        if !text.is_empty() {
            self.query.push_str(text);
            self.generation = Self::next_generation();
            self.matches.clear();
            self.current_index = 0;
        }
    }

    pub fn pop_char(&mut self) -> bool {
        let popped = self.query.pop().is_some();
        if popped {
            self.generation = Self::next_generation();
            self.matches.clear();
            self.current_index = 0;
        }
        popped
    }

    pub fn set_results(&mut self, total_scrollback: usize, matches: Vec<SearchMatch>) {
        self.total_scrollback = total_scrollback;
        self.matches = matches;
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

/// Client-side cached terminal state for a single daemon pane.
#[derive(Debug, Clone)]
pub struct PaneViewState {
    pub summary: PaneSummary,
    pub cols: u16,
    pub rows: u16,
    pub cursor: CursorState,
    pub viewport_rows: Vec<GridRow>,
    pub scrollback_len: usize,
    pub scroll_offset: usize,
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
            scrolled_rows: None,
            dirty_row_indices: Vec::new(),
            selection: None,
            agent_preset: None,
            last_failure_trace: None,
            last_seq: 0,
        }
    }

    /// Replaces the cached viewport with a full `GridSnapshot` and marks all rows dirty.
    pub fn apply_snapshot(&mut self, snapshot: GridSnapshot) {
        self.cols = snapshot.cols;
        self.rows = snapshot.rows;
        self.summary.cols = snapshot.cols;
        self.summary.rows = snapshot.rows;
        self.cursor = snapshot.cursor;
        self.last_seq = snapshot.seq;
        self.scrollback_len = snapshot.scrollback_len;
        self.viewport_rows = snapshot.rows_data;
        self.scroll_offset = self.scroll_offset.min(self.scrollback_len);
        if self.scroll_offset == 0 {
            self.scrolled_rows = None;
        }
        self.mark_all_visible_dirty();
    }

    /// Patches only the rows present in `delta` and records their indices for incremental shaping.
    pub fn apply_delta(&mut self, delta: GridDelta) {
        if self.last_seq != 0 && delta.seq <= self.last_seq {
            return;
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
            self.mark_all_visible_dirty();
        }
    }

    /// Applies a `GetScrollback` response from `daedalusd`.
    pub fn apply_scrollback(&mut self, total_scrollback: usize, rows: Vec<GridRow>) {
        self.scrollback_len = total_scrollback;
        self.scroll_offset = self.scroll_offset.min(total_scrollback);
        if self.scroll_offset == 0 {
            self.scrolled_rows = None;
        } else {
            self.scrolled_rows = Some(rows);
        }
        self.mark_all_visible_dirty();
    }

    /// Returns the rows currently visible in the pane viewport (scrolled history or live viewport).
    pub fn visible_rows(&self) -> &[GridRow] {
        if self.scroll_offset > 0
            && let Some(ref scrolled) = self.scrolled_rows
        {
            return scrolled.as_slice();
        }
        &self.viewport_rows
    }

    /// Adjusts `scroll_offset` by `delta` (positive = scroll up into history,
    /// negative = scroll down toward live output), clamped to `0..=scrollback_len`.
    pub fn adjust_scroll_offset(&mut self, delta: isize) -> usize {
        let candidate = if delta >= 0 {
            self.scroll_offset.saturating_add(delta as usize)
        } else {
            self.scroll_offset.saturating_sub(delta.unsigned_abs())
        };
        let clamped = candidate.min(self.scrollback_len);
        if clamped != self.scroll_offset {
            self.scroll_offset = clamped;
            if self.scroll_offset == 0 {
                self.scrolled_rows = None;
                self.mark_all_visible_dirty();
            }
        }
        self.scroll_offset
    }

    /// Returns the pane to the live bottom viewport (`scroll_offset = 0`).
    pub fn reset_scroll(&mut self) {
        if self.scroll_offset != 0 || self.scrolled_rows.is_some() {
            self.scroll_offset = 0;
            self.scrolled_rows = None;
            self.mark_all_visible_dirty();
        }
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

/// Top-level UI application state shared between input handling, daemon bridge events, and rendering.
#[derive(Debug)]
pub struct AppState {
    pub connected: bool,
    pub workspaces: Vec<WorkspaceInfo>,
    pub active_workspace_id: Option<WorkspaceId>,
    pub panes: HashMap<PaneId, PaneViewState>,
    pub focused_role: PaneRole,
    pub sidebar_visible: bool,
    pub zoomed: bool,
    pub picker: PickerState,
    pub worktree_modal: WorktreeModalState,
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
    pub fn new() -> Self {
        Self {
            connected: false,
            workspaces: Vec::new(),
            active_workspace_id: None,
            panes: HashMap::new(),
            focused_role: PaneRole::Agent,
            sidebar_visible: true,
            zoomed: false,
            picker: PickerState::default(),
            worktree_modal: WorktreeModalState::default(),
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
            self.picker.close();
            self.worktree_modal.close();
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

    /// Returns the active workspace if it is dormant (`panes.is_empty()`) and not already
    /// spawning its default 3 panes.
    pub fn active_dormant_workspace(&self) -> Option<&WorkspaceInfo> {
        self.active_workspace()
            .filter(|ws| ws.panes.is_empty() && !self.spawning_workspaces.contains(&ws.id))
    }

    /// Marks `workspace_id` as having an in-flight `EnsureWorkspacePanes` request.
    /// Returns `true` if it was not already marked.
    pub fn mark_workspace_spawning(&mut self, workspace_id: WorkspaceId) -> bool {
        self.spawning_workspaces.insert(workspace_id)
    }

    pub fn clear_workspace_spawning(&mut self, workspace_id: WorkspaceId) {
        self.spawning_workspaces.remove(&workspace_id);
    }

    /// Opens the Agent preset picker modal while ensuring the worktree modal is closed.
    pub fn open_picker_with(&mut self, preset: AgentPreset) {
        self.worktree_modal.close();
        self.picker.open_with(preset);
    }

    /// Opens the Git worktree creator modal while ensuring the Agent preset picker is closed.
    pub fn open_worktree_modal(&mut self) {
        self.picker.close();
        self.worktree_modal.open();
    }

    /// Toggles the Git worktree creator modal, closing the Agent preset picker if opening.
    pub fn toggle_worktree_modal(&mut self) {
        if self.worktree_modal.open {
            self.worktree_modal.close();
        } else {
            self.open_worktree_modal();
        }
    }

    pub fn active_workspace(&self) -> Option<&WorkspaceInfo> {
        let id = self.active_workspace_id?;
        self.workspaces.iter().find(|ws| ws.id == id)
    }

    pub fn active_workspace_mut(&mut self) -> Option<&mut WorkspaceInfo> {
        let id = self.active_workspace_id?;
        self.workspaces.iter_mut().find(|ws| ws.id == id)
    }

    pub fn pane_id_for_role(&self, role: PaneRole) -> Option<PaneId> {
        let ws = self.active_workspace()?;
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

    pub fn focused_pane_id(&self) -> Option<PaneId> {
        self.pane_id_for_role(self.focused_role)
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

        self.workspaces = workspaces;
        self.spawning_workspaces
            .retain(|ws_id| self.workspaces.iter().any(|ws| ws.id == *ws_id));
        let active_still_exists = self
            .active_workspace_id
            .is_some_and(|id| self.workspaces.iter().any(|ws| ws.id == id));
        if !active_still_exists {
            self.active_workspace_id = self.workspaces.first().map(|ws| ws.id);
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
        let entry = self
            .panes
            .entry(summary.id)
            .and_modify(|view| view.summary = summary.clone())
            .or_insert_with(|| PaneViewState::from_summary(summary));
        if let Some(preset) = agent_preset {
            entry.agent_preset = Some(preset);
        }
    }

    /// Applies a `ServerEvent` from `daedalusd`.
    /// Returns `Some(pane_id)` when `ServerEvent::ResyncRequired` requires a fresh `GetSnapshot`.
    pub fn apply_server_event(&mut self, event: ServerEvent) -> Option<PaneId> {
        match event {
            ServerEvent::GridDelta(delta) => {
                if let Some(pane) = self.panes.get_mut(&delta.pane_id) {
                    pane.apply_delta(delta);
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
            } => {
                if let Some(ws) = self.workspaces.iter_mut().find(|w| w.id == workspace_id) {
                    ws.bound_ports = bound_ports;
                    ws.containers = containers;
                }
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
    pub fn focus_direction(&mut self, dir: FocusDirection) -> PaneRole {
        let next = match (self.focused_role, dir) {
            (PaneRole::Agent, FocusDirection::Right) => PaneRole::Service,
            (PaneRole::Service, FocusDirection::Left)
            | (PaneRole::Runner, FocusDirection::Left) => PaneRole::Agent,
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
    } else if statuses.len() >= MAX_EXTERNAL_STATUSES {
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
        }
    }

    fn make_workspace_with_three_panes() -> (WorkspaceInfo, PaneId, PaneId, PaneId) {
        let ws_id = WorkspaceId::new();
        let agent = make_pane_summary(ws_id, PaneRole::Agent);
        let service = make_pane_summary(ws_id, PaneRole::Service);
        let runner = make_pane_summary(ws_id, PaneRole::Runner);
        let ids = (agent.id, service.id, runner.id);
        let ws = WorkspaceInfo {
            id: ws_id,
            name: "demo".to_string(),
            root_dir: PathBuf::from("/tmp/repo"),
            slot_index: 0,
            injected_env: HashMap::new(),
            panes: vec![agent, service, runner],
            port_offset: 0,
            compose_project_name: None,
            git_branch: None,
            bound_ports: Vec::new(),
            containers: Vec::new(),
            external_statuses: Vec::new(),
            manifest: None,
        };
        (ws, ids.0, ids.1, ids.2)
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
            scrollback_len: 5,
        });

        // Clamps above scrollback_len
        assert_eq!(pane.adjust_scroll_offset(100), 5);
        // Clamps below 0
        assert_eq!(pane.adjust_scroll_offset(-100), 0);

        // Scroll up 3 lines and load scrollback rows
        assert_eq!(pane.adjust_scroll_offset(3), 3);
        pane.apply_scrollback(5, vec![make_row(0, 2, "hist-2"), make_row(1, 3, "hist-3")]);
        assert_eq!(row_text(&pane.visible_rows()[0]), "hist-2");
        assert_eq!(row_text(&pane.visible_rows()[1]), "hist-3");

        // Reset scroll returns to live viewport
        pane.reset_scroll();
        assert_eq!(pane.scroll_offset, 0);
        assert!(pane.scrolled_rows.is_none());
        assert_eq!(row_text(&pane.visible_rows()[0]), "live-0");
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
    fn directional_focus_navigates_three_pane_layout() {
        let mut state = AppState::new();
        assert_eq!(state.focused_role, PaneRole::Agent);

        assert_eq!(
            state.focus_direction(FocusDirection::Right),
            PaneRole::Service
        );
        assert_eq!(
            state.focus_direction(FocusDirection::Down),
            PaneRole::Runner
        );
        assert_eq!(state.focus_direction(FocusDirection::Up), PaneRole::Service);
        assert_eq!(state.focus_direction(FocusDirection::Left), PaneRole::Agent);
        // Moving left from Agent is a no-op
        assert_eq!(state.focus_direction(FocusDirection::Left), PaneRole::Agent);
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
            id: ws_id,
            name: "dormant".to_string(),
            root_dir: PathBuf::from("/tmp/dormant"),
            slot_index: 0,
            injected_env: HashMap::new(),
            panes: Vec::new(),
            port_offset: 0,
            compose_project_name: None,
            git_branch: None,
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
        state.clear_workspace_spawning(ws_id);
        assert_eq!(state.active_dormant_workspace().map(|w| w.id), Some(ws_id));

        // Once a pane spawns, it is no longer dormant
        assert!(state.mark_workspace_spawning(ws_id));
        state.register_pane(make_pane_summary(ws_id, PaneRole::Agent), None);
        state.clear_workspace_spawning(ws_id);
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
}
