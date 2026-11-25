#[cfg(unix)]
use crate::process::command::run_command_until;
#[cfg(any(target_os = "linux", windows))]
use crate::process::command::run_command_with_timeout;
#[cfg(target_os = "linux")]
use crate::process::inventory::pgid_has_non_zombie_proc;
pub use crate::process::inventory::{
    collect_descendant_pids, descendants_from_ppid_edges, parse_ps_ppid_table,
};
use crate::shell_integration::ShellLaunchSpec;
use daedalus_proto::{PaneId, PaneRole, WorkspaceId};
#[cfg(unix)]
use nix::sys::signal::{Signal, killpg};
#[cfg(unix)]
use nix::unistd::Pid;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
#[cfg(unix)]
use std::process::Command;
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

#[cfg(unix)]
const PANE_TEARDOWN_TIMEOUT: Duration = Duration::from_secs(3);
#[cfg(unix)]
const SYSTEMD_CALL_TIMEOUT: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolationMode {
    SystemdScope,
    PgidFallback,
    WindowsProcessTree,
}

impl IsolationMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SystemdScope => "systemd-scope",
            Self::PgidFallback => "pgid-fallback",
            Self::WindowsProcessTree => "windows-process-tree",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Supervisor {
    mode: IsolationMode,
}

pub struct ProcessHandle {
    child: Box<dyn Child + Send + Sync>,
    pid: Option<u32>,
    #[cfg_attr(not(unix), allow(dead_code))]
    systemd_unit: Option<String>,
    torn_down: bool,
}

pub struct SpawnedPty {
    pub direct: bool,
    pub launch_preset: Option<String>,
    pub master: Box<dyn MasterPty + Send>,
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
    pub handle: ProcessHandle,
}

#[cfg(target_os = "linux")]
pub fn detect_isolation_mode() -> IsolationMode {
    let mut cmd = Command::new("systemd-run");
    cmd.args(["--user", "--scope", "--quiet", "--", "true"]);
    if run_command_with_timeout(&mut cmd, Duration::from_secs(2)) {
        IsolationMode::SystemdScope
    } else {
        tracing::warn!(
            "systemd user session unavailable or timed out; falling back to setsid + process-group (killpg) isolation"
        );
        IsolationMode::PgidFallback
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
pub fn detect_isolation_mode() -> IsolationMode {
    IsolationMode::PgidFallback
}

#[cfg(windows)]
pub fn detect_isolation_mode() -> IsolationMode {
    IsolationMode::WindowsProcessTree
}

impl Supervisor {
    pub fn detect() -> Self {
        Self {
            mode: detect_isolation_mode(),
        }
    }

    pub fn with_mode(mode: IsolationMode) -> Self {
        Self { mode }
    }

    pub fn isolation_mode(&self) -> IsolationMode {
        self.mode
    }

    #[allow(clippy::too_many_arguments)]
    pub fn spawn_pty(
        &self,
        workspace_id: WorkspaceId,
        pane_id: PaneId,
        role: PaneRole,
        cwd: &Path,
        cols: u16,
        rows: u16,
        launch: &ShellLaunchSpec,
        env: &HashMap<String, String>,
    ) -> Result<SpawnedPty, String> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: rows.max(1),
                cols: cols.max(1),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("openpty failed: {e}"))?;

        let (mut cmd, systemd_unit) = match self.mode {
            IsolationMode::SystemdScope => {
                let unit = format!("daedalus-{}-{}", workspace_id.short(), pane_id.short());
                let mut cb = CommandBuilder::new("systemd-run");
                cb.args([
                    "--user",
                    "--scope",
                    "--quiet",
                    &format!("--unit={unit}"),
                    &format!("--working-directory={}", cwd.display()),
                    "--",
                ]);
                #[cfg(target_os = "linux")]
                {
                    let search_path = launch
                        .extra_env
                        .get("PATH")
                        .or_else(|| env.get("PATH"))
                        .map(std::ffi::OsStr::new)
                        .or_else(|| cb.get_env("PATH"));
                    cb.arg(resolve_scope_executable(&launch.program, cwd, search_path)?);
                }
                #[cfg(not(target_os = "linux"))]
                cb.arg(&launch.program);
                for arg in &launch.args {
                    cb.arg(arg);
                }
                (cb, Some(format!("{unit}.scope")))
            }
            IsolationMode::PgidFallback | IsolationMode::WindowsProcessTree => {
                let mut cb = CommandBuilder::new(&launch.program);
                for arg in &launch.args {
                    cb.arg(arg);
                }
                cb.cwd(cwd);
                (cb, None)
            }
        };

        cmd.env("TERM", "xterm-256color");
        cmd.env("DAEDALUS_WORKSPACE_ID", workspace_id.to_string());
        cmd.env("DAEDALUS_PANE_ID", pane_id.to_string());
        cmd.env("DAEDALUS_PANE_ROLE", role.to_string());
        for (k, v) in env {
            cmd.env(k, v);
        }
        for (k, v) in &launch.extra_env {
            cmd.env(k, v);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("spawn_command failed: {e}"))?;
        let pid = child.process_id();

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("clone master reader failed: {e}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| format!("take master writer failed: {e}"))?;

        let launch_preset = match role {
            PaneRole::Agent => daedalus_proto::agent_launch_preset(&launch.program)
                .or((!launch.direct).then_some("shell")),
            PaneRole::Runner if launch.direct => daedalus_proto::cargo_launch_preset(
                &launch.program,
                launch.args.first().map(String::as_str),
            ),
            _ => None,
        }
        .map(str::to_owned);
        Ok(SpawnedPty {
            direct: launch.direct,
            launch_preset,
            master: pair.master,
            reader,
            writer,
            handle: ProcessHandle {
                child,
                pid,
                systemd_unit,
                torn_down: false,
            },
        })
    }
}

// portable-pty starts systemd-run here, so resolve its child on the daemon
// before acknowledging the spawn. Relative PATH entries belong to the workspace.
#[cfg(target_os = "linux")]
fn resolve_scope_executable(
    program: &str,
    cwd: &Path,
    search_path: Option<&std::ffi::OsStr>,
) -> Result<std::path::PathBuf, String> {
    use nix::unistd::{AccessFlags, access};
    let cwd = std::path::absolute(cwd).map_err(|error| error.to_string())?;
    let executable = |path: &Path| path.is_file() && access(path, AccessFlags::X_OK).is_ok();
    if program.contains('/') {
        let path = cwd.join(program);
        if executable(&path) {
            return Ok(path);
        }
    } else if let Some(search_path) = search_path {
        for dir in std::env::split_paths(search_path) {
            let path = cwd.join(dir).join(program);
            if executable(&path) {
                return Ok(path);
            }
        }
    }
    Err(format!(
        "program '{program}' not found or not executable in {}",
        cwd.display()
    ))
}

impl ProcessHandle {
    pub fn root_pid(&self) -> Option<u32> {
        self.pid
    }

    pub fn try_wait_exit(&mut self) -> Option<i32> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(status.exit_code() as i32),
            _ => None,
        }
    }

    pub fn wait_exit(&mut self) -> Option<i32> {
        self.child.wait().ok().map(|s| s.exit_code() as i32)
    }

    #[cfg(unix)]
    pub fn teardown(&mut self) {
        self.teardown_with_systemctl(Path::new("systemctl"), PANE_TEARDOWN_TIMEOUT);
    }

    #[cfg(unix)]
    fn teardown_with_systemctl(&mut self, systemctl: &Path, timeout: Duration) {
        if self.torn_down {
            return;
        }
        self.torn_down = true;

        let deadline = Instant::now() + timeout;
        // Keep time for a final KILL and reset even if the liveness probe stalls.
        let grace_deadline = if self.systemd_unit.is_some() {
            deadline - (SYSTEMD_CALL_TIMEOUT * 2).min(timeout / 2)
        } else {
            deadline
        };
        if self.try_wait_exit().is_some() && !self.has_live_processes(systemctl, grace_deadline) {
            self.reset_failed_unit(systemctl, deadline);
            return;
        }

        self.send_signal(Signal::SIGTERM, systemctl, grace_deadline);
        while Instant::now() < grace_deadline {
            if self.try_wait_exit().is_some() && !self.has_live_processes(systemctl, grace_deadline)
            {
                self.reset_failed_unit(systemctl, deadline);
                return;
            }
            std::thread::sleep(
                grace_deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(50)),
            );
        }

        self.send_signal(Signal::SIGKILL, systemctl, deadline);
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.reset_failed_unit(systemctl, deadline);
    }

    #[cfg(windows)]
    pub fn teardown(&mut self) {
        if self.torn_down {
            return;
        }
        self.torn_down = true;

        if self.try_wait_exit().is_some() {
            return;
        }

        if let Some(pid) = self.pid {
            let mut cmd = crate::background_command("taskkill");
            cmd.args(["/F", "/T", "/PID", &pid.to_string()]);
            let _ = run_command_with_timeout(&mut cmd, Duration::from_secs(3));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    #[cfg(unix)]
    fn has_live_processes(&self, systemctl: &Path, deadline: Instant) -> bool {
        self.has_live_pgid_processes()
            || self.systemd_unit.as_deref().is_some_and(|unit| {
                // An unavailable status leaves the scope's liveness unknown.
                // Try the final signal before treating it as closed.
                run_systemctl_quiet(systemctl, &["is-active", "--quiet", unit], deadline)
                    .is_none_or(|status| status.success())
            })
    }

    #[cfg(unix)]
    fn has_live_pgid_processes(&self) -> bool {
        let Some(pid) = self.pid else {
            return false;
        };
        let pgid = pid as i32;
        if killpg(Pid::from_raw(pgid), None).is_err() {
            return false;
        }
        #[cfg(target_os = "linux")]
        {
            // In container environments where PID 1 does not reap reparented zombies,
            // killpg(..., None) still succeeds if only zombies remain in the group.
            pgid_has_non_zombie_proc(pgid)
        }
        #[cfg(not(target_os = "linux"))]
        {
            true
        }
    }

    #[cfg(unix)]
    fn send_signal(&self, sig: Signal, systemctl: &Path, deadline: Instant) {
        if let Some(pid) = self.pid {
            let _ = killpg(Pid::from_raw(pid as i32), sig);
        }
        let sig_name = match sig {
            Signal::SIGTERM => "SIGTERM",
            Signal::SIGKILL => "SIGKILL",
            _ => "SIGTERM",
        };
        if let Some(unit) = &self.systemd_unit {
            let _ = run_systemctl_quiet(systemctl, &["kill", "-s", sig_name, unit], deadline);
        }
    }

    #[cfg(unix)]
    fn reset_failed_unit(&self, systemctl: &Path, deadline: Instant) {
        if let Some(unit) = &self.systemd_unit {
            let _ = run_systemctl_quiet(systemctl, &["reset-failed", unit], deadline);
        }
    }
}

#[cfg(unix)]
fn run_systemctl_quiet(
    systemctl: &Path,
    args: &[&str],
    deadline: Instant,
) -> Option<std::process::ExitStatus> {
    let deadline = deadline.min(Instant::now() + SYSTEMD_CALL_TIMEOUT);
    run_command_until(Command::new(systemctl).arg("--user").args(args), deadline)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use crate::process::inventory::parse_proc_stat;
    #[cfg(unix)]
    use nix::sys::signal::kill;
    #[cfg(unix)]
    use std::io::BufRead;
    #[cfg(unix)]
    use tempfile::TempDir;

    #[cfg(target_os = "linux")]
    #[test]
    fn systemd_resolves_program_using_workspace_path_and_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let program = bin.join("workspace-agent");
        let path = std::ffi::OsStr::new("bin");
        assert!(resolve_scope_executable("workspace-agent", dir.path(), Some(path)).is_err());
        std::fs::write(&program, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(resolve_scope_executable("workspace-agent", dir.path(), Some(path)).is_err());
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            resolve_scope_executable("workspace-agent", dir.path(), Some(path)).unwrap(),
            program
        );
        assert_eq!(
            resolve_scope_executable("./bin/workspace-agent", dir.path(), None).unwrap(),
            program
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn scope_launcher_preserves_project_command_arguments_and_launch_environment() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let runner = dir.path().join("systemd-run");
        let marker = dir.path().join("args");
        std::fs::write(
            &runner,
            r#"#!/bin/sh
printf '%s\n' "$@" "$CUSTOM_ENV" "$DAEDALUS_PANE_ID" > "$MARKER"
exit 17
"#,
        )
        .unwrap();
        std::fs::set_permissions(&runner, std::fs::Permissions::from_mode(0o755)).unwrap();
        let ws = WorkspaceId::new();
        let pane = PaneId::new();
        let mut spawned = Supervisor::with_mode(IsolationMode::SystemdScope)
            .spawn_pty(
                ws,
                pane,
                PaneRole::Agent,
                dir.path(),
                0,
                0,
                &ShellLaunchSpec {
                    direct: false,
                    program: "/bin/sh".into(),
                    args: vec!["argument with spaces".into()],
                    extra_env: HashMap::from([("CUSTOM_ENV".into(), "launch-specific".into())]),
                },
                &HashMap::from([
                    ("MARKER".into(), marker.to_str().unwrap().into()),
                    ("PATH".into(), dir.path().to_str().unwrap().into()),
                    ("CUSTOM_ENV".into(), "workspace".into()),
                ]),
            )
            .unwrap();
        assert_eq!(spawned.handle.wait_exit(), Some(17));
        let args = std::fs::read_to_string(marker).unwrap();
        assert_eq!(
            args.lines().collect::<Vec<_>>(),
            [
                "--user",
                "--scope",
                "--quiet",
                &format!("--unit=daedalus-{}-{}", ws.short(), pane.short()),
                &format!("--working-directory={}", dir.path().display()),
                "--",
                "/bin/sh",
                "argument with spaces",
                "launch-specific",
                &pane.to_string(),
            ]
        );
    }

    #[cfg(target_os = "linux")]
    fn is_process_alive(pid: i32) -> bool {
        if kill(Pid::from_raw(pid), None).is_err() {
            return false;
        }
        if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            && let Some((state, _)) = parse_proc_stat(&stat)
        {
            return !matches!(state, 'Z' | 'X');
        }
        true
    }

    #[cfg(all(unix, not(target_os = "linux")))]
    fn is_process_alive(pid: i32) -> bool {
        kill(Pid::from_raw(pid), None).is_ok()
    }

    #[cfg(unix)]
    fn assert_orphan_killed(script: &str) {
        let dir = TempDir::new().unwrap();
        let supervisor = Supervisor::with_mode(IsolationMode::PgidFallback);
        let launch = ShellLaunchSpec {
            direct: false,
            program: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), script.to_string()],
            extra_env: HashMap::new(),
        };

        let mut spawned = supervisor
            .spawn_pty(
                WorkspaceId::new(),
                PaneId::new(),
                PaneRole::Service,
                dir.path(),
                80,
                24,
                &launch,
                &HashMap::new(),
            )
            .unwrap();

        let mut reader = std::io::BufReader::new(spawned.reader);
        let mut line = String::new();
        let child_pid: i32 = loop {
            line.clear();
            let n = reader.read_line(&mut line).unwrap();
            assert!(n > 0, "PTY closed before emitting CHILD_PID");
            if let Some(rest) = line.trim().strip_prefix("CHILD_PID=") {
                break rest.trim().parse().unwrap();
            }
        };

        // Verify background sleep is alive
        assert!(is_process_alive(child_pid));

        drop(spawned.writer);
        drop(reader);
        drop(spawned.master);
        spawned.handle.teardown();

        // Background sleep must be dead (ESRCH or unreaped zombie in container PID ns)
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if !is_process_alive(child_pid) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("orphan child process {child_pid} survived teardown");
    }

    #[cfg(unix)]
    #[test]
    fn teardown_kills_background_orphan_descendants() {
        assert_orphan_killed("sleep 60 & echo CHILD_PID=$!; wait");
    }

    #[cfg(unix)]
    #[test]
    fn teardown_kills_sighup_immune_orphan_descendants() {
        assert_orphan_killed("(trap '' HUP; exec sleep 60) & echo CHILD_PID=$!; wait");
    }

    #[cfg(target_os = "linux")]
    const SLOW_SYSTEMCTL_SIGNAL: &str =
        "[ \"$2\" != kill ] || exec /bin/sleep 4\n[ \"$2\" != is-active ]\n";

    #[cfg(target_os = "linux")]
    fn teardown_against_fake_systemctl(script: &str, systemctl_script: &str) -> String {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let systemctl = dir.path().join("systemctl");
        std::fs::write(
            &systemctl,
            format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"${{0%/*}}/trace\"\n{systemctl_script}"),
        )
        .unwrap();
        std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut pane = Supervisor::with_mode(IsolationMode::PgidFallback)
            .spawn_pty(
                WorkspaceId::new(),
                PaneId::new(),
                PaneRole::Service,
                dir.path(),
                80,
                24,
                &ShellLaunchSpec {
                    direct: true,
                    program: "/bin/sh".into(),
                    args: vec!["-c".into(), script.into()],
                    extra_env: HashMap::new(),
                },
                &HashMap::new(),
            )
            .unwrap();
        const UNIT: &str = "daedalus-test.scope";
        pane.handle.systemd_unit = Some(UNIT.into());
        let mut ready = String::new();
        std::io::BufReader::new(pane.reader)
            .read_line(&mut ready)
            .unwrap();
        assert_eq!(ready.trim(), "ready");
        let pid = pane.handle.root_pid().unwrap() as i32;
        let start = Instant::now();
        pane.handle
            .teardown_with_systemctl(&systemctl, Duration::from_secs(1));
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "systemctl exceeded the shared teardown budget: {:?}",
            start.elapsed()
        );
        assert!(!is_process_alive(pid));
        std::fs::read_to_string(dir.path().join("trace")).unwrap()
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn systemd_teardown_bounds_slow_helpers_on_normal_exit() {
        let trace = teardown_against_fake_systemctl(
            "echo ready; exec /bin/sleep 30",
            SLOW_SYSTEMCTL_SIGNAL,
        );
        assert!(trace.contains("--user kill -s SIGTERM"));
        assert!(trace.contains("--user is-active --quiet"));
        assert!(trace.contains("--user reset-failed"));
        assert!(!trace.contains("SIGKILL"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn systemd_teardown_reserves_time_to_escalate_a_term_resistant_pane() {
        let trace = teardown_against_fake_systemctl(
            "trap '' TERM; echo ready; exec /bin/sleep 30",
            SLOW_SYSTEMCTL_SIGNAL,
        );
        assert!(trace.contains("--user kill -s SIGTERM"));
        assert!(trace.contains("--user kill -s SIGKILL"));
        assert!(trace.contains("--user reset-failed"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn systemd_teardown_escalates_when_scope_status_is_unavailable() {
        let trace = teardown_against_fake_systemctl(
            "echo ready; exec /bin/sleep 30",
            "[ \"$2\" = kill ] || exec /bin/sleep 4\n",
        );
        assert!(trace.contains("--user kill -s SIGTERM"));
        assert!(trace.contains("--user is-active --quiet"));
        assert!(trace.contains("--user kill -s SIGKILL"));
        assert!(trace.contains("--user reset-failed"));
    }

    #[test]
    fn isolation_mode_as_str_matches_expected_names() {
        assert_eq!(IsolationMode::SystemdScope.as_str(), "systemd-scope");
        assert_eq!(IsolationMode::PgidFallback.as_str(), "pgid-fallback");
        assert_eq!(
            IsolationMode::WindowsProcessTree.as_str(),
            "windows-process-tree"
        );
    }

    #[cfg(all(unix, not(target_os = "linux")))]
    #[test]
    fn detect_isolation_mode_uses_pgid_fallback_on_non_linux() {
        assert_eq!(detect_isolation_mode(), IsolationMode::PgidFallback);
        assert_eq!(
            Supervisor::detect().isolation_mode(),
            IsolationMode::PgidFallback
        );
    }

    #[cfg(windows)]
    #[test]
    fn detect_isolation_mode_uses_windows_process_tree_on_windows() {
        assert_eq!(detect_isolation_mode(), IsolationMode::WindowsProcessTree);
        assert_eq!(
            Supervisor::detect().isolation_mode(),
            IsolationMode::WindowsProcessTree
        );
    }
}
