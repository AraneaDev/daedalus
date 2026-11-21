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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub id: WorkspaceId,
    pub name: String,
    pub root_dir: PathBuf,
    pub slot_index: u16,
    pub injected_env: HashMap<String, String>,
    pub panes: Vec<PaneSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureTrace {
    pub pane_id: PaneId,
    pub command: Option<String>,
    pub exit_code: i32,
    pub cwd: Option<PathBuf>,
    pub output: String,
    pub captured_at_ms: u64,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridDelta {
    pub pane_id: PaneId,
    pub seq: u64,
    pub cols: u16,
    pub rows: u16,
    pub cursor: CursorState,
    pub dirty_rows: Vec<GridRow>,
}

/// Wraps input bytes in XTerm bracketed paste markers (`ESC [ 200 ~` ... `ESC [ 201 ~`)
/// so interactive CLI agents (`claude`, `codex`, `agy`) receive multi-line prompts or
/// failure traces without premature newline submission.
pub fn wrap_bracketed_paste(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 12);
    out.extend_from_slice(b"\x1b[200~");
    out.extend_from_slice(data);
    out.extend_from_slice(b"\x1b[201~");
    out
}
