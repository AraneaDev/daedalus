use crate::{isolation, manifest};
use daedalus_proto::{PaneRole, ProtoError, SetupInspection, WorkspaceManifest};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const COMMON_TOOLS: &[&str] = &[
    "codex", "claude", "agy", "sh", "cmd", "pwsh", "docker", "cargo", "npm", "node", "pnpm",
    "yarn", "bun", "composer", "python", "python3", "make", "brew", "winget", "rustup",
];

#[cfg(unix)]
fn executable(path: &Path) -> bool {
    path.is_file() && nix::unistd::access(path, nix::unistd::AccessFlags::X_OK).is_ok()
}

#[cfg(windows)]
fn executable(path: &Path) -> bool {
    let extensions = std::env::var("PATHEXT").unwrap_or_else(|_| ".EXE;.COM;.BAT;.CMD".into());
    if let Some(extension) = path.extension().and_then(|s| s.to_str()) {
        return path.is_file()
            && extensions
                .split(';')
                .any(|ext| ext.trim_start_matches('.').eq_ignore_ascii_case(extension));
    }
    extensions
        .split(';')
        .filter(|ext| !ext.is_empty())
        .any(|ext| path.with_extension(ext.trim_start_matches('.')).is_file())
}

fn available(root: &Path, name: &str, search: &[PathBuf]) -> bool {
    let path = Path::new(name);
    if path.components().count() > 1 || path.is_absolute() {
        return executable(&root.join(path));
    }
    search
        .iter()
        .any(|dir| executable(&root.join(dir).join(name)))
}

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).into()).collect()
}

fn suggestions(root: &Path, manifest: WorkspaceManifest) -> Vec<(PaneRole, Vec<String>)> {
    let compose = isolation::find_compose_file(root).is_some();
    let tools = manifest::detect_tools(root, compose);
    let has = |name: &str| tools.iter().any(|tool| tool == name);
    let package = root.join("package.json");
    let scripts = has("npm")
        .then(|| {
            let metadata = package.metadata().ok()?;
            if !metadata.is_file() || metadata.len() > 2 * 1024 * 1024 {
                return None;
            }
            let json: serde_json::Value =
                serde_json::from_slice(&std::fs::read(package).ok()?).ok()?;
            json.get("scripts")?.as_object().cloned()
        })
        .flatten();
    let script = |name: &str| {
        scripts
            .as_ref()
            .and_then(|s| s.get(name))
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.trim().is_empty())
    };
    let agent = manifest.agent_command.or_else(|| {
        let preset = manifest.agent_preset?;
        let canonical = daedalus_proto::canonical_agent_preset(&preset)?;
        let program = match canonical {
            "shell" if cfg!(windows) => "cmd",
            "shell" => "sh",
            other => other,
        };
        Some(argv(&[program]))
    });
    let service = manifest.service_command.or_else(|| {
        if script("dev") {
            Some(argv(&["npm", "run", "dev"]))
        } else if script("start") {
            Some(argv(&["npm", "start"]))
        } else if compose {
            Some(argv(&["docker", "compose", "up"]))
        } else {
            None
        }
    });
    let runner = manifest.runner_command.or_else(|| {
        if has("Cargo") {
            Some(argv(&["cargo", "test"]))
        } else if script("test") {
            Some(argv(&["npm", "test"]))
        } else {
            None
        }
    });
    [
        (PaneRole::Agent, agent),
        (PaneRole::Service, service),
        (PaneRole::Runner, runner),
    ]
    .into_iter()
    .filter_map(|(role, command)| Some((role, command?)))
    .collect()
}

pub(crate) fn inspect(
    root: &Path,
    executables: Vec<String>,
) -> Result<SetupInspection, ProtoError> {
    if !root.is_dir() {
        return Err(ProtoError::InvalidRequest(format!(
            "root_dir does not exist or is not a directory: {}",
            root.display()
        )));
    }
    if executables.len() > 128 || executables.iter().any(|s| s.is_empty() || s.len() > 4096) {
        return Err(ProtoError::InvalidRequest(
            "expected up to 128 executable names of 1–4096 bytes".into(),
        ));
    }
    let manifest = manifest::load_project_manifest(root).unwrap_or_default();
    let mut commands = suggestions(root, manifest.workspace_manifest);
    let candidates: BTreeSet<_> = executables
        .into_iter()
        .chain(COMMON_TOOLS.iter().map(|s| s.to_string()))
        .chain(commands.iter().map(|(_, cmd)| cmd[0].clone()))
        .collect();
    let path = std::env::var_os("PATH").unwrap_or_default();
    let search: Vec<_> = std::env::split_paths(&path).collect();
    let available: Vec<_> = candidates
        .into_iter()
        .filter(|name| available(root, name, &search))
        .collect();
    if !commands.iter().any(|(role, _)| *role == PaneRole::Agent) {
        let agent = ["codex", "claude", "agy"]
            .into_iter()
            .find(|name| available.iter().any(|tool| tool == name))
            .unwrap_or("codex");
        commands.insert(0, (PaneRole::Agent, argv(&[agent])));
    }
    let host = ["COMPUTERNAME", "HOSTNAME"]
        .into_iter()
        .find_map(|var| std::env::var(var).ok().filter(|h| !h.trim().is_empty()))
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|h| h.trim().to_owned())
        })
        .unwrap_or_else(|| "daemon host".into());
    let dependency_install = (root.join("package.json").is_file()
        && root.join("package-lock.json").is_file()
        && !root.join("node_modules").is_dir())
    .then(|| argv(&["npm", "ci"]));
    Ok(SetupInspection {
        dependency_install,
        commands,
        available,
        host,
        platform: std::env::consts::OS.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspection_resolves_mixed_case_manifest_presets() {
        let dir = tempfile::tempdir().unwrap();
        for (preset, expected) in [
            ("aGy", "agy"),
            ("ClAuDe", "claude"),
            ("CODEX", "codex"),
            ("ShElL", if cfg!(windows) { "cmd" } else { "sh" }),
        ] {
            let manifest = WorkspaceManifest {
                agent_preset: Some(preset.into()),
                ..Default::default()
            };
            assert!(
                suggestions(dir.path(), manifest).contains(&(PaneRole::Agent, argv(&[expected])))
            );
        }
        assert_eq!(daedalus_proto::canonical_agent_preset("custom"), None);
    }

    #[test]
    fn review_prefers_manifest_preserves_argv_and_only_suggests_present_scripts() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"scripts":{"dev":"vite","test":"vitest"}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join(".daedalus.toml"),
            "[runner]\ncommand = ['custom','value with spaces']",
        )
        .unwrap();
        let inspection = inspect(dir.path(), vec![]).unwrap();
        assert!(
            inspection
                .commands
                .contains(&(PaneRole::Runner, argv(&["custom", "value with spaces"])))
        );
        assert!(
            inspection
                .commands
                .contains(&(PaneRole::Service, argv(&["npm", "run", "dev"])))
        );
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        assert!(
            !inspect(dir.path(), vec![])
                .unwrap()
                .commands
                .iter()
                .any(|(role, _)| *role == PaneRole::Service)
        );
    }

    #[test]
    fn project_dependencies_only_suggest_locked_npm_install_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        for file in ["package.json", "package-lock.json"] {
            std::fs::write(dir.path().join(file), "{}").unwrap();
        }
        assert_eq!(
            inspect(dir.path(), vec![]).unwrap().dependency_install,
            Some(argv(&["npm", "ci"]))
        );
        std::fs::create_dir(dir.path().join("node_modules")).unwrap();
        assert!(
            inspect(dir.path(), vec![])
                .unwrap()
                .dependency_install
                .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn executable_in_project_checks_permissions_without_running_project_code() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("custom");
        std::fs::write(&file, "#!/bin/sh\nexit 1").unwrap();
        let probe = || {
            inspect(dir.path(), vec!["./custom".into()])
                .unwrap()
                .available
                .contains(&"./custom".into())
        };
        assert!(!probe());
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(probe());
    }
}
