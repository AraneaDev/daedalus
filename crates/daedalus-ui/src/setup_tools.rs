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
