use bitflags::bitflags;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorkspaceId(pub Uuid);

impl WorkspaceId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn short(&self) -> String {
        self.0.simple().to_string()[..8].to_string()
    }
}

impl Default for WorkspaceId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for WorkspaceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PaneId(pub Uuid);

impl PaneId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn short(&self) -> String {
        self.0.simple().to_string()[..8].to_string()
    }
}

impl Default for PaneId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for PaneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneRole {
    Agent,
    Service,
    Runner,
}

impl fmt::Display for PaneRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Agent => write!(f, "agent"),
            Self::Service => write!(f, "service"),
            Self::Runner => write!(f, "runner"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneStatus {
    Starting,
    Idle,
    Busy,
    Exited { code: Option<i32> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    Idle,
    Thinking,
    WaitingForApproval,
    Error,
}

impl AgentState {
    pub fn from_cli_name(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "idle" => Some(Self::Idle),
            "thinking" => Some(Self::Thinking),
            "waiting" | "approval" | "waiting_for_approval" | "waiting-for-approval" => {
                Some(Self::WaitingForApproval)
            }
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

impl fmt::Display for AgentState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Idle => write!(f, "idle"),
            Self::Thinking => write!(f, "thinking"),
            Self::WaitingForApproval => write!(f, "waiting_for_approval"),
            Self::Error => write!(f, "error"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSummary {
    pub id: PaneId,
    pub workspace_id: WorkspaceId,
    pub role: PaneRole,
    pub status: PaneStatus,
    pub cols: u16,
    pub rows: u16,
    pub cwd: Option<PathBuf>,
    pub last_exit_code: Option<i32>,
    pub has_failure_trace: bool,
    #[serde(default)]
    pub agent_state: Option<AgentState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitWorktreeInfo {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub head: String,
    pub is_primary: bool,
    pub is_detached: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GitWorkspaceStatus {
    pub is_worktree: bool,
    pub dirty: bool,
    pub ahead: u32,
    pub behind: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortSource {
    Docker,
    LocalProcess,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortMapping {
    pub host_port: u16,
    pub container_port: Option<u16>,
    pub service: Option<String>,
    pub source: PortSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerHealth {
    pub name: String,
    pub service: Option<String>,
    pub state: String,
    pub health: Option<String>,
    pub ports: Vec<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusLevel {
    Info,
    Ok,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalStatus {
    pub source: String,
    pub level: StatusLevel,
    pub label: String,
    pub detail: Option<String>,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomAction {
    pub id: String,
    pub label: String,
    pub target: PaneRole,
    pub command: String,
    #[serde(default)]
    pub shortcut: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WorkspaceManifest {
    #[serde(default)]
    pub agent_preset: Option<String>,
    #[serde(default)]
    pub agent_command: Option<Vec<String>>,
    #[serde(default)]
    pub service_command: Option<Vec<String>>,
    #[serde(default)]
    pub runner_command: Option<Vec<String>>,
    #[serde(default)]
    pub actions: Vec<CustomAction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub id: WorkspaceId,
    pub name: String,
    pub root_dir: PathBuf,
    pub slot_index: u16,
    pub injected_env: HashMap<String, String>,
    pub panes: Vec<PaneSummary>,
    #[serde(default)]
    pub port_offset: u16,
    #[serde(default)]
    pub compose_project_name: Option<String>,
    #[serde(default)]
    pub git_branch: Option<String>,
    #[serde(default)]
    pub git_status: Option<GitWorkspaceStatus>,
    #[serde(default)]
    pub bound_ports: Vec<PortMapping>,
    #[serde(default)]
    pub containers: Vec<ContainerHealth>,
    #[serde(default)]
    pub external_statuses: Vec<ExternalStatus>,
    #[serde(default)]
    pub manifest: Option<WorkspaceManifest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureTrace {
    pub pane_id: PaneId,
    pub command: Option<String>,
    pub exit_code: i32,
    pub cwd: Option<PathBuf>,
    pub output: String,
    pub captured_at_ms: u64,
    #[serde(default)]
    pub git_branch: Option<String>,
    #[serde(default)]
    pub git_diff_stat: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
    pub struct CellFlags: u16 {
        const BOLD          = 1 << 0;
        const ITALIC        = 1 << 1;
        const UNDERLINE     = 1 << 2;
        const INVERSE       = 1 << 3;
        const DIM           = 1 << 4;
        const STRIKETHROUGH = 1 << 5;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell {
    pub text: String,
    pub fg: Color,
    pub bg: Color,
    pub flags: CellFlags,
    pub width: u8,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            text: " ".to_string(),
            fg: Color::Default,
            bg: Color::Default,
            flags: CellFlags::empty(),
            width: 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridRow {
    pub row_index: u16,
    pub abs_row: u64,
    pub cells: Vec<Cell>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CursorState {
    pub col: u16,
    pub row: u16,
    pub visible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridSnapshot {
    pub pane_id: PaneId,
    pub seq: u64,
    pub cols: u16,
    pub rows: u16,
    pub cursor: CursorState,
    pub rows_data: Vec<GridRow>,
    #[serde(default)]
    pub scrollback_len: usize,
    /// Changes when the terminal erases its saved lines.
    #[serde(default)]
    pub history_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridDelta {
    pub pane_id: PaneId,
    pub seq: u64,
    pub cols: u16,
    pub rows: u16,
    pub cursor: CursorState,
    pub dirty_rows: Vec<GridRow>,
    #[serde(default)]
    pub history_generation: u64,
    /// Older daemons omit history length in incremental updates.
    #[serde(default)]
    pub scrollback_len: Option<usize>,
}

/// A single substring match in a pane's scrollback/viewport history (`end_col` is exclusive).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchMatch {
    pub abs_row: u64,
    pub start_col: u16,
    pub end_col: u16,
}

const BRACKETED_PASTE_FORBIDDEN: &[&[u8]] = &[b"\x1b[201~", b"\x1b[200~", b"\x9b201~", b"\x9b200~"];

fn strip_bracketed_paste_markers(data: &[u8]) -> Vec<u8> {
    let mut current = data.to_vec();
    loop {
        let mut next = Vec::with_capacity(current.len());
        let mut i = 0;
        let mut removed = false;
        while i < current.len() {
            if let Some(marker) = BRACKETED_PASTE_FORBIDDEN
                .iter()
                .find(|m| current[i..].starts_with(m))
            {
                i += marker.len();
                removed = true;
            } else {
                next.push(current[i]);
                i += 1;
            }
        }
        if !removed {
            return next;
        }
        current = next;
    }
}

/// Wraps input bytes in XTerm bracketed paste markers (`ESC [ 200 ~` ... `ESC [ 201 ~`)
/// so interactive CLI agents (`claude`, `codex`, `agy`) receive multi-line prompts or
/// failure traces without premature newline submission.
///
/// Any embedded 7-bit or 8-bit bracketed paste markers are iteratively stripped first
/// to prevent breakout / command injection.
pub fn wrap_bracketed_paste(data: &[u8]) -> Vec<u8> {
    let sanitized = strip_bracketed_paste_markers(data);
    let mut out = Vec::with_capacity(sanitized.len() + 12);
    out.extend_from_slice(b"\x1b[200~");
    out.extend_from_slice(&sanitized);
    out.extend_from_slice(b"\x1b[201~");
    out
}
