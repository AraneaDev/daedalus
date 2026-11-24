use noyalib::compat::serde_yaml::{self, Mapping, Sequence, Value};
use std::collections::{BTreeMap, HashMap};
use std::io::{self, ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

const COMPOSE_FILENAMES: [&str; 4] = [
    "compose.yml",
    "compose.yaml",
    "docker-compose.yml",
    "docker-compose.yaml",
];
const COMPOSE_DOWN_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone)]
pub(super) enum ParsedPortEntry {
    Remappable {
        host_ip: Option<String>,
        host_port: u16,
        container_port: String,
        protocol: Option<String>,
    },
    Passthrough(Value),
}

/// Runs `docker compose -p <compose_project_name> down` inside `root_dir` with a 15-second timeout.
pub fn run_compose_down(root_dir: &Path, compose_project_name: &str) -> Result<(), String> {
    compose_down(
        crate::background_command("docker"),
        root_dir,
        compose_project_name,
        COMPOSE_DOWN_TIMEOUT,
    )
}

fn compose_down(
    mut cmd: std::process::Command,
    root_dir: &Path,
    compose_project_name: &str,
    timeout: Duration,
) -> Result<(), String> {
    cmd.arg("compose")
        .arg("-p")
        .arg(compose_project_name)
        .arg("down")
        .current_dir(root_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn docker compose: {e}"))?;

    let stdout_handle = child.stdout.take().map(spawn_reader);
    let stderr_handle = child.stderr.take().map(spawn_reader);

    let deadline = Instant::now() + timeout;
    let mut poll_interval = Duration::from_millis(2);

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                let now = Instant::now();
                if now >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    if let Some(h) = stdout_handle {
                        let _ = h.join();
                    }
                    if let Some(h) = stderr_handle {
                        let _ = h.join();
                    }
                    return Err(format!("docker compose down timed out after {timeout:?}"));
                }
                let remaining = deadline.saturating_duration_since(now);
                std::thread::sleep(poll_interval.min(remaining));
                poll_interval = (poll_interval * 2).min(Duration::from_millis(25));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("failed waiting for docker compose: {e}"));
            }
        }
    };

    let _ = stdout_handle.and_then(|h| h.join().ok());
    let stderr_bytes = stderr_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();

    if status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&stderr_bytes);
        Err(format!(
            "docker compose down failed ({status}): {}",
            stderr.trim()
        ))
    }
}

fn spawn_reader<R: Read + Send + 'static>(mut reader: R) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = reader.read_to_end(&mut buf);
        buf
    })
}

pub(crate) fn find_compose_file(root_dir: &Path) -> Option<PathBuf> {
    COMPOSE_FILENAMES
        .iter()
        .map(|name| root_dir.join(name))
        .find(|path| path.is_file())
}

/// Sanitizes a workspace name and slot index into a Compose v2 project name (`[a-z0-9_-]`).
pub(super) fn sanitize_compose_project(name: &str, slot_index: u16) -> String {
    let mapped: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = mapped.trim_matches(|c| c == '-' || c == '_');
    let base = if trimmed.is_empty() {
        "daedalus"
    } else {
        trimmed
    };

    if slot_index == 0 {
        base.to_string()
    } else {
        format!("{base}-wt{slot_index}")
    }
}

fn untag_value(mut value: &Value) -> &Value {
    while let Value::Tagged(tagged) = value {
        value = tagged.value();
    }
    value
}

pub(super) fn parse_compose_ports(
    path: &Path,
    env_lookup: &HashMap<String, String>,
) -> io::Result<BTreeMap<String, Vec<ParsedPortEntry>>> {
    let contents = std::fs::read_to_string(path)?;
    if contents.trim().is_empty() {
        return Ok(BTreeMap::new());
    }

    let doc: Value = match serde_yaml::from_str(&contents) {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "failed to parse compose file; skipping port extraction");
            return Ok(BTreeMap::new());
        }
    };

    let Some(root_map) = untag_value(&doc).as_mapping() else {
        return Ok(BTreeMap::new());
    };
    let Some(services_val) = root_map.get("services") else {
        return Ok(BTreeMap::new());
    };
    let Some(services_map) = untag_value(services_val).as_mapping() else {
        return Ok(BTreeMap::new());
    };

    let mut result = BTreeMap::new();
    for (svc_name, svc_val) in services_map {
        let Some(svc_map) = untag_value(svc_val).as_mapping() else {
            continue;
        };
        let Some(ports_val) = svc_map.get("ports") else {
            continue;
        };
        let Some(ports_seq) = untag_value(ports_val).as_sequence() else {
            continue;
        };

        let mut parsed = Vec::with_capacity(ports_seq.len());
        for item in ports_seq {
            parsed.push(parse_port_item(item, env_lookup));
        }
        if !parsed.is_empty() {
            result.insert(svc_name.to_string(), parsed);
        }
    }

    Ok(result)
}

fn parse_port_item(item: &Value, env_lookup: &HashMap<String, String>) -> ParsedPortEntry {
    match untag_value(item) {
        Value::String(s) => parse_short_port_spec(s, env_lookup)
            .unwrap_or_else(|| ParsedPortEntry::Passthrough(item.clone())),
        Value::Mapping(map) => parse_long_port_spec(map, env_lookup)
            .unwrap_or_else(|| ParsedPortEntry::Passthrough(item.clone())),
        _ => ParsedPortEntry::Passthrough(item.clone()),
    }
}

fn parse_short_port_spec(
    raw: &str,
    env_lookup: &HashMap<String, String>,
) -> Option<ParsedPortEntry> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let (base, protocol) = match trimmed.rsplit_once('/') {
        Some((before, proto))
            if !proto.is_empty() && proto.chars().all(|c| c.is_ascii_alphanumeric()) =>
        {
            (before, Some(proto.to_string()))
        }
        _ => (trimmed, None),
    };

    let parts = split_colons_outside_delims(base);
    match parts.as_slice() {
        [host_expr, container_port] => {
            let host_port = resolve_port_expr(host_expr, env_lookup)?;
            Some(ParsedPortEntry::Remappable {
                host_ip: None,
                host_port,
                container_port: (*container_port).to_string(),
                protocol,
            })
        }
        [host_ip, host_expr, container_port] => {
            let host_port = resolve_port_expr(host_expr, env_lookup)?;
            let ip = (!host_ip.is_empty()).then(|| (*host_ip).to_string());
            Some(ParsedPortEntry::Remappable {
                host_ip: ip,
                host_port,
                container_port: (*container_port).to_string(),
                protocol,
            })
        }
        _ => None,
    }
}

fn parse_long_port_spec(
    map: &Mapping,
    env_lookup: &HashMap<String, String>,
) -> Option<ParsedPortEntry> {
    let published_val = map.get("published")?;
    let target_val = map.get("target")?;

    let host_port = value_to_port(untag_value(published_val), env_lookup)?;
    let container_port = value_to_scalar_string(untag_value(target_val))?;
    let protocol = map
        .get("protocol")
        .and_then(|v| untag_value(v).as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let host_ip = map
        .get("host_ip")
        .and_then(|v| untag_value(v).as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    Some(ParsedPortEntry::Remappable {
        host_ip,
        host_port,
        container_port,
        protocol,
    })
}

fn value_to_port(val: &Value, env_lookup: &HashMap<String, String>) -> Option<u16> {
    match val {
        Value::Number(n) => n
            .as_u64()
            .and_then(|v| u16::try_from(v).ok())
            .filter(|&p| p > 0),
        Value::String(s) => resolve_port_expr(s, env_lookup),
        _ => None,
    }
}

fn value_to_scalar_string(val: &Value) -> Option<String> {
    match val {
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// Splits `s` on `:` while ignoring colons inside `${...}` and `[...]`.
fn split_colons_outside_delims(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut brace_depth = 0usize;
    let mut bracket_depth = 0usize;
    let bytes = s.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        match bytes[i] {
            b'$' if i + 1 < bytes.len() && bytes[i + 1] == b'{' => {
                brace_depth += 1;
                i += 2;
                continue;
            }
            b'{' if brace_depth > 0 => brace_depth += 1,
            b'}' if brace_depth > 0 => brace_depth -= 1,
            b'[' => bracket_depth += 1,
            b']' if bracket_depth > 0 => bracket_depth -= 1,
            b':' if brace_depth == 0 && bracket_depth == 0 => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    parts.push(&s[start..]);
    parts
}

/// Resolves a numeric port or Compose env expression (`${VAR:-3000}`, `${VAR-3000}`, `${VAR}`, `$VAR`).
fn resolve_port_expr(expr: &str, env_lookup: &HashMap<String, String>) -> Option<u16> {
    let trimmed = expr.trim();
    if let Ok(port) = trimmed.parse::<u16>() {
        return (port > 0).then_some(port);
    }

    if let Some(inner) = trimmed.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
        if let Some((var_name, default)) = inner.split_once(":-").or_else(|| inner.split_once('-'))
        {
            if let Some(val) = env_lookup.get(var_name.trim())
                && let Ok(port) = val.trim().parse::<u16>()
                && port > 0
            {
                return Some(port);
            }
            return default.trim().parse::<u16>().ok().filter(|&p| p > 0);
        }
        return env_lookup
            .get(inner.trim())
            .and_then(|v| v.trim().parse::<u16>().ok())
            .filter(|&p| p > 0);
    }

    if let Some(var_name) = trimmed.strip_prefix('$') {
        return env_lookup
            .get(var_name.trim())
            .and_then(|v| v.trim().parse::<u16>().ok())
            .filter(|&p| p > 0);
    }

    None
}

pub(super) fn write_compose_override(
    root_dir: &Path,
    compose_services: &BTreeMap<String, Vec<ParsedPortEntry>>,
    port_offset: u16,
) -> io::Result<PathBuf> {
    let daedalus_dir = root_dir.join(".daedalus");
    std::fs::create_dir_all(&daedalus_dir)?;
    let override_path = daedalus_dir.join("compose.override.yml");

    let mut remappable_services = Vec::new();
    for (svc_name, entries) in compose_services {
        let has_remappable = entries
            .iter()
            .any(|e| matches!(e, ParsedPortEntry::Remappable { .. }));
        if !has_remappable {
            continue;
        }

        let mut port_seq = Sequence::with_capacity(entries.len());
        for entry in entries {
            match entry {
                ParsedPortEntry::Remappable {
                    host_ip,
                    host_port,
                    container_port,
                    protocol,
                } => {
                    let new_host_port = host_port.checked_add(port_offset).ok_or_else(|| {
                        io::Error::new(
                            ErrorKind::InvalidInput,
                            format!("port {host_port} + offset {port_offset} overflows u16"),
                        )
                    })?;
                    let formatted = format_remapped_port(
                        host_ip.as_deref(),
                        new_host_port,
                        container_port,
                        protocol.as_deref(),
                    );
                    port_seq.push(Value::String(formatted));
                }
                ParsedPortEntry::Passthrough(raw) => {
                    port_seq.push(raw.clone());
                }
            }
        }

        remappable_services.push((svc_name, port_seq));
    }

    // Compose requires an explicit !override tag to replace the original port list.
    let mut yaml = String::from("services:");
    if remappable_services.is_empty() {
        yaml.push_str(" {}\n");
    } else {
        yaml.push('\n');
        for (svc_name, port_seq) in remappable_services {
            let key_yaml = serde_yaml::to_string(svc_name)
                .map_err(|e| io::Error::other(format!("failed to serialize service name: {e}")))?;
            let seq_yaml = serde_yaml::to_string(&Value::Sequence(port_seq)).map_err(|e| {
                io::Error::other(format!("failed to serialize compose.override.yml: {e}"))
            })?;
            yaml.push_str("  ");
            yaml.push_str(key_yaml.trim());
            yaml.push_str(":\n    ports: !override\n");
            for line in seq_yaml.lines() {
                if !line.is_empty() {
                    yaml.push_str("      ");
                    yaml.push_str(line);
                    yaml.push('\n');
                }
            }
        }
    }

    std::fs::write(&override_path, yaml)?;
    Ok(override_path)
}

fn format_remapped_port(
    host_ip: Option<&str>,
    host_port: u16,
    container_port: &str,
    protocol: Option<&str>,
) -> String {
    let mut out = String::new();
    if let Some(ip) = host_ip {
        out.push_str(ip);
        out.push(':');
    }
    out.push_str(&host_port.to_string());
    out.push(':');
    out.push_str(container_port);
    if let Some(proto) = protocol {
        out.push('/');
        out.push_str(proto);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[cfg(unix)]
    #[test]
    fn compose_down_passes_project_and_directory_and_reports_failures() {
        use std::process::Command;
        let dir = tempdir().unwrap();
        let shell = |script: &str| {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", script, "docker"]);
            command
        };
        compose_down(
            shell(r#"pwd > cwd; printf '%s\n' "$@" > args"#),
            dir.path(),
            "project name",
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("args")).unwrap(),
            "compose\n-p\nproject name\ndown\n"
        );
        let cwd = std::fs::read_to_string(dir.path().join("cwd")).unwrap();
        assert_eq!(Path::new(cwd.trim()), dir.path().canonicalize().unwrap());
        let err = compose_down(
            shell("echo 'engine offline' >&2; exit 17"),
            dir.path(),
            "test",
            Duration::from_secs(5),
        )
        .unwrap_err();
        assert!(
            err.contains("17") && err.contains("engine offline"),
            "{err}"
        );
        let err = compose_down(
            shell("exec sleep 30"),
            dir.path(),
            "test",
            Duration::from_millis(30),
        )
        .unwrap_err();
        assert!(err.contains("timed out after 30ms"), "{err}");
        assert!(
            run_compose_down(&dir.path().join("missing"), "test")
                .unwrap_err()
                .contains("failed to spawn")
        );
    }

    #[test]
    fn invalid_compose_documents_are_skipped_and_io_errors_propagate() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("compose.yaml");
        let env = HashMap::new();
        assert_eq!(
            parse_compose_ports(&path, &env).unwrap_err().kind(),
            ErrorKind::NotFound
        );
        for yaml in [
            "",
            "{",
            "true",
            "networks: {}",
            "services: false",
            "services: {empty: {}, broken: 1, invalid: {ports: false}}",
        ] {
            std::fs::write(&path, yaml).unwrap();
            assert!(
                parse_compose_ports(&path, &env).unwrap().is_empty(),
                "{yaml:?}"
            );
        }
    }

    #[test]
    fn override_remaps_interpolated_ipv6_and_long_ports_and_preserves_unresolved_entries() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("compose.yml");
        std::fs::write(
            &path,
            r#"
services:
  api:
    ports: !override
      - '[::1]:${HTTP:-80}:80/tcp'
      - {published: '$DNS', target: '53', protocol: udp, host_ip: '127.0.0.1'}
      - '${UNKNOWN}:90'
      - {published: true, target: 12}
      - 123
  untouched: {ports: [5000]}
"#,
        )
        .unwrap();
        let env = HashMap::from([
            ("HTTP".into(), "8000".into()),
            ("DNS".into(), "5300".into()),
        ]);
        let ports = parse_compose_ports(&path, &env).unwrap();
        let output = write_compose_override(dir.path(), &ports, 1).unwrap();
        let saved = std::fs::read_to_string(&output).unwrap();
        let doc: Value = serde_yaml::from_str(&saved).unwrap();
        let services = doc
            .as_mapping()
            .unwrap()
            .get("services")
            .unwrap()
            .as_mapping()
            .unwrap();
        assert!(services.get("untouched").is_none());
        let api = services
            .get("api")
            .unwrap()
            .as_mapping()
            .unwrap()
            .get("ports")
            .unwrap();
        let seq = untag_value(api).as_sequence().unwrap();
        assert_eq!(seq[0].as_str(), Some("[::1]:8001:80/tcp"));
        assert_eq!(seq[1].as_str(), Some("127.0.0.1:5301:53/udp"));
        assert_eq!(seq[2].as_str(), Some("${UNKNOWN}:90"));
        assert_eq!(
            seq[3].as_mapping().unwrap().get("published"),
            Some(&Value::Bool(true))
        );
        assert_eq!(seq[4].as_u64(), Some(123));
        assert_eq!(
            write_compose_override(dir.path(), &ports, u16::MAX)
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidInput
        );
        assert_eq!(std::fs::read_to_string(output).unwrap(), saved);
    }
}
