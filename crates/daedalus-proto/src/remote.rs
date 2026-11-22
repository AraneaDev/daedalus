use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const DEFAULT_REMOTE_TCP_TARGET: &str = "127.0.0.1:7777";
const TUNNEL_READY_TIMEOUT: Duration = Duration::from_secs(10);
const TUNNEL_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Parsed `ssh://[user@]host[:port][/remote/socket/path]` or `ssh://[user@]host[:port][?tcp=host:port]` target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshTarget {
    pub user: Option<String>,
    pub host: String,
    pub port: Option<u16>,
    pub remote_target: Option<String>,
}

impl SshTarget {
    /// Parses an `ssh://` target URI, validating against empty hosts, invalid ports,
    /// control/whitespace characters, and leading-dash SSH option injection.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let rest = raw
            .strip_prefix("ssh://")
            .ok_or_else(|| "SSH target must start with 'ssh://'".to_string())?;
        if rest.is_empty() {
            return Err("SSH target cannot be empty".to_string());
        }
        if raw.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err("SSH target cannot contain whitespace or control characters".to_string());
        }

        let (authority, suffix) = match rest.find(['/', '?']) {
            Some(idx) => (&rest[..idx], &rest[idx..]),
            None => (rest, ""),
        };

        let (user, host, port) = parse_authority(authority)?;
        let remote_target = parse_remote_suffix(suffix)?;

        Ok(Self {
            user,
            host,
            port,
            remote_target,
        })
    }

    /// Returns the remote forwarding destination for `ssh -L 127.0.0.1:<local_port>:<spec>`.
    ///
    /// Uses the explicit remote Unix socket path or `?tcp=...` target when provided,
    /// otherwise defaults to `127.0.0.1:7777` (matching `daedalusd --socket tcp://127.0.0.1:7777`).
    pub fn remote_forward_spec(&self) -> String {
        self.remote_target
            .clone()
            .unwrap_or_else(|| DEFAULT_REMOTE_TCP_TARGET.to_string())
    }

    /// Returns `user@host` if a user is set, otherwise `host`.
    pub fn display_label(&self) -> String {
        match &self.user {
            Some(user) => format!("{user}@{}", self.host),
            None => self.host.clone(),
        }
    }
}

fn validate_ssh_token(value: &str, kind: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("SSH {kind} cannot be empty"));
    }
    if value.starts_with('-') {
        return Err(format!("SSH {kind} cannot start with '-'"));
    }
    if value.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!(
            "SSH {kind} cannot contain whitespace or control characters"
        ));
    }
    Ok(())
}

fn parse_port(raw: &str, context: &str) -> Result<u16, String> {
    if raw.is_empty() {
        return Err(format!("{context} port cannot be empty after ':'"));
    }
    let port: u16 = raw
        .parse()
        .map_err(|_| format!("invalid {context} port '{raw}'"))?;
    if port == 0 {
        return Err(format!("{context} port must be between 1 and 65535"));
    }
    Ok(port)
}

fn parse_authority(authority: &str) -> Result<(Option<String>, String, Option<u16>), String> {
    if authority.is_empty() {
        return Err("SSH host cannot be empty".to_string());
    }
    if authority.matches('@').count() > 1 {
        return Err("SSH target cannot contain multiple '@' characters".to_string());
    }

    let (user, host_port) = match authority.split_once('@') {
        Some((u, hp)) => {
            validate_ssh_token(u, "user")?;
            if u.contains(':') || u.contains('\\') {
                return Err("SSH user contains invalid characters".to_string());
            }
            (Some(u.to_string()), hp)
        }
        None => (None, authority),
    };

    let (host, port) = if let Some(after_open) = host_port.strip_prefix('[') {
        let close_idx = after_open
            .find(']')
            .ok_or_else(|| "unclosed '[' in IPv6 SSH host".to_string())?;
        let inner_host = &after_open[..close_idx];
        let trailing = &after_open[close_idx + 1..];
        let port = if trailing.is_empty() {
            None
        } else if let Some(port_str) = trailing.strip_prefix(':') {
            Some(parse_port(port_str, "SSH")?)
        } else {
            return Err("unexpected characters after ']' in SSH host".to_string());
        };
        (inner_host, port)
    } else if let Some((h, port_str)) = host_port.rsplit_once(':') {
        if h.contains(':') {
            return Err("IPv6 SSH hosts must be enclosed in brackets '[...]'".to_string());
        }
        (h, Some(parse_port(port_str, "SSH")?))
    } else {
        (host_port, None)
    };

    validate_ssh_token(host, "host")?;
    if host.contains('\\') {
        return Err("SSH host contains invalid characters".to_string());
    }

    Ok((user, host.to_string(), port))
}

fn parse_remote_suffix(suffix: &str) -> Result<Option<String>, String> {
    if suffix.is_empty() {
        return Ok(None);
    }

    if let Some(query) = suffix.strip_prefix('?') {
        return parse_tcp_query(query).map(Some);
    }

    if let Some((path, query)) = suffix.split_once('?') {
        if path == "/" {
            return parse_tcp_query(query).map(Some);
        }
        return Err("cannot specify both a remote socket path and query parameters".to_string());
    }

    if suffix == "/" {
        return Err("remote socket path cannot be empty '/'".to_string());
    }

    Ok(Some(suffix.to_string()))
}

fn parse_tcp_query(query: &str) -> Result<String, String> {
    let tcp_addr = query.strip_prefix("tcp=").ok_or_else(|| {
        format!("unsupported SSH query parameter '{query}' (expected tcp=host:port)")
    })?;
    let (host, port_str) = tcp_addr
        .rsplit_once(':')
        .ok_or_else(|| "tcp query target must be in host:port form".to_string())?;
    let host_unbracketed = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    validate_ssh_token(host_unbracketed, "remote tcp host")?;
    let port = parse_port(port_str, "remote tcp")?;
    Ok(format!("{host}:{port}"))
}

/// Builds OpenSSH CLI arguments for a background port-forwarding tunnel.
pub fn build_ssh_forward_args(target: &SshTarget, local_port: u16) -> Vec<String> {
    let mut args = vec![
        "-N".to_string(),
        "-T".to_string(),
        "-o".to_string(),
        "ExitOnForwardFailure=yes".to_string(),
        "-o".to_string(),
        "ServerAliveInterval=15".to_string(),
        "-o".to_string(),
        "ServerAliveCountMax=3".to_string(),
        "-L".to_string(),
        format!("127.0.0.1:{local_port}:{}", target.remote_forward_spec()),
    ];
    if let Some(port) = target.port {
        args.push("-p".to_string());
        args.push(port.to_string());
    }
    args.push("--".to_string());
    args.push(target.display_label());
    args
}

/// Active OpenSSH local port-forwarding tunnel backed by a child `ssh` process.
pub struct SshTunnel {
    child: Child,
    local_endpoint: PathBuf,
    target: SshTarget,
}

impl SshTunnel {
    /// Reserves an ephemeral localhost TCP port, spawns `ssh -N -T -L ...`,
    /// and waits up to 10 seconds for the forwarded local port to accept connections.
    pub async fn start(target: SshTarget) -> std::io::Result<Self> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let local_port = listener.local_addr()?.port();
        drop(listener);

        let args = build_ssh_forward_args(&target, local_port);
        let mut cmd = Command::new("ssh");
        cmd.args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let child = cmd.spawn()?;
        let mut tunnel = Self {
            child,
            local_endpoint: PathBuf::from(format!("tcp://127.0.0.1:{local_port}")),
            target,
        };

        let deadline = tokio::time::Instant::now() + TUNNEL_READY_TIMEOUT;
        loop {
            match tunnel.child.try_wait() {
                Ok(Some(status)) => {
                    let stderr = read_child_stderr(&mut tunnel.child);
                    let detail = if stderr.is_empty() {
                        String::new()
                    } else {
                        format!(": {stderr}")
                    };
                    return Err(std::io::Error::other(format!(
                        "ssh tunnel to {} exited early ({status}){detail}",
                        tunnel.target.display_label()
                    )));
                }
                Ok(None) => {}
                Err(err) => return Err(err),
            }

            if let Ok(stream) = tokio::net::TcpStream::connect(("127.0.0.1", local_port)).await {
                drop(stream);
                return Ok(tunnel);
            }

            if tokio::time::Instant::now() >= deadline {
                let _ = tunnel.child.kill();
                let _ = tunnel.child.wait();
                let stderr = read_child_stderr(&mut tunnel.child);
                let detail = if stderr.is_empty() {
                    String::new()
                } else {
                    format!(": {stderr}")
                };
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!(
                        "timed out waiting for ssh tunnel to {}{detail}",
                        tunnel.target.display_label()
                    ),
                ));
            }

            tokio::time::sleep(TUNNEL_POLL_INTERVAL).await;
        }
    }

    /// Returns the local `tcp://127.0.0.1:<port>` endpoint that forwards to the remote daemon.
    pub fn local_endpoint(&self) -> &Path {
        &self.local_endpoint
    }

    /// Returns the parsed SSH target metadata.
    pub fn target(&self) -> &SshTarget {
        &self.target
    }
}

impl Drop for SshTunnel {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_child_stderr(child: &mut Child) -> String {
    let Some(mut stderr) = child.stderr.take() else {
        return String::new();
    };
    let mut buf = String::new();
    let _ = stderr.read_to_string(&mut buf);
    buf.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_ssh_targets() {
        let basic = SshTarget::parse("ssh://devbox").unwrap();
        assert_eq!(
            basic,
            SshTarget {
                user: None,
                host: "devbox".to_string(),
                port: None,
                remote_target: None,
            }
        );
        assert_eq!(basic.display_label(), "devbox");
        assert_eq!(basic.remote_forward_spec(), "127.0.0.1:7777");

        let full =
            SshTarget::parse("ssh://tim@10.0.0.5:2222/run/user/1000/daedalus/daedalusd.sock")
                .unwrap();
        assert_eq!(
            full,
            SshTarget {
                user: Some("tim".to_string()),
                host: "10.0.0.5".to_string(),
                port: Some(2222),
                remote_target: Some("/run/user/1000/daedalus/daedalusd.sock".to_string()),
            }
        );
        assert_eq!(full.display_label(), "tim@10.0.0.5");
        assert_eq!(
            full.remote_forward_spec(),
            "/run/user/1000/daedalus/daedalusd.sock"
        );

        let ipv6 = SshTarget::parse("ssh://alice@[::1]:2200/tmp/daedalusd.sock").unwrap();
        assert_eq!(
            ipv6,
            SshTarget {
                user: Some("alice".to_string()),
                host: "::1".to_string(),
                port: Some(2200),
                remote_target: Some("/tmp/daedalusd.sock".to_string()),
            }
        );
        assert_eq!(ipv6.display_label(), "alice@::1");

        let tcp_query = SshTarget::parse("ssh://alice@host.example?tcp=127.0.0.1:8888").unwrap();
        assert_eq!(tcp_query.remote_forward_spec(), "127.0.0.1:8888");
    }

    #[test]
    fn rejects_invalid_and_option_injection_ssh_targets() {
        for bad in [
            "tcp://127.0.0.1:7777",
            "/run/user/1000/daedalus/daedalusd.sock",
            "ssh://",
            "ssh://:22",
            "ssh://@host",
            "ssh://user@",
            "ssh://-oProxyCommand=bad",
            "ssh://alice@-evilhost",
            "ssh://-baduser@host",
            "ssh://user@host:0",
            "ssh://user@host:70000",
            "ssh://user@host:",
            "ssh://user@host/run/sock with space",
            "ssh://user@host\n-oEvil",
            "ssh://user@host/",
            "ssh://user@host?unknown=1",
            "ssh://user@::1:22",
            "ssh://user@[::1",
        ] {
            assert!(
                SshTarget::parse(bad).is_err(),
                "expected '{bad}' to be rejected"
            );
        }
    }

    #[test]
    fn builds_ssh_forward_args_with_port_and_double_dash() {
        let target_default = SshTarget::parse("ssh://devbox").unwrap();
        assert_eq!(
            build_ssh_forward_args(&target_default, 54321),
            vec![
                "-N",
                "-T",
                "-o",
                "ExitOnForwardFailure=yes",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=3",
                "-L",
                "127.0.0.1:54321:127.0.0.1:7777",
                "--",
                "devbox",
            ]
        );

        let target_full = SshTarget::parse(
            "ssh://tim@remote.internal:2222/run/user/1000/daedalus/daedalusd.sock",
        )
        .unwrap();
        assert_eq!(
            build_ssh_forward_args(&target_full, 49152),
            vec![
                "-N",
                "-T",
                "-o",
                "ExitOnForwardFailure=yes",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=3",
                "-L",
                "127.0.0.1:49152:/run/user/1000/daedalus/daedalusd.sock",
                "-p",
                "2222",
                "--",
                "tim@remote.internal",
            ]
        );
    }
}
