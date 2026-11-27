use std::collections::{HashMap, HashSet};
use std::time::Instant;

use clap::ValueEnum;
use daedalus_proto::{
    AgentState, ContainerHealth, ExternalStatus, GitWorkspaceStatus, GridSnapshot, PaneId,
    PaneRole, PaneStatus, PaneSummary, PortMapping, SearchMatch, ServerEvent, WorkspaceId,
    WorkspaceInfo,
};
use unicode_segmentation::UnicodeSegmentation;

mod dialogs;
mod pane;
mod search;
mod widgets;

pub use dialogs::{
    ActionPaletteState, CloseWorkspaceModalState, PickerState, SessionAction, WorktreeModalState,
};
pub use pane::PaneViewState;
pub use search::SearchState;

/// Ordered workspace membership. Pane metadata lives in AppState::panes.
pub type WorkspaceView = daedalus_proto::types::Workspace<PaneId>;

/// Put the exit status first so it stays visible when a terminal title is clipped.
pub fn terminal_title(label: String, status: PaneStatus) -> String {
    match status {
        PaneStatus::Exited { code: Some(code) } => format!("Exited {code} · {label}"),
        PaneStatus::Exited { code: None } => format!("Exited · {label}"),
        _ => label,
    }
}

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
        let canonical = daedalus_proto::canonical_agent_preset(name)?;
        Self::ALL
            .into_iter()
            .find(|preset| preset.short_name() == canonical)
    }

    pub(crate) fn from_executable(program: &str) -> Option<Self> {
        daedalus_proto::agent_launch_preset(program).and_then(Self::from_cli_name)
    }
}

fn remove_last_grapheme(text: &mut String) -> bool {
    let Some((start, _)) = text.grapheme_indices(true).next_back() else {
        return false;
    };
    text.truncate(start);
    true
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActiveOverlay {
    FailureRecipients,
    SetupReview,
    WorkspaceForm,
    Settings,
    ActionPalette,
    AgentPicker,
    Worktree,
    CloseWorkspace,
    Search,
}

/// Top-level UI application state shared between input handling, daemon bridge events, and rendering.
#[derive(Debug)]
pub struct AppState {
    pub connection: ConnectionState,
    pub workspace_list_loaded: bool,
    pub workspaces: Vec<WorkspaceView>,
    pub active_workspace_id: Option<WorkspaceId>,
    pub panes: HashMap<PaneId, PaneViewState>,
    pub focused_role: PaneRole,
    workspace_focus: HashMap<WorkspaceId, PaneRole>,
    selected_panes: HashMap<(WorkspaceId, PaneRole), PaneId>,
    pub image_preview: Option<(PaneId, daedalus_proto::rpc::ImagePreview)>,
    pub pending_image_preview: Option<(PaneId, uuid::Uuid)>,
    pub sidebar_override: Option<bool>,
    pub sidebar_first: usize,
    pub zoomed: bool,
    pub remote_host: Option<String>,
    pub failure_recipients: Option<FailureRecipients>,
    pub setup_review: crate::setup_review::SetupReview,
    pub workspace_form: crate::workspace_form::WorkspaceForm,
    pub settings: crate::settings::SettingsDialog,
    pub workspace_widgets: HashMap<WorkspaceId, crate::workspace_widgets::WorkspaceWidgets>,
    pub workbench: Option<(WorkspaceId, daedalus_proto::workbench::WorkbenchState)>,
    pub widget_results: Vec<daedalus_proto::workbench::WidgetSnapshot>,
    pub workbench_repo_update: bool,
    pub recap: crate::recap::Recap,
    pub test_workspaces: HashSet<WorkspaceId>,
    pub reduced_motion: bool,
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
    pub fn selected_png_path(&self) -> Option<String> {
        let pane = self.panes.get(&self.focused_pane_id()?)?;
        let selection = pane.selection?;
        if selection.anchor.abs_row != selection.head.abs_row {
            return None;
        }
        let text = crate::selection::extract_selected_text(pane.visible_rows(), &selection);
        let path = text.trim();
        (path.len() <= 4096
            && !path.chars().any(char::is_control)
            && std::path::Path::new(path)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("png")))
        .then(|| path.to_owned())
    }

    /// Frontmost input surface in the same order as visual overlays.
    pub fn active_overlay(&self) -> Option<ActiveOverlay> {
        [
            (
                self.failure_recipients.is_some(),
                ActiveOverlay::FailureRecipients,
            ),
            (self.setup_review.open, ActiveOverlay::SetupReview),
            (self.workspace_form.open, ActiveOverlay::WorkspaceForm),
            (self.settings.open, ActiveOverlay::Settings),
            (self.action_palette.open, ActiveOverlay::ActionPalette),
            (self.picker.open, ActiveOverlay::AgentPicker),
            (self.worktree_modal.open, ActiveOverlay::Worktree),
            (self.close_modal.open, ActiveOverlay::CloseWorkspace),
            (self.search.is_some(), ActiveOverlay::Search),
        ]
        .into_iter()
        .find_map(|(open, overlay)| open.then_some(overlay))
    }

    pub fn has_input_overlay(&self) -> bool {
        self.active_overlay().is_some()
    }

    pub fn can_create_worktree(&self) -> bool {
        self.connection == ConnectionState::Connected
            && !self.worktree_modal.branch_input.trim().is_empty()
    }

    pub fn new() -> Self {
        Self {
            connection: ConnectionState::Connecting,
            workspace_list_loaded: false,
            workspaces: Vec::new(),
            active_workspace_id: None,
            panes: HashMap::new(),
            focused_role: PaneRole::Agent,
            image_preview: None,
            pending_image_preview: None,
            sidebar_override: None,
            sidebar_first: 0,
            workspace_focus: HashMap::new(),
            selected_panes: HashMap::new(),
            zoomed: true,
            remote_host: None,
            failure_recipients: None,
            setup_review: Default::default(),
            workspace_form: Default::default(),
            settings: Default::default(),
            workspace_widgets: HashMap::new(),
            workbench: None,
            widget_results: Vec::new(),
            workbench_repo_update: false,
            recap: Default::default(),
            test_workspaces: HashSet::new(),
            reduced_motion: false,
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

    pub fn focus_clicked_pane(&mut self, role: PaneRole) {
        if self
            .search
            .as_ref()
            .is_some_and(|s| Some(s.pane_id) != self.pane_id_for_role(role))
        {
            self.close_search();
        }
        self.focused_role = role;
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
    pub fn active_dormant_workspace(&self) -> Option<&WorkspaceView> {
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

    /// Status and recovery guidance shared by the visible terminal and the screen reader.
    pub fn terminal_guidance(
        &self,
        role: PaneRole,
    ) -> Option<(&'static str, Option<&'static str>)> {
        if self.connection != ConnectionState::Connected
            || role != self.focused_role
            || self.pane_id_for_role(role).is_some()
        {
            return None;
        }
        let Some(workspace) = self.active_workspace() else {
            return (self.workspace_list_loaded && self.workspaces.is_empty()).then_some((
                "No workspaces open",
                Some("Choose Workspaces → + Workspace."),
            ));
        };
        if self.spawning_workspaces.contains(&workspace.id) {
            return Some(("Starting terminals…", None));
        }
        if workspace.needs_default_panes() {
            return None;
        }
        let title = if workspace.panes.is_empty() {
            "No terminals open"
        } else {
            match role {
                PaneRole::Agent => "No agent terminal open",
                PaneRole::Service => "No service terminal open",
                PaneRole::Runner => "No runner terminal open",
            }
        };
        Some((title, Some("Choose Launch to start a session.")))
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

    pub fn workspaces_in_sidebar_order(&self) -> Vec<&WorkspaceView> {
        crate::sidebar_content::repository_groups(&self.workspaces)
            .into_iter()
            .flat_map(|(_, indices)| indices)
            .map(|index| &self.workspaces[index])
            .collect()
    }

    /// Select the current workspace's failure first, then advance through failures in sidebar order.
    pub fn next_failed_workspace(&self) -> Option<WorkspaceId> {
        let order = self.workspaces_in_sidebar_order();
        let start = order
            .iter()
            .position(|ws| Some(ws.id) == self.active_workspace_id)
            .map_or(0, |current| {
                let failure = self.failed_runner(order[current]);
                current + usize::from(failure.is_some() && failure == self.focused_pane_id())
            });
        order[start..]
            .iter()
            .chain(&order[..start])
            .find(|ws| self.failed_runner(ws).is_some())
            .map(|ws| ws.id)
    }

    /// Cycles the active workspace forward (`delta > 0`) or backward (`delta < 0`), wrapping cleanly.
    pub fn cycle_workspace(&mut self, delta: isize) -> Option<WorkspaceId> {
        let next = self.adjacent_workspace(delta)?;
        self.select_workspace(next);
        Some(next)
    }

    pub fn adjacent_workspace(&self, delta: isize) -> Option<WorkspaceId> {
        let order = self.workspaces_in_sidebar_order();
        let len = order.len();
        if len == 0 {
            return None;
        }
        let next_idx = match self
            .active_workspace_id
            .and_then(|id| order.iter().position(|ws| ws.id == id))
        {
            Some(idx) => (idx + delta.rem_euclid(len as isize) as usize) % len,
            None if delta < 0 => len - 1,
            None => 0,
        };
        Some(order[next_idx].id)
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
            let next = {
                let mut candidates = self.workspace_panes(ws).filter(|pane| pane.role == role);
                let first = candidates.clone().next().map(|pane| pane.id);
                candidates.find(|pane| pane.id == current);
                candidates.next().map(|pane| pane.id).or(first)
            };
            if let Some(next) = next {
                self.selected_panes.insert((ws.id, role), next);
            }
        }
        self.focused_role = role;
        self.close_search();
    }

    pub fn workspace_panes<'a>(
        &'a self,
        workspace: &'a WorkspaceView,
    ) -> impl DoubleEndedIterator<Item = &'a PaneSummary> + Clone {
        workspace
            .panes
            .iter()
            .filter_map(|id| self.panes.get(id).map(|pane| &pane.summary))
    }

    /// Prioritizes any live agent awaiting input over the latest agent's state.
    pub fn workspace_agent_state(&self, ws: &WorkspaceView) -> Option<AgentState> {
        let mut agents = self
            .workspace_panes(ws)
            .rev()
            .filter(|pane| pane.role == PaneRole::Agent);
        if agents.clone().any(|pane| {
            !matches!(pane.status, PaneStatus::Exited { .. })
                && pane.agent_state == Some(AgentState::WaitingForApproval)
        }) {
            return Some(AgentState::WaitingForApproval);
        }
        agents
            .next()?
            .agent_state
            .filter(|state| *state != AgentState::WaitingForApproval)
    }

    pub fn apply_agent_state(
        &mut self,
        _workspace_id: WorkspaceId,
        pane_id: PaneId,
        state: AgentState,
    ) {
        if let Some(pane) = self.panes.get_mut(&pane_id) {
            pane.summary.agent_state = Some(state);
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

    pub fn active_test_workspace(&self) -> Option<WorkspaceId> {
        self.active_workspace_id.filter(|id| {
            self.connection == ConnectionState::Connected && self.test_workspaces.contains(id)
        })
    }

    pub fn active_workspace(&self) -> Option<&WorkspaceView> {
        let id = self.active_workspace_id?;
        self.workspaces.iter().find(|ws| ws.id == id)
    }

    pub fn active_actions(&self) -> &[daedalus_proto::CustomAction] {
        self.active_workspace()
            .and_then(|ws| ws.manifest.as_ref())
            .map(|m| m.actions.as_slice())
            .unwrap_or_default()
    }

    pub fn palette_len(&self) -> usize {
        self.action_palette
            .session_actions
            .as_ref()
            .map_or_else(|| self.active_actions().len(), Vec::len)
    }

    pub fn open_action_palette(&mut self) {
        self.picker.close();
        self.worktree_modal.close();
        self.close_modal.close();
        self.close_search();
        self.action_palette.open();
    }

    pub fn active_workspace_mut(&mut self) -> Option<&mut WorkspaceView> {
        let id = self.active_workspace_id?;
        self.workspaces.iter_mut().find(|ws| ws.id == id)
    }

    pub fn pane_id_for_role(&self, role: PaneRole) -> Option<PaneId> {
        let ws = self.active_workspace()?;
        if let Some(id) = self.selected_panes.get(&(ws.id, role))
            && self
                .workspace_panes(ws)
                .any(|pane| pane.id == *id && pane.role == role)
        {
            return Some(*id);
        }
        self.workspace_panes(ws)
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

    pub fn recap_rows(&self, ws: &WorkspaceView) -> Vec<crate::recap::RecapEntry> {
        let mut rows = self.recap.entries(ws.id);
        if rows.iter().any(|row| row.pane.is_some()) {
            let tabs = self.terminal_tabs(ws);
            for row in &mut rows {
                if let Some(id) = row.pane {
                    if let Some((_, title)) = tabs.iter().find(|(pane, _)| *pane == id) {
                        row.label = format!("{title} · {}", row.label);
                    } else {
                        row.pane = None;
                    }
                }
            }
        }
        rows
    }

    pub fn focus_recap_row(&mut self, row: &crate::recap::RecapEntry) -> bool {
        let Some(ws) = self.active_workspace() else {
            return false;
        };
        let Some(id) = row.pane else { return false };
        if !self.recap_rows(ws).contains(row) {
            return false;
        }
        let Some(role) = self
            .workspace_panes(ws)
            .find(|pane| pane.id == id)
            .map(|pane| pane.role)
        else {
            return false;
        };
        self.select_session(ws.id, role, id);
        self.recap.keyboard_row = None;
        true
    }

    /// Keep role ordinals in workspace order, including panes with a custom name.
    pub fn terminal_tabs(&self, ws: &WorkspaceView) -> Vec<(PaneId, String)> {
        let base_name = |pane: &PaneSummary| match pane.role {
            PaneRole::Agent => match self.panes.get(&pane.id).and_then(|p| p.agent_preset) {
                Some(AgentPreset::Claude) => "Claude",
                Some(AgentPreset::Codex) => "Codex",
                Some(AgentPreset::Agy) => "Antigravity",
                _ => "Agent",
            },
            PaneRole::Service => "Service",
            PaneRole::Runner => self
                .panes
                .get(&pane.id)
                .and_then(|view| view.cargo_preset)
                .map_or("Runner", crate::assignments::CargoPreset::label),
        };
        let mut totals = HashMap::new();
        let mut ordinals = HashMap::new();
        for pane in self.workspace_panes(ws) {
            *totals.entry(base_name(pane)).or_insert(0usize) += 1;
        }
        self.workspace_panes(ws)
            .map(|pane| {
                let base = base_name(pane);
                let ordinal = ordinals.entry(base).or_insert(0usize);
                *ordinal += 1;
                let name = self
                    .session_names
                    .get(&pane.id)
                    .cloned()
                    .unwrap_or_else(|| {
                        if totals[base] > 1 {
                            format!("{base} {ordinal}")
                        } else {
                            base.to_owned()
                        }
                    });
                (pane.id, terminal_title(name, pane.status))
            })
            .collect()
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
        let mut adjacent = None;
        for ws in &mut self.workspaces {
            if let Some(index) = ws.panes.iter().position(|pane| *pane == id) {
                ws.panes.remove(index);
                if focused && self.zoomed {
                    // Keep the next tab under the same pointer; at the end, step left.
                    adjacent = ws
                        .panes
                        .get(index)
                        .or_else(|| ws.panes.last())
                        .and_then(|id| self.panes.get(id))
                        .map(|pane| (ws.id, pane.summary.role, pane.summary.id));
                }
                break;
            }
        }
        if let Some((workspace, role, pane)) = adjacent {
            self.select_session(workspace, role, pane);
        } else if focused {
            self.focus_available_session();
        }
    }

    fn focus_available_session(&mut self) {
        if self.focused_pane_id().is_none()
            && let Some(pane) = self
                .active_workspace()
                .and_then(|ws| self.workspace_panes(ws).next())
        {
            self.focused_role = pane.role;
        }
    }

    pub fn select_role_pane(&mut self, workspace: WorkspaceId, role: PaneRole, pane: PaneId) {
        self.selected_panes.insert((workspace, role), pane);
    }

    pub fn select_session(&mut self, workspace: WorkspaceId, role: PaneRole, pane: PaneId) {
        self.select_role_pane(workspace, role, pane);
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
        let agents: Vec<_> = self
            .workspace_panes(ws)
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
        if !self.workspace_panes(ws).any(|pane| {
            pane.role == PaneRole::Runner && pane.id == choices.runner && pane.has_failure_trace
        }) {
            return Err("The original Runner failure is no longer available");
        }
        choices
            .agents
            .get(choices.selected)
            .copied()
            .filter(|id| {
                self.workspace_panes(ws).any(|pane| {
                    pane.id == *id
                        && pane.role == PaneRole::Agent
                        && !matches!(pane.status, PaneStatus::Exited { .. })
                })
            })
            .ok_or("The selected agent is no longer available")
    }

    pub(crate) fn recipient_name(&self, agent: PaneId) -> &str {
        self.session_names
            .get(&agent)
            .map_or("Agent", String::as_str)
    }

    pub fn failure_recipient_label(&self, agent: PaneId) -> String {
        format!("{} · {}", self.recipient_name(agent), agent.short())
    }

    /// The last-listed runner whose latest known result failed, across all panes in the workspace.
    pub fn failed_runner(&self, ws: &WorkspaceView) -> Option<PaneId> {
        self.workspace_panes(ws)
            .rev()
            .filter(|pane| pane.role == PaneRole::Runner)
            .find(|pane| {
                let code = match pane.status {
                    PaneStatus::Exited { code: Some(code) } => Some(code),
                    _ => pane.last_exit_code,
                };
                // The daemon can retain a historical trace after a successful run.
                code.map_or(pane.has_failure_trace, |code| code != 0)
            })
            .map(|pane| pane.id)
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
        self.workspace_list_loaded = true;
        let previous_focus = self.focused_pane_id();
        let previously_empty = self
            .active_workspace()
            .is_some_and(|workspace| workspace.panes.is_empty());
        let workspaces: Vec<_> = workspaces
            .into_iter()
            .map(|ws| self.cache_workspace(ws))
            .collect();
        let live_panes: HashSet<_> = workspaces
            .iter()
            .flat_map(|ws| ws.panes.iter().copied())
            .collect();
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
        self.test_workspaces
            .retain(|id| workspaces.iter().any(|ws| ws.id == *id));
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
        let active = self
            .active_workspace_id
            .filter(|id| self.workspaces.iter().any(|workspace| workspace.id == *id));
        if let Some(workspace_id) = active {
            if let Some(pane_id) = previous_focus.filter(|id| live_panes.contains(id)) {
                // Keep the existing focus even if the user never explicitly selected
                // this role's tab and the daemon appended a sibling.
                self.select_role_pane(workspace_id, self.focused_role, pane_id);
            } else if previous_focus.is_some() || previously_empty {
                self.focus_available_session();
            }
        } else {
            self.settings.open = false;
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
        let ws = self.cache_workspace(ws);
        self.panes
            .retain(|id, pane| pane.summary.workspace_id != ws.id || ws.panes.contains(id));
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

    fn cache_workspace(&mut self, workspace: WorkspaceInfo) -> WorkspaceView {
        workspace.map_panes(|summary| self.cache_pane(summary).summary.id)
    }

    fn cache_pane(&mut self, mut summary: PaneSummary) -> &mut PaneViewState {
        use std::collections::hash_map::Entry;
        match self.panes.entry(summary.id) {
            Entry::Vacant(entry) => entry.insert(PaneViewState::from_summary(summary)),
            Entry::Occupied(entry) => {
                let view = entry.into_mut();
                // A list response has no grid sequence. Keep dimensions paired with the
                // displayed grid until a sequenced snapshot or delta replaces them.
                if view.grid_received {
                    summary.cols = view.summary.cols;
                    summary.rows = view.summary.rows;
                }
                if summary.role == PaneRole::Runner
                    && let Some(preset) = summary
                        .launch_preset
                        .as_deref()
                        .and_then(crate::assignments::CargoPreset::from_launch_tag)
                {
                    view.cargo_preset = Some(preset);
                }
                if let Some(preset) = PaneViewState::agent_launch_preset(&summary) {
                    view.agent_preset = Some(preset);
                }
                view.summary = summary;
                view
            }
        }
    }

    /// Records the new pane in workspace order and preserves an existing terminal grid.
    pub fn register_pane(&mut self, summary: PaneSummary, select_tab: bool) {
        if let Some(ws) = self
            .workspaces
            .iter_mut()
            .find(|ws| ws.id == summary.workspace_id)
            && !ws.panes.contains(&summary.id)
        {
            ws.panes.push(summary.id);
        }
        if select_tab {
            self.select_role_pane(summary.workspace_id, summary.role, summary.id);
        }
        self.cache_pane(summary);
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
                        pane.summary.cwd = cwd;
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
            ServerEvent::ProjectSetupChanged { .. }
            | ServerEvent::WorkbenchChanged { .. }
            | ServerEvent::WorkspaceMembershipChanged => None,
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
mod test_support;
#[cfg(test)]
mod tests;
