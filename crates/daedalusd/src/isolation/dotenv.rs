use std::collections::HashMap;
use std::io;
use std::path::Path;

#[derive(Debug, Default)]
pub(super) struct EnvScanResult {
    pub(super) all_vars: HashMap<String, String>,
    pub(super) port_vars: Vec<(String, u16)>,
    pub(super) url_vars: Vec<(String, String, u16)>,
}

pub(super) fn scan_env_file(root_dir: &Path) -> io::Result<EnvScanResult> {
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

pub(super) fn rewrite_localhost_url_port(url: &str, offset: u16) -> Option<String> {
    let (start, end, port) = find_localhost_url_port_span(url)?;
    let new_port = port.checked_add(offset)?;
    let mut out = String::with_capacity(url.len() + 4);
    out.push_str(&url[..start]);
    out.push_str(&new_port.to_string());
    out.push_str(&url[end..]);
    Some(out)
}
