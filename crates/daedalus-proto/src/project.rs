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

impl Assignment {
    /// Older documents represented the manifest's Shell preset as a synthetic executable.
    pub fn uses_default_shell(&self) -> bool {
        self.role == PaneRole::Agent
            && self
                .id
                .strip_prefix("manifest-agent")
                .is_some_and(|suffix| suffix.chars().all(|ch| ch == '-'))
            && matches!(self.argv.as_slice(), [preset] if crate::canonical_agent_preset(preset) == Some("shell"))
    }
}

/// The saved pane setup the client used as its starting point. An unset setup
/// is distinct from a saved empty list.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSetupBaseline {
    pub assignments: Option<Vec<Assignment>>,
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
