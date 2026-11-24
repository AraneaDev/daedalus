use daedalus_proto::SetupInspection;

/// Install recipes are literal argument lists. Custom project commands only affect tool lookup.
pub fn install_command(tool: &str, inspection: &SetupInspection) -> Option<Vec<String>> {
    let has = |name| {
        inspection
            .available
            .iter()
            .any(|candidate| candidate == name)
    };
    if has(tool) {
        return None;
    }
    let args: &[&str] = match tool {
        "codex" if has("npm") => &["npm", "install", "-g", "@openai/codex"],
        "claude" if has("npm") => &["npm", "install", "-g", "@anthropic-ai/claude-code"],
        "npm" | "node" if has("brew") && inspection.platform == "macos" => {
            &["brew", "install", "node"]
        }
        _ => return None,
    };
    Some(args.iter().map(|arg| (*arg).into()).collect())
}

pub fn guide_url(tool: &str) -> Option<&'static str> {
    Some(match tool {
        "codex" => "https://developers.openai.com/codex/cli/",
        "claude" => "https://code.claude.com/docs/en/setup",
        "agy" => "https://antigravity.google/docs/cli/tutorial/",
        "cargo" | "rustc" => "https://rust-lang.org/install",
        "npm" | "node" => "https://nodejs.org/en/download",
        "docker" => "https://docs.docker.com/get-started/get-docker/",
        "composer" => "https://getcomposer.org/download/",
        "python" | "python3" => "https://www.python.org/downloads/",
        "make" => "https://www.gnu.org/software/make/",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_recipes_require_a_missing_tool_and_matching_host_package_manager() {
        let mut host = SetupInspection {
            commands: vec![],
            available: vec!["npm".into()],
            host: "runner".into(),
            platform: "linux".into(),
            dependency_install: None,
        };
        for (tool, package) in [
            ("codex", "@openai/codex"),
            ("claude", "@anthropic-ai/claude-code"),
        ] {
            assert_eq!(
                install_command(tool, &host).unwrap(),
                ["npm", "install", "-g", package]
            );
            host.available.push(tool.into());
            assert!(install_command(tool, &host).is_none());
            host.available.pop();
        }
        host.available = vec!["brew".into()];
        assert!(install_command("npm", &host).is_none());
        host.platform = "macos".into();
        for exe in ["npm", "node"] {
            assert_eq!(
                install_command(exe, &host).unwrap(),
                ["brew", "install", "node"]
            );
        }
        assert!(install_command("custom", &host).is_none());
        assert!(guide_url("custom").is_none());
    }
}
