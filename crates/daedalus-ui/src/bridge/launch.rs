use daedalus_proto::remote::{SshTarget, SshTunnel};
use daedalus_proto::transport::IpcStream;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub(super) fn should_auto_spawn_daemon(
    auto_spawn_daemon: bool,
    spawn_attempted: bool,
    socket_path: &Path,
) -> bool {
    auto_spawn_daemon
        && ssh_uri(socket_path).is_none()
        && !spawn_attempted
        && matches!(
            daedalus_proto::transport::parse_endpoint(socket_path),
            daedalus_proto::transport::IpcEndpoint::Local(_)
        )
}

fn ssh_uri(endpoint: &Path) -> Option<&str> {
    endpoint.to_str().filter(|raw| raw.starts_with("ssh://"))
}

pub(super) async fn connect_endpoint(
    endpoint: &Path,
) -> std::io::Result<(IpcStream, Option<SshTunnel>)> {
    if let Some(uri) = ssh_uri(endpoint) {
        let target = SshTarget::parse(uri)
            .map_err(|message| std::io::Error::new(std::io::ErrorKind::InvalidInput, message))?;
        let tunnel = SshTunnel::start(target).await?;
        let stream = IpcStream::connect(tunnel.local_endpoint()).await?;
        Ok((stream, Some(tunnel)))
    } else {
        Ok((IpcStream::connect(endpoint).await?, None))
    }
}

fn daemon_binary_name() -> &'static str {
    #[cfg(windows)]
    {
        "daedalusd.exe"
    }
    #[cfg(not(windows))]
    {
        "daedalusd"
    }
}

fn resolve_daemon_binary(current_exe: Option<&Path>) -> PathBuf {
    let bin_name = daemon_binary_name();
    if let Some(exe) = current_exe
        && let Some(parent) = exe.parent()
    {
        let sibling = parent.join(bin_name);
        if is_executable_file(&sibling) {
            return sibling;
        }
    }
    PathBuf::from(bin_name)
}

pub(super) fn spawn_daemon_detached(
    socket_path: &Path,
    auth_token: Option<&str>,
) -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe().ok();
    let bin = resolve_daemon_binary(exe.as_deref());
    let _child = spawn_daemon_at(&bin, socket_path, auth_token)?;
    Ok(bin)
}

fn spawn_daemon_at(
    bin: &Path,
    socket_path: &Path,
    auth_token: Option<&str>,
) -> std::io::Result<std::process::Child> {
    let mut cmd = Command::new(bin);
    if let Some(token) = auth_token {
        cmd.env("DAEDALUS_AUTH_TOKEN", token);
    }
    cmd.arg("--socket")
        .arg(socket_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(unix)]
    {
        // SAFETY: `setsid()` is async-signal-safe per POSIX and detaches `daedalusd`
        // into its own session so it outlives the UI process cleanly.
        unsafe {
            cmd.pre_exec(|| {
                nix::unistd::setsid()
                    .map_err(|errno| std::io::Error::from_raw_os_error(errno as i32))?;
                Ok(())
            });
        }
    }

    #[cfg(windows)]
    {
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW);
    }

    cmd.spawn()
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && (meta.permissions().mode() & 0o111 != 0))
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;
    #[test]
    fn resolve_daemon_binary_prefers_sibling_executable() {
        let dir = TempDir::new().unwrap();
        let ui_name = if cfg!(windows) {
            "daedalus-ui.exe"
        } else {
            "daedalus-ui"
        };
        let fake_ui = dir.path().join(ui_name);
        fs::write(&fake_ui, "").unwrap();

        // Without sibling daedalusd -> falls back to PATH lookup name
        assert_eq!(
            resolve_daemon_binary(Some(&fake_ui)),
            PathBuf::from(daemon_binary_name())
        );

        // With executable sibling daedalusd -> returns sibling path
        let sibling = dir.path().join(daemon_binary_name());
        fs::write(&sibling, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        fs::set_permissions(&sibling, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(resolve_daemon_binary(Some(&fake_ui)), sibling);
    }

    #[test]
    fn auto_spawn_only_for_local_endpoints() {
        assert!(!should_auto_spawn_daemon(
            true,
            false,
            Path::new("ssh://host")
        ));
        assert!(!should_auto_spawn_daemon(
            true,
            false,
            Path::new("tcp://127.0.0.1:7777")
        ));
        assert!(!should_auto_spawn_daemon(
            false,
            false,
            Path::new("/tmp/daedalusd.sock")
        ));
        assert!(!should_auto_spawn_daemon(
            true,
            true,
            Path::new("/tmp/daedalusd.sock")
        ));
        assert!(should_auto_spawn_daemon(
            true,
            false,
            Path::new("/tmp/daedalusd.sock")
        ));
        assert!(should_auto_spawn_daemon(
            true,
            false,
            Path::new(r"\\.\pipe\daedalus-alice-daedalusd")
        ));
    }
    #[tokio::test]
    async fn invalid_ssh_target_fails_before_launching_a_tunnel() {
        let error = match connect_endpoint(Path::new("ssh://")).await {
            Ok(_) => panic!("empty SSH target was accepted"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[cfg(unix)]
    #[test]
    fn detached_daemon_spawn_preserves_the_socket_and_token() {
        let temp = TempDir::new().unwrap();
        let binary = temp.path().join("daemon-fixture");
        let marker = temp.path().join("started");
        fs::write(
            &binary,
            "#!/bin/sh
[ \"$1\" = --socket ] && [ \"$DAEDALUS_AUTH_TOKEN\" = fixture-auth ] || exit 3
printf started > \"$2\"
",
        )
        .unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        let mut child = spawn_daemon_at(&binary, &marker, Some("fixture-auth")).unwrap();
        assert!(child.wait().unwrap().success());
        assert_eq!(fs::read_to_string(&marker).unwrap(), "started");
        assert_eq!(
            spawn_daemon_at(&temp.path().join("missing"), &marker, None)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::NotFound
        );
    }
}
