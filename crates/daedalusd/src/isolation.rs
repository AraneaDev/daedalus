use serde_yml::{Mapping, Sequence, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{self, ErrorKind, Read};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const COMPOSE_FILENAMES: [&str; 4] = [
    "compose.yml",
    "compose.yaml",
    "docker-compose.yml",
    "docker-compose.yaml",
];
const PORT_BLOCK_SIZE: u16 = 100;
const MAX_PROBE_BLOCKS: u16 = 50;
const COMPOSE_DOWN_TIMEOUT: Duration = Duration::from_secs(15);

/// Result of computing port/Compose/env isolation for a workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsolationPlan {
    pub slot_index: u16,
    pub port_offset: u16,
    pub compose_project_name: Option<String>,
    pub compose_override_path: Option<PathBuf>,
    pub injected_env: HashMap<String, String>,
    pub remapped_ports: HashMap<u16, u16>,
}

#[derive(Debug, Clone)]
enum ParsedPortEntry {
    Remappable {
        host_ip: Option<String>,
        host_port: u16,
        container_port: String,
        protocol: Option<String>,
    },
    Passthrough(Value),
}

#[derive(Debug, Default)]
struct EnvScanResult {
    all_vars: HashMap<String, String>,
    port_vars: Vec<(String, u16)>,
    url_vars: Vec<(String, String, u16)>,
}

/// Computes a conflict-free port offset, rewrites `.env` port/URL variables,
/// and generates `.daedalus/compose.override.yml` when a Compose file exists.
pub fn prepare_workspace_isolation(
    name: &str,
    root_dir: &Path,
    slot_index: u16,
    caller_env: &HashMap<String, String>,
) -> io::Result<IsolationPlan> {
    prepare_workspace_isolation_with_reserved(
        name,
        root_dir,
        slot_index,
        caller_env,
        &HashSet::new(),
    )
}

/// Computes a conflict-free port offset while treating `reserved_ports` as unavailable,
/// rewrites `.env` port/URL variables, and generates `.daedalus/compose.override.yml`
/// when a Compose file exists.
pub fn prepare_workspace_isolation_with_reserved(
    name: &str,
    root_dir: &Path,
    slot_index: u16,
    caller_env: &HashMap<String, String>,
    reserved_ports: &HashSet<u16>,
) -> io::Result<IsolationPlan> {
    prepare_workspace_isolation_with_extra_ports(
        name,
        root_dir,
        slot_index,
        caller_env,
        reserved_ports,
        &[],
    )
}

/// Computes a conflict-free port offset while treating `reserved_ports` as unavailable
/// and including `extra_ports` (e.g. from `.daedalus.toml` `[ports].extra`) in
/// port reservation and offset remapping.
pub fn prepare_workspace_isolation_with_extra_ports(
    name: &str,
    root_dir: &Path,
    slot_index: u16,
    caller_env: &HashMap<String, String>,
    reserved_ports: &HashSet<u16>,
    extra_ports: &[u16],
) -> io::Result<IsolationPlan> {
    let env_scan = scan_env_file(root_dir)?;
    let mut env_lookup = env_scan.all_vars.clone();
    for (k, v) in caller_env {
        env_lookup.insert(k.clone(), v.clone());
    }

    let compose_file = find_compose_file(root_dir);
    let compose_services = match &compose_file {
        Some(path) => parse_compose_ports(path, &env_lookup)?,
        None => BTreeMap::new(),
    };

    let mut base_ports = BTreeSet::new();
    for entries in compose_services.values() {
        for entry in entries {
            if let ParsedPortEntry::Remappable { host_port, .. } = entry {
                base_ports.insert(*host_port);
            }
        }
    }
    for (_, port) in &env_scan.port_vars {
        base_ports.insert(*port);
    }
    for (_, _, port) in &env_scan.url_vars {
        base_ports.insert(*port);
    }
    for &port in extra_ports {
        if port > 0 {
            base_ports.insert(port);
        }
    }

    let port_offset = allocate_port_offset(slot_index, &base_ports, reserved_ports)?;

    let mut remapped_ports = HashMap::new();
    if port_offset > 0 {
        for &base in &base_ports {
            let mapped = base.checked_add(port_offset).ok_or_else(|| {
                io::Error::new(
                    ErrorKind::InvalidInput,
                    format!("port {base} + offset {port_offset} overflows u16"),
                )
            })?;
            remapped_ports.insert(base, mapped);
        }
    }

    let mut injected_env = HashMap::new();
    let (compose_project_name, compose_override_path) = match compose_file {
        Some(base_path) => {
            let project = sanitize_compose_project(name, slot_index);
            let override_path = write_compose_override(root_dir, &compose_services, port_offset)?;
            injected_env.insert("COMPOSE_PROJECT_NAME".to_string(), project.clone());
            injected_env.insert(
                "COMPOSE_FILE".to_string(),
                format!("{}:{}", base_path.display(), override_path.display()),
            );
            (Some(project), Some(override_path))
        }
        None => (None, None),
    };

    if port_offset > 0 {
        for (key, port) in &env_scan.port_vars {
            if let Some(mapped) = port.checked_add(port_offset) {
                injected_env.insert(key.clone(), mapped.to_string());
            }
        }
        for (key, raw_url, _) in &env_scan.url_vars {
            if let Some(rewritten) = rewrite_localhost_url_port(raw_url, port_offset) {
                injected_env.insert(key.clone(), rewritten);
            }
        }
    }

    for (k, v) in caller_env {
        injected_env.insert(k.clone(), v.clone());
    }

    Ok(IsolationPlan {
        slot_index,
        port_offset,
        compose_project_name,
        compose_override_path,
        injected_env,
        remapped_ports,
    })
}

/// Runs `docker compose -p <compose_project_name> down` inside `root_dir` with a 15-second timeout.
pub fn run_compose_down(root_dir: &Path, compose_project_name: &str) -> Result<(), String> {
    let mut cmd = Command::new("docker");
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

    let deadline = Instant::now() + COMPOSE_DOWN_TIMEOUT;
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
                    return Err("docker compose down timed out after 15s".to_string());
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

fn find_compose_file(root_dir: &Path) -> Option<PathBuf> {
    COMPOSE_FILENAMES
        .iter()
        .map(|name| root_dir.join(name))
        .find(|path| path.is_file())
}

/// Sanitizes a workspace name and slot index into a Compose v2 project name (`[a-z0-9_-]`).
fn sanitize_compose_project(name: &str, slot_index: u16) -> String {
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

fn parse_compose_ports(
    path: &Path,
    env_lookup: &HashMap<String, String>,
) -> io::Result<BTreeMap<String, Vec<ParsedPortEntry>>> {
    let contents = std::fs::read_to_string(path)?;
    if contents.trim().is_empty() {
        return Ok(BTreeMap::new());
    }

    let doc: Value = match serde_yml::from_str(&contents) {
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

fn write_compose_override(
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

    // Emit `!override` YAML tags directly because `serde_yml 0.0.13`'s generic
    // `to_string` routes `Value::Tagged` through `serialize_map` instead of
    // writing YAML tag syntax.
    let mut yaml = String::from("services:");
    if remappable_services.is_empty() {
        yaml.push_str(" {}\n");
    } else {
        yaml.push('\n');
        for (svc_name, port_seq) in remappable_services {
            let key_yaml = serde_yml::to_string(svc_name)
                .map_err(|e| io::Error::other(format!("failed to serialize service name: {e}")))?;
            let seq_yaml = serde_yml::to_string(&Value::Sequence(port_seq)).map_err(|e| {
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

fn scan_env_file(root_dir: &Path) -> io::Result<EnvScanResult> {
    let env_path = root_dir.join(".env");
    let example_path = root_dir.join(".env.example");
    let target = if env_path.is_file() {
        env_path
    } else if example_path.is_file() {
        example_path
    } else {
        return Ok(EnvScanResult::default());
    };

    let contents = std::fs::read_to_string(target)?;
    let mut scan = EnvScanResult::default();

    for line in contents.lines() {
        let Some((key, value)) = parse_dotenv_line(line) else {
            continue;
        };
        scan.all_vars.insert(key.clone(), value.clone());

        if key == "PORT" || key.ends_with("_PORT") {
            if let Ok(port) = value.parse::<u16>()
                && port > 0
            {
                scan.port_vars.push((key, port));
            }
        } else if key.ends_with("_URL")
            && let Some(port) = extract_localhost_url_port(&value)
        {
            scan.url_vars.push((key, value, port));
        }
    }

    Ok(scan)
}

fn parse_dotenv_line(raw: &str) -> Option<(String, String)> {
    let mut line = raw.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    if let Some(rest) = line.strip_prefix("export ") {
        line = rest.trim_start();
    }
    let (key, raw_val) = line.split_once('=')?;
    let key = key.trim();
    if key.is_empty() {
        return None;
    }

    let val = raw_val.trim();
    let parsed_val = if let Some(quoted) = strip_matching_quotes(val) {
        quoted.to_string()
    } else {
        strip_inline_comment(val).trim().to_string()
    };

    Some((key.to_string(), parsed_val))
}

fn strip_matching_quotes(val: &str) -> Option<&str> {
    if val.len() >= 2
        && ((val.starts_with('"') && val.ends_with('"'))
            || (val.starts_with('\'') && val.ends_with('\'')))
    {
        Some(&val[1..val.len() - 1])
    } else {
        None
    }
}

fn strip_inline_comment(val: &str) -> &str {
    match val.find(" #") {
        Some(idx) => &val[..idx],
        None => val,
    }
}

/// Finds the byte range `(start..end)` of the port in a URL whose host is `localhost` or `127.0.0.1`.
fn find_localhost_url_port_span(url: &str) -> Option<(usize, usize, u16)> {
    let scheme_idx = url.find("://")?;
    let authority_start = scheme_idx + 3;
    let rest = &url[authority_start..];
    let authority_end = authority_start + rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &url[authority_start..authority_end];

    let (host_port_offset, host_port) = match authority.rfind('@') {
        Some(at_idx) => (authority_start + at_idx + 1, &authority[at_idx + 1..]),
        None => (authority_start, authority),
    };

    let (host, port_str) = host_port.split_once(':')?;
    if !host.eq_ignore_ascii_case("localhost") && host != "127.0.0.1" {
        return None;
    }
    if port_str.is_empty() || !port_str.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let port = port_str.parse::<u16>().ok().filter(|&p| p > 0)?;
    let port_start = host_port_offset + host.len() + 1;
    let port_end = port_start + port_str.len();
    Some((port_start, port_end, port))
}

fn extract_localhost_url_port(url: &str) -> Option<u16> {
    find_localhost_url_port_span(url).map(|(_, _, port)| port)
}

fn rewrite_localhost_url_port(url: &str, offset: u16) -> Option<String> {
    let (start, end, port) = find_localhost_url_port_span(url)?;
    let new_port = port.checked_add(offset)?;
    let mut out = String::with_capacity(url.len() + 4);
    out.push_str(&url[..start]);
    out.push_str(&new_port.to_string());
    out.push_str(&url[end..]);
    Some(out)
}

fn allocate_port_offset(
    slot_index: u16,
    base_ports: &BTreeSet<u16>,
    reserved_ports: &HashSet<u16>,
) -> io::Result<u16> {
    if slot_index == 0 {
        return Ok(0);
    }

    let mut offset = match slot_index.checked_mul(PORT_BLOCK_SIZE) {
        Some(o) => o,
        None => {
            return Err(io::Error::new(
                ErrorKind::AddrInUse,
                format!("slot_index {slot_index} overflows u16 port offset"),
            ));
        }
    };

    if base_ports.is_empty() {
        return Ok(offset);
    }

    for _ in 0..MAX_PROBE_BLOCKS {
        let block_ok = base_ports.iter().all(|&base| {
            base.checked_add(offset)
                .filter(|&p| p > 0)
                .is_some_and(|p| !reserved_ports.contains(&p) && is_port_available(p))
        });
        if block_ok {
            return Ok(offset);
        }
        offset = match offset.checked_add(PORT_BLOCK_SIZE) {
            Some(next) => next,
            None => break,
        };
    }

    Err(io::Error::new(
        ErrorKind::AddrInUse,
        format!(
            "no conflict-free port block found within {MAX_PROBE_BLOCKS} blocks from slot {slot_index}"
        ),
    ))
}

fn is_port_available(port: u16) -> bool {
    let Ok(v4_listener) = TcpListener::bind(("127.0.0.1", port)) else {
        return false;
    };
    let available = match TcpListener::bind(("::1", port)) {
        Ok(_v6_listener) => true,
        Err(err) => matches!(
            err.kind(),
            ErrorKind::AddrNotAvailable | ErrorKind::Unsupported
        ),
    };
    drop(v4_listener);
    available
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn parses_short_and_long_compose_ports() {
        let dir = tempdir().unwrap();
        let compose = dir.path().join("compose.yml");
        std::fs::write(
            &compose,
            r#"
services:
  web:
    image: node:20
    ports:
      - "8080:80"
      - "127.0.0.1:5432:5432"
      - "3000:3000/tcp"
      - "${WEB_PORT:-4000}:4000"
      - "9090"
  db:
    image: postgres:16
    ports:
      - target: 5432
        published: 15432
        protocol: tcp
        host_ip: 127.0.0.1
"#,
        )
        .unwrap();

        let env = HashMap::new();
        let parsed = parse_compose_ports(&compose, &env).unwrap();

        let web = parsed.get("web").unwrap();
        assert_eq!(web.len(), 5);
        assert!(matches!(
            &web[0],
            ParsedPortEntry::Remappable {
                host_ip: None,
                host_port: 8080,
                container_port,
                protocol: None,
            } if container_port == "80"
        ));
        assert!(matches!(
            &web[1],
            ParsedPortEntry::Remappable {
                host_ip: Some(ip),
                host_port: 5432,
                container_port,
                protocol: None,
            } if ip == "127.0.0.1" && container_port == "5432"
        ));
        assert!(matches!(
            &web[2],
            ParsedPortEntry::Remappable {
                host_ip: None,
                host_port: 3000,
                container_port,
                protocol: Some(proto),
            } if container_port == "3000" && proto == "tcp"
        ));
        assert!(matches!(
            &web[3],
            ParsedPortEntry::Remappable {
                host_ip: None,
                host_port: 4000,
                container_port,
                protocol: None,
            } if container_port == "4000"
        ));
        assert!(matches!(&web[4], ParsedPortEntry::Passthrough(_)));

        let db = parsed.get("db").unwrap();
        assert_eq!(db.len(), 1);
        assert!(matches!(
            &db[0],
            ParsedPortEntry::Remappable {
                host_ip: Some(ip),
                host_port: 15432,
                container_port,
                protocol: Some(proto),
            } if ip == "127.0.0.1" && container_port == "5432" && proto == "tcp"
        ));
    }

    #[test]
    fn rewrites_dotenv_ports_and_localhost_urls() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join(".env"),
            r#"
# App settings
PORT=3400
export API_PORT="8480"
DATABASE_URL=postgres://user:p@ss@localhost:5732/app?sslmode=disable
REDIS_URL='redis://127.0.0.1:6479/0'
REMOTE_URL=https://api.example.com:8443/v1
LOCAL_NO_PORT_URL=http://localhost/health
"#,
        )
        .unwrap();

        let mut caller_env = HashMap::new();
        caller_env.insert("API_PORT".to_string(), "9999".to_string());

        let plan = prepare_workspace_isolation("My App", dir.path(), 1, &caller_env).unwrap();

        assert_eq!(plan.slot_index, 1);
        assert_eq!(plan.port_offset, 100);
        assert_eq!(plan.compose_project_name, None);
        assert_eq!(plan.compose_override_path, None);

        assert_eq!(
            plan.injected_env.get("PORT").map(String::as_str),
            Some("3500")
        );
        // Caller env overrides remapped .env variable.
        assert_eq!(
            plan.injected_env.get("API_PORT").map(String::as_str),
            Some("9999")
        );
        assert_eq!(
            plan.injected_env.get("DATABASE_URL").map(String::as_str),
            Some("postgres://user:p@ss@localhost:5832/app?sslmode=disable")
        );
        assert_eq!(
            plan.injected_env.get("REDIS_URL").map(String::as_str),
            Some("redis://127.0.0.1:6579/0")
        );
        assert!(!plan.injected_env.contains_key("REMOTE_URL"));
        assert!(!plan.injected_env.contains_key("LOCAL_NO_PORT_URL"));

        assert_eq!(plan.remapped_ports.get(&3400), Some(&3500));
        assert_eq!(plan.remapped_ports.get(&5732), Some(&5832));
        assert_eq!(plan.remapped_ports.get(&6479), Some(&6579));
    }

    #[test]
    fn falls_back_to_env_example_and_skips_remap_for_slot_zero() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join(".env.example"),
            "PORT=4500\nDATABASE_URL=postgres://localhost:5432/db\n",
        )
        .unwrap();

        let plan = prepare_workspace_isolation("primary", dir.path(), 0, &HashMap::new()).unwrap();

        assert_eq!(plan.port_offset, 0);
        assert!(plan.remapped_ports.is_empty());
        assert!(!plan.injected_env.contains_key("PORT"));
        assert!(!plan.injected_env.contains_key("DATABASE_URL"));
    }

    #[test]
    fn bumps_port_block_on_real_tcp_conflict() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "PORT=3000\n").unwrap();

        let blocker = TcpListener::bind(("127.0.0.1", 3100))
            .expect("port 3100 must be free before blocking it for conflict test");

        let plan =
            prepare_workspace_isolation("wt-conflict", dir.path(), 1, &HashMap::new()).unwrap();

        drop(blocker);

        assert_eq!(plan.slot_index, 1);
        assert_eq!(plan.port_offset, 200);
        assert_eq!(plan.remapped_ports.get(&3000), Some(&3200));
        assert_eq!(
            plan.injected_env.get("PORT").map(String::as_str),
            Some("3200")
        );
    }

    #[test]
    fn skips_reserved_ports_from_other_workspaces() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "PORT=3000\n").unwrap();

        let reserved = HashSet::from([3200]);
        let plan = prepare_workspace_isolation_with_reserved(
            "wt-reserved",
            dir.path(),
            2,
            &HashMap::new(),
            &reserved,
        )
        .unwrap();

        assert_eq!(plan.slot_index, 2);
        assert_eq!(plan.port_offset, 300);
        assert_eq!(plan.remapped_ports.get(&3000), Some(&3300));
        assert_eq!(
            plan.injected_env.get("PORT").map(String::as_str),
            Some("3300")
        );
    }

    #[test]
    fn generates_compose_override_with_override_tag_and_env() {
        let dir = tempdir().unwrap();
        let compose_path = dir.path().join("docker-compose.yaml");
        std::fs::write(
            &compose_path,
            r#"
services:
  web:
    ports:
      - "${WEB_PORT:-4200}:3000/tcp"
      - "127.0.0.1:8280:80"
      - "9090"
  db:
    ports:
      - target: 5432
        published: 5632
        host_ip: 127.0.0.1
        protocol: tcp
"#,
        )
        .unwrap();

        let plan = prepare_workspace_isolation("My Repo!", dir.path(), 1, &HashMap::new()).unwrap();

        let override_path = plan
            .compose_override_path
            .as_ref()
            .expect("override path must be set");
        assert!(override_path.is_file());
        assert_eq!(
            override_path,
            &dir.path().join(".daedalus").join("compose.override.yml")
        );
        assert_eq!(plan.compose_project_name.as_deref(), Some("my-repo-wt1"));
        assert_eq!(
            plan.injected_env
                .get("COMPOSE_PROJECT_NAME")
                .map(String::as_str),
            Some("my-repo-wt1")
        );
        assert_eq!(
            plan.injected_env.get("COMPOSE_FILE").map(String::as_str),
            Some(format!("{}:{}", compose_path.display(), override_path.display()).as_str())
        );

        let yaml_text = std::fs::read_to_string(override_path).unwrap();
        assert!(yaml_text.contains("!override"), "yaml was:\n{yaml_text}");
        assert!(
            yaml_text.contains("4300:3000/tcp"),
            "yaml was:\n{yaml_text}"
        );
        assert!(
            yaml_text.contains("127.0.0.1:8380:80"),
            "yaml was:\n{yaml_text}"
        );
        assert!(yaml_text.contains("9090"), "yaml was:\n{yaml_text}");
        assert!(
            yaml_text.contains("127.0.0.1:5732:5432/tcp"),
            "yaml was:\n{yaml_text}"
        );

        // Verify the YAML parses back with `!override` tags intact.
        let parsed: Value = serde_yml::from_str(&yaml_text).unwrap();
        let web_ports = &parsed["services"]["web"]["ports"];
        assert!(matches!(
            web_ports,
            Value::Tagged(tagged) if tagged.tag().nobang() == "override"
        ));
    }

    #[test]
    fn resolves_bare_env_var_in_compose_and_handles_ipv6_short_syntax() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "APP_PORT=4600\n").unwrap();
        std::fs::write(
            dir.path().join("compose.yml"),
            r#"
services:
  app:
    ports:
      - "${APP_PORT}:8080"
      - "[::1]:5932:5432"
"#,
        )
        .unwrap();

        let plan = prepare_workspace_isolation("app", dir.path(), 1, &HashMap::new()).unwrap();
        assert_eq!(plan.remapped_ports.get(&4600), Some(&4700));
        assert_eq!(plan.remapped_ports.get(&5932), Some(&6032));

        let yaml_text =
            std::fs::read_to_string(plan.compose_override_path.as_ref().unwrap()).unwrap();
        assert!(yaml_text.contains("4700:8080"), "yaml was:\n{yaml_text}");
        assert!(
            yaml_text.contains("[::1]:6032:5432"),
            "yaml was:\n{yaml_text}"
        );
    }

    #[test]
    fn errors_when_port_offset_overflows_u16() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "PORT=65500\n").unwrap();

        let err = prepare_workspace_isolation("overflow", dir.path(), 1, &HashMap::new())
            .expect_err("65500 + 100 should overflow u16 across all probe blocks");
        assert_eq!(err.kind(), ErrorKind::AddrInUse);
    }

    #[test]
    fn sanitize_compose_project_matches_spec_examples() {
        assert_eq!(sanitize_compose_project("myrepo", 0), "myrepo");
        assert_eq!(sanitize_compose_project("myrepo", 1), "myrepo-wt1");
        assert_eq!(sanitize_compose_project("---", 0), "daedalus");
        assert_eq!(
            sanitize_compose_project("Feat/Auth_V2", 3),
            "feat-auth_v2-wt3"
        );
    }

    #[test]
    fn extra_ports_are_remapped_and_avoid_reserved_and_bound_conflicts() {
        let dir = tempdir().unwrap();
        let blocker = TcpListener::bind(("127.0.0.1", 19100))
            .expect("port 19100 must be free before blocking");
        let reserved = HashSet::from([19280]);

        let plan = prepare_workspace_isolation_with_extra_ports(
            "extra",
            dir.path(),
            1,
            &HashMap::new(),
            &reserved,
            &[0, 19000, 19080],
        )
        .unwrap();
        drop(blocker);

        // Slot 1 starts at offset 100 (19100 blocked by TCP listener),
        // bumps to offset 200 (19280 blocked by reserved_ports),
        // and lands on offset 300.
        assert_eq!(plan.port_offset, 300);
        assert_eq!(plan.remapped_ports.get(&19000), Some(&19300));
        assert_eq!(plan.remapped_ports.get(&19080), Some(&19380));
        assert!(!plan.remapped_ports.contains_key(&0));
    }
}
