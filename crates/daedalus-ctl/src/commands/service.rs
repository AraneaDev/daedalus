use crate::{DEFAULT_DAEDALUSD_BIN, DEFAULT_LAUNCHD_LABEL, ServiceAction};
use std::path::{Path, PathBuf};

pub(crate) fn resolve_daedalusd_binary(
    explicit: Option<PathBuf>,
    current_exe: Option<&Path>,
) -> PathBuf {
    if let Some(path) = explicit {
        return path;
    }
    if let Some(sibling) = current_exe
        .and_then(|exe| exe.parent())
        .map(|dir| dir.join("daedalusd"))
        .filter(|path| path.is_file())
    {
        return sibling;
    }
    PathBuf::from(DEFAULT_DAEDALUSD_BIN)
}

fn default_launchd_log_dir(home: Option<&str>) -> PathBuf {
    match home.map(str::trim).filter(|h| !h.is_empty()) {
        Some(h) => PathBuf::from(format!("{}/Library/Logs", h.trim_end_matches('/'))),
        None => std::env::temp_dir(),
    }
}

fn escape_xml(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

pub(crate) fn render_launchd_plist(
    label: &str,
    binary: &Path,
    socket: &Path,
    log_dir: &Path,
) -> String {
    let label_xml = escape_xml(label);
    let binary_xml = escape_xml(&binary.to_string_lossy());
    let socket_xml = escape_xml(&socket.to_string_lossy());
    let log_dir_str = log_dir.to_string_lossy();
    let log_prefix = log_dir_str.trim_end_matches(['/', '\\']);
    let stdout_xml = escape_xml(&format!("{log_prefix}/daedalusd.out.log"));
    let stderr_xml = escape_xml(&format!("{log_prefix}/daedalusd.err.log"));

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label_xml}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{binary_xml}</string>
    <string>--socket</string>
    <string>{socket_xml}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>{stdout_xml}</string>
  <key>StandardErrorPath</key>
  <string>{stderr_xml}</string>
</dict>
</plist>
"#
    )
}

pub(crate) fn execute(action: ServiceAction, socket: &Path) {
    match action {
        ServiceAction::PrintLaunchd { binary, label } => {
            let label = label
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(DEFAULT_LAUNCHD_LABEL)
                .to_string();
            let current_exe = std::env::current_exe().ok();
            let binary = resolve_daedalusd_binary(binary, current_exe.as_deref());
            let home = std::env::var("HOME").ok();
            let log_dir = default_launchd_log_dir(home.as_deref());
            print!(
                "{}",
                render_launchd_plist(&label, &binary, socket, &log_dir)
            );
        }
    }
}
