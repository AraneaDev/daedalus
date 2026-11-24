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

/// Checks the rules every stored setup must satisfy, wherever it is stored.
pub fn validate_assignments(assignments: &[Assignment]) -> Result<(), String> {
    let mut ids = std::collections::HashSet::new();
    for assignment in assignments {
        if assignment.id.trim().is_empty() || !ids.insert(&assignment.id) {
            return Err("Each assignment needs a unique, nonempty ID".into());
        }
        if assignment.name.trim().is_empty() {
            return Err("Each assignment needs a name".into());
        }
        if assignment
            .argv
            .first()
            .is_none_or(|arg| arg.trim().is_empty())
        {
            return Err("Each assignment needs an executable".into());
        }
        if assignment.argv.iter().any(|arg| arg.contains('\0')) {
            return Err("Assignment arguments cannot contain a NUL character".into());
        }
    }
    Ok(())
}

/// A project the daemon has seen before, offered again when opening a workspace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentProject {
    pub name: String,
    pub root_dir: PathBuf,
    pub last_used: u64,
    pub has_setup: bool,
}
