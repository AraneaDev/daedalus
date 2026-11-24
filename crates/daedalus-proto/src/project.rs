use crate::types::PaneRole;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A pane the user assigned to a project: what to run and in which role.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assignment {
    pub id: String,
    pub name: String,
    pub role: PaneRole,
    pub argv: Vec<String>,
}

/// A project the daemon has seen before, offered again when opening a workspace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentProject {
    pub name: String,
    pub root_dir: PathBuf,
    pub last_used: u64,
    pub has_setup: bool,
}
