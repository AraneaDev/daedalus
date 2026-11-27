use crate::types::PaneRole;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Runtime meaning of a manifest role, before applying a consumer's pane policy.
#[derive(Debug, PartialEq, Eq)]
pub enum ManifestTarget {
    DefaultShell,
    Command(Vec<String>),
    Unconfigured,
}

impl ManifestTarget {
    pub fn into_command(self) -> Option<Vec<String>> {
        match self {
            Self::Command(argv) => Some(argv),
            Self::DefaultShell | Self::Unconfigured => None,
        }
    }

    /// Encode the default shell using the existing portable assignment sentinel.
    pub fn into_assignment(self, role: PaneRole, id: String, name: &str) -> Option<Assignment> {
        let default_shell = self == Self::DefaultShell;
        let (id, argv) = match self {
            Self::DefaultShell => ("manifest-agent".into(), vec!["shell".into()]),
            Self::Command(argv) => (id, argv),
            Self::Unconfigured => return None,
        };
        let mut assignment = Assignment {
            id,
            name: name.into(),
            role,
            argv,
        };
        // A literal command named "shell" has its own portable ID.
        if !default_shell && assignment.uses_default_shell() {
            assignment.id.push_str("-command");
        }
        Some(assignment)
    }
}

pub struct ManifestLaunchTargets {
    pub roles: [(PaneRole, ManifestTarget); 3],
    pub selected_preset: Option<&'static str>,
    pub launch_preset: Option<&'static str>,
}

/// Resolve executable arguments once for provisioning, launch settings and portable export.
/// Export has no machine-local fallback preset.
pub fn manifest_launch_targets(
    manifest: Option<&crate::WorkspaceManifest>,
    fallback_agent: Option<&str>,
) -> ManifestLaunchTargets {
    let declared = manifest
        .and_then(|m| m.agent_preset.as_deref())
        .and_then(crate::canonical_agent_preset);
    let selected_preset =
        declared.or_else(|| fallback_agent.and_then(crate::canonical_agent_preset));
    let command = |argv: Option<&Vec<String>>| argv.filter(|args| !args.is_empty()).cloned();
    let (agent, launch_preset) = match command(manifest.and_then(|m| m.agent_command.as_ref())) {
        Some(argv) => {
            let preset = crate::agent_launch_preset(&argv[0]).or(declared);
            (ManifestTarget::Command(argv), preset)
        }
        None => (
            match selected_preset {
                Some("shell") => ManifestTarget::DefaultShell,
                Some(preset) => ManifestTarget::Command(vec![preset.into()]),
                None => ManifestTarget::Unconfigured,
            },
            selected_preset,
        ),
    };
    let target = |argv| command(argv).map_or(ManifestTarget::Unconfigured, ManifestTarget::Command);
    ManifestLaunchTargets {
        roles: [
            (PaneRole::Agent, agent),
            (
                PaneRole::Service,
                target(manifest.and_then(|m| m.service_command.as_ref())),
            ),
            (
                PaneRole::Runner,
                target(manifest.and_then(|m| m.runner_command.as_ref())),
            ),
        ],
        selected_preset,
        launch_preset,
    }
}

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
