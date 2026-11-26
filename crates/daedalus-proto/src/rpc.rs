use crate::types::{
    AgentState, ContainerHealth, ExternalStatus, FailureTrace, GitWorkspaceStatus, GitWorktreeInfo,
    GridDelta, GridRow, GridSnapshot, PaneId, PaneRole, PaneStatus, PaneSummary, PortMapping,
    SearchMatch, StatusLevel, WorkspaceId, WorkspaceInfo,
};
use crate::workbench::{
    ConflictChoice, WidgetActionResult, WidgetDefinition, WidgetLibrary, WidgetResult,
    WidgetSnapshot, WidgetTemplate, WorkbenchEdit, WorkbenchState, WorkspaceCreationPreview,
    WorkspaceDocument, WorkspaceImportPreview, WorkspaceImportSource,
};
use crate::{ActivityCursor, ActivityPage, Assignment, RecentProject};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImagePreview {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl ImagePreview {
    pub const MAX_EDGE: u32 = 192;

    pub fn valid(&self) -> bool {
        (1..=Self::MAX_EDGE).contains(&self.width)
            && (1..=Self::MAX_EDGE).contains(&self.height)
            && self.rgba.len() == self.width as usize * self.height as usize * 4
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub payload: ClientMessage,
}

/// Read-only discovery on the daemon host. Commands retain their argument boundaries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupInspection {
    pub commands: Vec<(PaneRole, Vec<String>)>,
    pub available: Vec<String>,
    #[serde(default)]
    pub dependency_install: Option<Vec<String>>,
    pub host: String,
    pub platform: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientMessage {
    Ping,
    Authenticate {
        token: String,
    },
    InspectSetup {
        root_dir: PathBuf,
        executables: Vec<String>,
    },
    CreateManualWorkspace {
        name: String,
        root_dir: PathBuf,
    },
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
    WatchWorkspaceMembership,
    ReadActivity {
        workspace_id: WorkspaceId,
        cursor: Option<ActivityCursor>,
    },
    ResolveWorkspaceDirectory {
        root_dir: PathBuf,
    },
    CloseWorkspace {
        id: WorkspaceId,
        #[serde(default)]
        compose_down: bool,
        #[serde(default)]
        remove_worktree: bool,
        #[serde(default)]
        force_remove_worktree: bool,
    },
    SetAgentState {
        pane_id: Option<PaneId>,
        workspace_id: Option<WorkspaceId>,
        root_dir: Option<PathBuf>,
        state: AgentState,
    },
    PublishExternalStatus {
        workspace_id: Option<WorkspaceId>,
        root_dir: Option<PathBuf>,
        source: String,
        level: StatusLevel,
        label: String,
        detail: Option<String>,
    },
    ClearExternalStatus {
        workspace_id: Option<WorkspaceId>,
        root_dir: Option<PathBuf>,
        source: String,
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
    /// Initializes one default role exactly once per workspace lifetime.
    EnsureDefaultPane {
        workspace_id: WorkspaceId,
        role: PaneRole,
        command: Option<Vec<String>>,
        cols: u16,
        rows: u16,
    },
    /// Replaces the expected Agent pane after the new process starts.
    ReplaceAgentPane {
        workspace_id: WorkspaceId,
        old_pane_id: Option<PaneId>,
        command: Option<Vec<String>>,
        cols: u16,
        rows: u16,
    },
    ClosePane {
        pane_id: PaneId,
    },
    SendInput {
        pane_id: PaneId,
        data: Vec<u8>,
    },
    LaunchRunnerAssignment {
        workspace_id: WorkspaceId,
        assignment_id: String,
        cols: u16,
        rows: u16,
    },
    Paste {
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
    SearchPane {
        pane_id: PaneId,
        query: String,
        case_sensitive: bool,
    },
    GetFailureTrace {
        pane_id: PaneId,
    },
    Subscribe {
        pane_ids: Option<Vec<PaneId>>,
        #[serde(default)]
        project_setup_events: bool,
    },
    PreviewImage {
        workspace_id: WorkspaceId,
        path: String,
    },
    ListRecentProjects,
    SetProjectSetup {
        workspace_id: WorkspaceId,
        assignments: Vec<Assignment>,
    },
    Unsubscribe,
    RunWorkspaceAction {
        workspace_id: WorkspaceId,
        action_id: String,
    },
    GetWorkbench {
        workspace_id: WorkspaceId,
    },
    WatchWorkbench {
        workspace_id: WorkspaceId,
    },
    UpdateWorkbench {
        workspace_id: WorkspaceId,
        revision: u64,
        document: WorkspaceDocument,
        /// Preserve all settings outside the layout, including revision-zero defaults.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        layout_only: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        edit: Option<WorkbenchEdit>,
    },
    SeedLegacyWorkbench {
        workspace_id: WorkspaceId,
        document: WorkspaceDocument,
    },
    ListWidgetTemplates,
    UpdateWidgetTemplates {
        revision: u64,
        templates: Vec<WidgetTemplate>,
    },
    PreviewWorkspaceImport {
        workspace_id: WorkspaceId,
        source: WorkspaceImportSource,
        linked: bool,
    },
    ApplyWorkspaceImport {
        workspace_id: WorkspaceId,
        revision: u64,
        incoming: WorkspaceDocument,
        linked: bool,
        resolutions: std::collections::BTreeMap<String, ConflictChoice>,
    },
    ExportWorkspace {
        workspace_id: WorkspaceId,
    },
    PreviewWorkspaceCreation {
        root_dir: PathBuf,
        source: WorkspaceImportSource,
        linked: bool,
    },
    ApplyWorkspaceCreation {
        preview: WorkspaceCreationPreview,
    },
    CheckWorkspaceRepo {
        workspace_id: WorkspaceId,
    },
    DecodeWidgetTemplate {
        text: String,
    },
    EncodeWidgetTemplate {
        template: WidgetTemplate,
    },
    PreviewWidget {
        workspace_id: WorkspaceId,
        name: String,
        definition: WidgetDefinition,
    },
    GetWidgetResults {
        workspace_id: WorkspaceId,
        refresh: Option<String>,
    },
    InvokeWidgetAction {
        workspace_id: WorkspaceId,
        widget: String,
        action: String,
    },
    /// Recompute the proposal with the user's choices without applying it.
    ResolveWorkspaceImport {
        workspace_id: WorkspaceId,
        revision: u64,
        incoming: WorkspaceDocument,
        linked: bool,
        resolutions: std::collections::BTreeMap<String, ConflictChoice>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)]
pub enum ServerMessage {
    Response {
        id: u64,
        result: Result<ResponsePayload, ProtoError>,
    },
    Event(ServerEvent),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)]
pub enum ResponsePayload {
    Pong {
        version: String,
        vt_backend: String,
        isolation_mode: String,
        #[serde(default)]
        journal_supported: bool,
        #[serde(default)]
        inspect_setup_supported: bool,
        #[serde(default)]
        recent_projects_supported: bool,
        #[serde(default)]
        project_setup_supported: bool,
        #[serde(default)]
        image_preview_supported: bool,
        #[serde(default)]
        paste_supported: bool,
        #[serde(default)]
        workbench_supported: bool,
        #[serde(default)]
        workspace_membership_supported: bool,
        #[serde(default)]
        runner_assignment_supported: bool,
        #[serde(default)]
        home_dir: Option<PathBuf>,
        #[serde(default)]
        resolved_import_preview_supported: bool,
        #[serde(default)]
        agent_launch_metadata_supported: bool,
    },
    ImagePreview(ImagePreview),
    RecentProjects(Vec<RecentProject>),
    SetupInspection(SetupInspection),
    Activity(ActivityPage),
    WorkspaceCreated(WorkspaceInfo),
    WorkspaceResolved(WorkspaceInfo),
    Workspaces(Vec<WorkspaceInfo>),
    Worktrees(Vec<GitWorktreeInfo>),
    PaneSpawned(PaneSummary),
    /// An ensure request reused a pane, or found a role the user left closed.
    DefaultPaneExisting(Option<PaneSummary>),
    Snapshot(GridSnapshot),
    ScrollbackRows {
        pane_id: PaneId,
        total_scrollback: usize,
        rows: Vec<GridRow>,
    },
    SearchMatches {
        pane_id: PaneId,
        total_scrollback: usize,
        matches: Vec<SearchMatch>,
    },
    FailureTrace(Option<FailureTrace>),
    Ok,
    Workbench(WorkbenchState),
    WorkbenchConflict(WorkbenchState),
    WidgetLibrary(WidgetLibrary),
    WidgetLibraryConflict(WidgetLibrary),
    WorkspaceImportPreview(WorkspaceImportPreview),
    WorkspaceCreationPreview(WorkspaceCreationPreview),
    WorkbenchCreated(WorkspaceInfo),
    WorkbenchToml(String),
    WorkspaceRepoStatus(Option<bool>),
    WidgetTemplate(WidgetTemplate),
    WidgetResult(WidgetResult),
    WidgetResults(Vec<WidgetSnapshot>),
    WidgetAction(WidgetActionResult),
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
    AgentStateChanged {
        workspace_id: WorkspaceId,
        pane_id: PaneId,
        state: AgentState,
    },
    WorkspaceRuntimeUpdated {
        workspace_id: WorkspaceId,
        bound_ports: Vec<PortMapping>,
        containers: Vec<ContainerHealth>,
        #[serde(default)]
        git_status: Option<GitWorkspaceStatus>,
        #[serde(default)]
        git_branch: Option<String>,
    },
    ExternalStatusUpdated {
        workspace_id: WorkspaceId,
        status: ExternalStatus,
    },
    ProjectSetupChanged {
        root_dir: PathBuf,
        assignments: Vec<Assignment>,
    },
    WorkspaceMembershipChanged,
    WorkbenchChanged {
        workspace_id: WorkspaceId,
        revision: u64,
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
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    #[error("failed to spawn process: {0}")]
    SpawnFailed(String),
    #[error("i/o error: {0}")]
    IoError(String),
    #[error("internal daemon error: {0}")]
    Internal(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pong_without_new_fields_decodes_with_defaults() {
        #[derive(Serialize)]
        enum OldPayload {
            Pong {
                version: String,
                vt_backend: String,
                isolation_mode: String,
            },
        }
        let bytes = rmp_serde::to_vec_named(&OldPayload::Pong {
            version: "0.0.1".into(),
            vt_backend: "vt".into(),
            isolation_mode: "none".into(),
        })
        .unwrap();
        let decoded: ResponsePayload = rmp_serde::from_slice(&bytes).unwrap();
        match decoded {
            ResponsePayload::Pong {
                recent_projects_supported,
                project_setup_supported,
                paste_supported,
                workspace_membership_supported,
                runner_assignment_supported,
                resolved_import_preview_supported,
                agent_launch_metadata_supported,
                home_dir,
                ..
            } => {
                assert!(!recent_projects_supported);
                assert!(!project_setup_supported);
                assert!(!paste_supported);
                assert!(!workspace_membership_supported);
                assert!(!runner_assignment_supported);
                assert!(!resolved_import_preview_supported);
                assert!(!agent_launch_metadata_supported);
                assert_eq!(home_dir, None);
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn subscribe_without_opt_in_defaults_to_no_project_events() {
        #[derive(Serialize)]
        enum OldMessage {
            Subscribe { pane_ids: Option<Vec<PaneId>> },
        }
        let bytes = rmp_serde::to_vec_named(&OldMessage::Subscribe { pane_ids: None }).unwrap();
        let decoded: ClientMessage = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(
            decoded,
            ClientMessage::Subscribe {
                pane_ids: None,
                project_setup_events: false,
            }
        );
    }

    #[test]
    fn workspace_info_without_setup_decodes() {
        #[derive(Serialize)]
        #[serde(rename = "WorkspaceInfo")]
        struct WithoutSetup {
            id: WorkspaceId,
            name: String,
            root_dir: PathBuf,
            slot_index: u16,
            injected_env: HashMap<String, String>,
            panes: Vec<PaneSummary>,
        }
        let stripped = rmp_serde::to_vec_named(&WithoutSetup {
            id: WorkspaceId::new(),
            name: "w".into(),
            root_dir: PathBuf::from("/tmp/w"),
            slot_index: 0,
            injected_env: HashMap::new(),
            panes: Vec::new(),
        })
        .unwrap();
        let decoded: WorkspaceInfo = rmp_serde::from_slice(&stripped).unwrap();
        assert_eq!(decoded.setup, None);
    }

    #[test]
    fn assignment_round_trips() {
        let assignment = Assignment {
            id: "setup-runner".into(),
            name: "Setup".into(),
            role: PaneRole::Runner,
            argv: vec!["make".into(), "test".into()],
        };
        let bytes = rmp_serde::to_vec_named(&assignment).unwrap();
        let decoded: Assignment = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(decoded, assignment);
    }
}
