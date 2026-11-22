use daedalus_proto::WorkspaceManifest;
use serde::Deserialize;
use std::collections::HashMap;
use std::io::ErrorKind;
use std::path::Path;
use tracing::warn;

pub const MANIFEST_FILENAME: &str = ".daedalus.toml";

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

/// Parses `.daedalus.toml` contents into a [`LoadedManifest`].
pub fn parse_project_manifest(contents: &str) -> Result<LoadedManifest, toml::de::Error> {
    let raw: RawManifest = toml::from_str(contents)?;
    let agent = raw.agent.unwrap_or_default();
    let service = raw.service.unwrap_or_default();
    let runner = raw.runner.unwrap_or_default();
    let ports = raw.ports.unwrap_or_default();

    Ok(LoadedManifest {
        workspace_manifest: WorkspaceManifest {
            agent_preset: normalize_preset(agent.preset),
            agent_command: normalize_command(agent.command),
            service_command: normalize_command(service.command),
            runner_command: normalize_command(runner.command),
            actions: Vec::new(),
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
}
