use crate::{Assignment, PaneRole, StatusLevel, WidgetConfig};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const WORKSPACE_FORMAT_VERSION: u32 = 1;
pub const SHARED_WORKSPACE_FILE: &str = ".daedalus-workspace.toml";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkbenchState {
    pub revision: u64,
    pub document: WorkspaceDocument,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_baseline: Option<WorkspaceDocument>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WidgetLibrary {
    pub revision: u64,
    pub templates: Vec<WidgetTemplate>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictChoice {
    Local,
    Incoming,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeConflict {
    pub path: String,
    pub base: Option<String>,
    pub local: Option<String>,
    pub incoming: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspaceImportSource {
    Text(String),
    Repo,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceImportPreview {
    pub workspace_id: crate::WorkspaceId,
    pub revision: u64,
    pub incoming: WorkspaceDocument,
    pub current: WorkspaceDocument,
    pub proposed: WorkspaceDocument,
    pub linked: bool,
    pub conflicts: Vec<MergeConflict>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceCreationPreview {
    pub root_dir: std::path::PathBuf,
    pub incoming: WorkspaceDocument,
    pub linked: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceDocument {
    pub version: u32,
    pub name: String,
    #[serde(default)]
    pub assignments: Vec<Assignment>,
    #[serde(default)]
    pub layout: WorkbenchLayout,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_widgets: Option<PaneWidgets>,
    #[serde(default)]
    pub templates: Vec<WidgetTemplate>,
    #[serde(default)]
    pub widgets: Vec<WidgetInstance>,
}

impl WorkspaceDocument {
    pub fn new(name: String) -> Self {
        Self {
            version: WORKSPACE_FORMAT_VERSION,
            name,
            assignments: Vec::new(),
            layout: WorkbenchLayout::default(),
            pane_widgets: None,
            templates: Vec::new(),
            widgets: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PaneWidgets {
    pub service: WidgetConfig,
    pub runner: WidgetConfig,
    pub service_enabled: Option<bool>,
    pub runner_enabled: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkbenchLayout {
    pub split_view: bool,
    /// Split proportions in thousandths, keyed by the desktop split name.
    pub splits: BTreeMap<String, u16>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateDocument {
    pub version: u32,
    pub template: WidgetTemplate,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WidgetTemplate {
    pub id: String,
    pub name: String,
    pub definition: WidgetDefinition,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WidgetInstance {
    pub id: String,
    pub name: String,
    pub pinned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition: Option<WidgetDefinition>,
}

impl WorkspaceDocument {
    pub fn widget_definition<'a>(
        &'a self,
        widget: &'a WidgetInstance,
    ) -> Option<&'a WidgetDefinition> {
        if let Some(definition) = &widget.definition {
            return Some(definition);
        }
        let template = widget.template.as_deref()?;
        self.templates
            .iter()
            .find(|candidate| candidate.id == template)
            .map(|candidate| &candidate.definition)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WidgetDefinition {
    pub sources: Vec<NamedSource>,
    pub display: WidgetDisplay,
    pub rules: Vec<WidgetRule>,
    pub actions: Vec<NamedAction>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedSource {
    pub id: String,
    #[serde(flatten)]
    pub source: WidgetSource,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WidgetSource {
    Git,
    Port {
        port: Option<u16>,
        service: Option<String>,
    },
    Container {
        name: Option<String>,
    },
    Runner {
        assignment: Option<String>,
    },
    Published {
        key: String,
    },
    Command {
        command: WorkspaceCommand,
        #[serde(default)]
        refresh: RefreshPolicy,
        #[serde(default)]
        output: CommandOutput,
    },
    /// Preserves summary behavior for widgets migrated from role-based settings.
    Pane {
        role: PaneRole,
        config: WidgetConfig,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceCommand {
    pub argv: Vec<String>,
    #[serde(default)]
    pub env: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RefreshPolicy {
    pub interval_seconds: Option<u32>,
    pub on_runner_completion: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommandOutput {
    #[default]
    ExitStatus,
    Text {
        contains: Option<String>,
    },
    Json,
    WidgetResult,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WidgetDisplay {
    pub icon: Option<String>,
    pub summary: Vec<WidgetField>,
    pub details: Vec<WidgetField>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WidgetField {
    pub source: String,
    pub field: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WidgetRule {
    pub field: WidgetField,
    pub condition: WidgetCondition,
    pub level: StatusLevel,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WidgetCondition {
    Equals { value: String },
    Contains { value: String },
    GreaterThan { value: i64 },
    Missing,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedAction {
    pub id: String,
    pub label: String,
    #[serde(flatten)]
    pub action: WidgetAction,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WidgetAction {
    Diff,
    Terminal { role: PaneRole },
    Logs { role: PaneRole },
    OpenUrl { url: String },
    Command { command: WorkspaceCommand },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WidgetResult {
    pub label: String,
    pub level: StatusLevel,
    #[serde(default)]
    pub details: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WidgetSnapshot {
    pub id: String,
    pub result: WidgetResult,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WidgetActionResult {
    Pane(crate::PaneId),
    OpenUrl(String),
    Output(WidgetResult),
}
