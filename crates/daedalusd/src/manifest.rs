use daedalus_proto::{CustomAction, PaneRole, WorkspaceManifest};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::io::ErrorKind;
use std::path::Path;
use tracing::warn;

pub const MANIFEST_FILENAME: &str = ".daedalus.toml";
pub const MAX_MANIFEST_ACTIONS: usize = 32;

/// Parsed project manifest loaded from `<root_dir>/.daedalus.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LoadedManifest {
    pub workspace_manifest: WorkspaceManifest,
    pub extra_ports: Vec<u16>,
    pub env: HashMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawManifest {
    #[serde(default)]
    agent: Option<RawAgentSection>,
    #[serde(default)]
    service: Option<RawCommandSection>,
    #[serde(default)]
    runner: Option<RawCommandSection>,
    #[serde(default)]
    ports: Option<RawPortsSection>,
    #[serde(default)]
    env: HashMap<String, String>,
    #[serde(default)]
    actions: Vec<RawActionEntry>,
}

#[derive(Debug, Default, Deserialize)]
struct RawActionEntry {
    #[serde(default)]
    id: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    target: String,
    #[serde(default)]
    command: String,
    #[serde(default)]
    shortcut: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawAgentSection {
    #[serde(default)]
    preset: Option<String>,
    #[serde(default)]
    command: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
struct RawCommandSection {
    #[serde(default)]
    command: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
struct RawPortsSection {
    #[serde(default)]
    extra: Vec<u16>,
}

fn normalize_command(command: Option<Vec<String>>) -> Option<Vec<String>> {
    command.filter(|cmd| !cmd.is_empty() && !cmd[0].trim().is_empty())
}

fn normalize_preset(preset: Option<String>) -> Option<String> {
    preset
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn parse_action_target(raw: &str) -> Option<PaneRole> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "agent" => Some(PaneRole::Agent),
        "service" => Some(PaneRole::Service),
        "runner" => Some(PaneRole::Runner),
        _ => None,
    }
}

fn normalize_actions(raw_actions: Vec<RawActionEntry>) -> Vec<CustomAction> {
    let mut out = Vec::new();
    let mut seen_ids = HashSet::new();

    for raw in raw_actions {
        if out.len() >= MAX_MANIFEST_ACTIONS {
            break;
        }
        let id = raw.id.trim().to_string();
        let label = raw.label.trim().to_string();
        let command = raw.command.trim().to_string();
        if id.is_empty() || label.is_empty() || command.is_empty() {
            continue;
        }
        let Some(target) = parse_action_target(&raw.target) else {
            continue;
        };
        if !seen_ids.insert(id.clone()) {
            continue;
        }
        let shortcut = raw
            .shortcut
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        out.push(CustomAction {
            id,
            label,
            target,
            command,
            shortcut,
        });
    }

    out
}

/// Parses `.daedalus.toml` contents into a [`LoadedManifest`].
pub fn parse_project_manifest(contents: &str) -> Result<LoadedManifest, toml::de::Error> {
    let raw: RawManifest = toml::from_str(contents)?;
    let agent = raw.agent.unwrap_or_default();
    let service = raw.service.unwrap_or_default();
    let runner = raw.runner.unwrap_or_default();
    let ports = raw.ports.unwrap_or_default();
    let actions = normalize_actions(raw.actions);

    Ok(LoadedManifest {
        workspace_manifest: WorkspaceManifest {
            agent_preset: normalize_preset(agent.preset),
            agent_command: normalize_command(agent.command),
            service_command: normalize_command(service.command),
            runner_command: normalize_command(runner.command),
            actions,
        },
        extra_ports: ports.extra.into_iter().filter(|&p| p > 0).collect(),
        env: raw.env,
    })
}

/// Loads optional `<root_dir>/.daedalus.toml`.
///
/// Missing files return `None` silently. Unreadable or malformed manifests log a
/// warning via `tracing::warn!` and fall back cleanly to `None`.
pub fn load_project_manifest(root_dir: &Path) -> Option<LoadedManifest> {
    let path = root_dir.join(MANIFEST_FILENAME);
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(err) if err.kind() == ErrorKind::NotFound => return None,
        Err(err) => {
            warn!(
                path = %path.display(),
                error = %err,
                "failed to read project manifest; ignoring .daedalus.toml"
            );
            return None;
        }
    };

    match parse_project_manifest(&contents) {
        Ok(manifest) => Some(manifest),
        Err(err) => {
            warn!(
                path = %path.display(),
                error = %err,
                "malformed project manifest; ignoring .daedalus.toml"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn loads_full_valid_manifest() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join(".daedalus.toml"),
            r#"
[agent]
preset = "agy"
command = ["agy", "--fast"]

[service]
command = ["docker", "compose", "up"]

[runner]
command = ["cargo", "test", "--workspace"]

[ports]
extra = [8080, 9000]

[env]
RUST_LOG = "info"
API_BASE = "http://localhost:8080"
"#,
        )
        .unwrap();

        let loaded = load_project_manifest(dir.path()).expect("manifest should load");
        assert_eq!(
            loaded.workspace_manifest,
            WorkspaceManifest {
                agent_preset: Some("agy".to_string()),
                agent_command: Some(vec!["agy".to_string(), "--fast".to_string()]),
                service_command: Some(vec![
                    "docker".to_string(),
                    "compose".to_string(),
                    "up".to_string(),
                ]),
                runner_command: Some(vec![
                    "cargo".to_string(),
                    "test".to_string(),
                    "--workspace".to_string(),
                ]),
                actions: Vec::new(),
            }
        );
        assert_eq!(loaded.extra_ports, vec![8080, 9000]);
        assert_eq!(loaded.env.get("RUST_LOG").map(String::as_str), Some("info"));
        assert_eq!(
            loaded.env.get("API_BASE").map(String::as_str),
            Some("http://localhost:8080")
        );
    }

    #[test]
    fn returns_none_when_manifest_is_missing() {
        let dir = tempdir().unwrap();
        assert_eq!(load_project_manifest(dir.path()), None);
    }

    #[test]
    fn returns_none_on_malformed_toml_without_crashing() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join(".daedalus.toml"),
            "[agent\npreset = \"agy\"",
        )
        .unwrap();
        assert_eq!(load_project_manifest(dir.path()), None);

        std::fs::write(
            dir.path().join(".daedalus.toml"),
            "[ports]\nextra = \"not-a-list\"\n",
        )
        .unwrap();
        assert_eq!(load_project_manifest(dir.path()), None);
    }

    #[test]
    fn handles_partial_manifest_and_filters_empty_commands_and_zero_ports() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join(".daedalus.toml"),
            r#"
[agent]
preset = "  "
command = []

[ports]
extra = [0, 7070]
"#,
        )
        .unwrap();

        let loaded = load_project_manifest(dir.path()).expect("partial manifest should load");
        assert_eq!(loaded.workspace_manifest, WorkspaceManifest::default());
        assert_eq!(loaded.extra_ports, vec![7070]);
        assert!(loaded.env.is_empty());
    }

    #[test]
    fn parses_valid_actions_and_trims_fields() {
        let loaded = parse_project_manifest(
            r#"
[[actions]]
id = "  test-all  "
label = "  Run Tests  "
target = " Runner "
command = "  cargo test --workspace  "
shortcut = "  Ctrl+T  "

[[actions]]
id = "restart-svc"
label = "Restart Service"
target = "service"
command = "docker compose restart"

[[actions]]
id = "ask-agent"
label = "Summarize"
target = "AGENT"
command = "/summarize"
shortcut = "   "
"#,
        )
        .expect("manifest with actions should parse");

        assert_eq!(
            loaded.workspace_manifest.actions,
            vec![
                CustomAction {
                    id: "test-all".to_string(),
                    label: "Run Tests".to_string(),
                    target: PaneRole::Runner,
                    command: "cargo test --workspace".to_string(),
                    shortcut: Some("Ctrl+T".to_string()),
                },
                CustomAction {
                    id: "restart-svc".to_string(),
                    label: "Restart Service".to_string(),
                    target: PaneRole::Service,
                    command: "docker compose restart".to_string(),
                    shortcut: None,
                },
                CustomAction {
                    id: "ask-agent".to_string(),
                    label: "Summarize".to_string(),
                    target: PaneRole::Agent,
                    command: "/summarize".to_string(),
                    shortcut: None,
                },
            ]
        );
    }

    #[test]
    fn deduplicates_action_ids_and_skips_invalid_entries() {
        let loaded = parse_project_manifest(
            r#"
[[actions]]
id = ""
label = "Missing ID"
target = "runner"
command = "echo nope"

[[actions]]
id = "blank-label"
label = "   "
target = "runner"
command = "echo nope"

[[actions]]
id = "blank-cmd"
label = "Blank Command"
target = "runner"
command = ""

[[actions]]
id = "bad-target"
label = "Unknown Target"
target = "shell"
command = "echo nope"

[[actions]]
id = "dup"
label = "First"
target = "runner"
command = "echo first"

[[actions]]
id = "dup"
label = "Second"
target = "service"
command = "echo second"
"#,
        )
        .expect("manifest should skip invalid actions without failing");

        assert_eq!(
            loaded.workspace_manifest.actions,
            vec![CustomAction {
                id: "dup".to_string(),
                label: "First".to_string(),
                target: PaneRole::Runner,
                command: "echo first".to_string(),
                shortcut: None,
            }]
        );
    }

    #[test]
    fn caps_actions_at_max_manifest_actions() {
        let mut toml_str = String::new();
        for i in 0..(MAX_MANIFEST_ACTIONS + 8) {
            toml_str.push_str(&format!(
                "[[actions]]\nid = \"act-{i}\"\nlabel = \"Action {i}\"\ntarget = \"runner\"\ncommand = \"echo {i}\"\n\n"
            ));
        }

        let loaded = parse_project_manifest(&toml_str).expect("manifest should parse");
        assert_eq!(
            loaded.workspace_manifest.actions.len(),
            MAX_MANIFEST_ACTIONS
        );
        assert_eq!(loaded.workspace_manifest.actions[0].id, "act-0");
        assert_eq!(
            loaded.workspace_manifest.actions[MAX_MANIFEST_ACTIONS - 1].id,
            format!("act-{}", MAX_MANIFEST_ACTIONS - 1)
        );
    }
}
