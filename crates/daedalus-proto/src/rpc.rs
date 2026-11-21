use crate::types::{
    ContainerHealth, ExternalStatus, FailureTrace, GitWorktreeInfo, GridDelta, GridRow,
    GridSnapshot, PaneId, PaneRole, PaneStatus, PaneSummary, PortMapping, StatusLevel, WorkspaceId,
    WorkspaceInfo,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub payload: ClientMessage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientMessage {
    Ping,
    CreateWorkspace {
        name: String,
        root_dir: PathBuf,
        env: HashMap<String, String>,
    },
    DiscoverWorktrees {
        repo_dir: PathBuf,
    },
    CreateWorktreeWorkspace {
        repo_dir: PathBuf,
        branch: String,
        create_branch: bool,
        env: HashMap<String, String>,
    },
    ListWorkspaces,
    CloseWorkspace {
        id: WorkspaceId,
        #[serde(default)]
        compose_down: bool,
    },
    PublishExternalStatus {
        workspace_id: Option<WorkspaceId>,
        root_dir: Option<PathBuf>,
        source: String,
        level: StatusLevel,
        label: String,
        detail: Option<String>,
    },
    SpawnPane {
        workspace_id: WorkspaceId,
        role: PaneRole,
        command: Option<Vec<String>>,
        shell: Option<String>,
        cols: u16,
        rows: u16,
        env: HashMap<String, String>,
    },
    ClosePane {
        pane_id: PaneId,
    },
    SendInput {
        pane_id: PaneId,
        data: Vec<u8>,
    },
    ResizePane {
        pane_id: PaneId,
        cols: u16,
        rows: u16,
    },
    GetSnapshot {
        pane_id: PaneId,
    },
    GetScrollback {
        pane_id: PaneId,
        offset_from_bottom: usize,
        count: usize,
    },
    GetFailureTrace {
        pane_id: PaneId,
    },
    Subscribe {
        pane_ids: Option<Vec<PaneId>>,
    },
    Unsubscribe,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerMessage {
    Response {
        id: u64,
        result: Result<ResponsePayload, ProtoError>,
    },
    Event(ServerEvent),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResponsePayload {
    Pong {
        version: String,
        vt_backend: String,
        isolation_mode: String,
    },
    WorkspaceCreated(WorkspaceInfo),
    Workspaces(Vec<WorkspaceInfo>),
    Worktrees(Vec<GitWorktreeInfo>),
    PaneSpawned(PaneSummary),
    Snapshot(GridSnapshot),
    ScrollbackRows {
        pane_id: PaneId,
        total_scrollback: usize,
        rows: Vec<GridRow>,
    },
    FailureTrace(Option<FailureTrace>),
    Ok,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerEvent {
    GridDelta(GridDelta),
    PaneStatusChanged {
        pane_id: PaneId,
        status: PaneStatus,
        cwd: Option<PathBuf>,
    },
    CommandFinished {
        pane_id: PaneId,
        command: Option<String>,
        exit_code: i32,
        has_failure_trace: bool,
    },
    PaneExited {
        pane_id: PaneId,
        exit_code: Option<i32>,
    },
    ResyncRequired {
        pane_id: PaneId,
    },
    WorkspaceRuntimeUpdated {
        workspace_id: WorkspaceId,
        bound_ports: Vec<PortMapping>,
        containers: Vec<ContainerHealth>,
    },
    ExternalStatusUpdated {
        workspace_id: WorkspaceId,
        status: ExternalStatus,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize)]
pub enum ProtoError {
    #[error("workspace not found: {0}")]
    WorkspaceNotFound(WorkspaceId),
    #[error("pane not found: {0}")]
    PaneNotFound(PaneId),
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("failed to spawn process: {0}")]
    SpawnFailed(String),
    #[error("i/o error: {0}")]
    IoError(String),
    #[error("internal daemon error: {0}")]
    Internal(String),
}
