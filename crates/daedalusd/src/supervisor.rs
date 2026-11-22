use crate::shell_integration::ShellLaunchSpec;
use daedalus_proto::{PaneId, PaneRole, WorkspaceId};
#[cfg(unix)]
use nix::sys::signal::{Signal, killpg};
#[cfg(unix)]
use nix::unistd::Pid;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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
                    &launch.program,
                ]);
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

        Ok(SpawnedPty {
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
        if self.torn_down {
            return;
        }
        self.torn_down = true;

        if self.try_wait_exit().is_some() && !self.has_live_processes() {
            self.reset_failed_unit();
            return;
        }

        self.send_signal(Signal::SIGTERM);
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if self.try_wait_exit().is_some() && !self.has_live_processes() {
                self.reset_failed_unit();
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        self.send_signal(Signal::SIGKILL);
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.reset_failed_unit();
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
            let mut cmd = Command::new("taskkill");
            cmd.args(["/F", "/T", "/PID", &pid.to_string()]);
            let _ = run_command_with_timeout(&mut cmd, Duration::from_secs(3));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    #[cfg(unix)]
    fn has_live_processes(&self) -> bool {
        self.has_live_pgid_processes() || self.has_active_systemd_unit()
    }

    #[cfg(unix)]
    fn has_active_systemd_unit(&self) -> bool {
        self.systemd_unit
            .as_deref()
            .is_some_and(|unit| run_systemctl_quiet(&["is-active", "--quiet", unit]))
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
    fn send_signal(&self, sig: Signal) {
        let sig_name = match sig {
            Signal::SIGTERM => "SIGTERM",
            Signal::SIGKILL => "SIGKILL",
            _ => "SIGTERM",
        };
        if let Some(unit) = &self.systemd_unit {
            let _ = run_systemctl_quiet(&["kill", "-s", sig_name, unit]);
        }
        if let Some(pid) = self.pid {
            let _ = killpg(Pid::from_raw(pid as i32), sig);
        }
    }

    #[cfg(unix)]
    fn reset_failed_unit(&self) {
        if let Some(unit) = &self.systemd_unit {
            let _ = run_systemctl_quiet(&["reset-failed", unit]);
        }
    }
}

#[cfg_attr(all(unix, not(target_os = "linux")), allow(dead_code))]
fn run_command_with_timeout(cmd: &mut Command, timeout: Duration) -> bool {
    let Ok(mut child) = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

#[cfg(unix)]
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub(crate) fn run_command_stdout_with_timeout(
    cmd: &mut Command,
    timeout: Duration,
    allow_nonzero_exit: bool,
) -> Option<String> {
    use std::os::unix::process::CommandExt;

    let mut child = cmd
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let pgid = Pid::from_raw(child.id() as i32);
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut stdout, &mut buf).ok()?;
        Some(String::from_utf8_lossy(&buf).into_owned())
    });

    let deadline = Instant::now() + timeout;
    let completed = loop {
        match child.try_wait() {
            Ok(Some(status)) => break allow_nonzero_exit || status.success(),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = killpg(pgid, Signal::SIGKILL);
                    let _ = child.kill();
                    let _ = child.wait();
                    break false;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => {
                let _ = killpg(pgid, Signal::SIGKILL);
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
        }
    };

    if !completed {
        return None;
    }

    reader.join().ok().flatten()
}

#[cfg(windows)]
pub(crate) fn run_command_stdout_with_timeout(
    cmd: &mut Command,
    timeout: Duration,
    allow_nonzero_exit: bool,
) -> Option<String> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut stdout, &mut buf).ok()?;
        Some(String::from_utf8_lossy(&buf).into_owned())
    });

    let deadline = Instant::now() + timeout;
    let completed = loop {
        match child.try_wait() {
            Ok(Some(status)) => break allow_nonzero_exit || status.success(),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break false;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
        }
    };

    if !completed {
        return None;
    }

    reader.join().ok().flatten()
}

#[cfg(unix)]
fn run_systemctl_quiet(args: &[&str]) -> bool {
    Command::new("systemctl")
        .arg("--user")
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Parses `ps -axo pid=,ppid=` output into `(pid, ppid)` pairs, ignoring headers and malformed lines.
pub fn parse_ps_ppid_table(output: &str) -> Vec<(u32, u32)> {
    let mut edges = Vec::new();
    for line in output.lines() {
        let mut cols = line.split_whitespace();
        let (Some(pid_str), Some(ppid_str), None) = (cols.next(), cols.next(), cols.next()) else {
            continue;
        };
        let (Ok(pid), Ok(ppid)) = (pid_str.parse::<u32>(), ppid_str.parse::<u32>()) else {
            continue;
        };
        if pid == 0 {
            continue;
        }
        edges.push((pid, ppid));
    }
    edges
}

/// Computes all descendant PIDs of `root` from `(pid, ppid)` edges.
pub fn descendants_from_ppid_edges(root: u32, edges: &[(u32, u32)]) -> Vec<u32> {
    if root == 0 {
        return Vec::new();
    }

    let mut children_by_ppid: HashMap<u32, Vec<u32>> = HashMap::new();
    for &(pid, ppid) in edges {
        if pid > 0 {
            children_by_ppid.entry(ppid).or_default().push(pid);
        }
    }
    for children in children_by_ppid.values_mut() {
        children.sort_unstable();
        children.dedup();
    }

    let mut visited = HashSet::from([root]);
    let mut queue = VecDeque::from([root]);
    let mut descendants = Vec::new();

    while let Some(pid) = queue.pop_front() {
        if let Some(children) = children_by_ppid.get(&pid) {
            for &child in children {
                if visited.insert(child) {
                    descendants.push(child);
                    queue.push_back(child);
                }
            }
        }
    }

    descendants
}

/// Collects all live descendant PIDs of `root_pid`.
#[cfg(target_os = "linux")]
pub fn collect_descendant_pids(root_pid: u32) -> Vec<u32> {
    if root_pid == 0 {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut edges = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if !name_str.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        if let Some(edge) = crate::runtime_monitor::parse_proc_stat_pid_ppid(&stat) {
            edges.push(edge);
        }
    }
    descendants_from_ppid_edges(root_pid, &edges)
}

/// Collects all live descendant PIDs of `root_pid` using `ps -axo pid=,ppid=`.
#[cfg(all(unix, not(target_os = "linux")))]
pub fn collect_descendant_pids(root_pid: u32) -> Vec<u32> {
    if root_pid == 0 {
        return Vec::new();
    }
    let mut cmd = Command::new("ps");
    cmd.args(["-axo", "pid=,ppid="]);
    let Some(output) = run_command_stdout_with_timeout(&mut cmd, Duration::from_secs(2), false)
    else {
        return Vec::new();
    };
    let edges = parse_ps_ppid_table(&output);
    descendants_from_ppid_edges(root_pid, &edges)
}

#[cfg(windows)]
pub(crate) fn collect_windows_ppid_edges() -> Vec<(u32, u32)> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let mut cmd = Command::new("powershell.exe");
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        r#"Get-CimInstance Win32_Process | ForEach-Object { "$($_.ProcessId) $($_.ParentProcessId)" }"#,
    ]);
    cmd.creation_flags(CREATE_NO_WINDOW);
    let Some(output) = run_command_stdout_with_timeout(&mut cmd, Duration::from_secs(2), false)
    else {
        return Vec::new();
    };
    parse_ps_ppid_table(&output)
}

/// Collects all live descendant PIDs of `root_pid` using `Win32_Process`.
#[cfg(windows)]
pub fn collect_descendant_pids(root_pid: u32) -> Vec<u32> {
    if root_pid == 0 {
        return Vec::new();
    }
    let edges = collect_windows_ppid_edges();
    descendants_from_ppid_edges(root_pid, &edges)
}

#[cfg(target_os = "linux")]
fn parse_proc_stat(stat: &str) -> Option<(char, i32)> {
    let after_comm = stat.rfind(')')?;
    let mut parts = stat[after_comm + 1..].split_whitespace();
    let state = parts.next()?.chars().next()?;
    let _ppid = parts.next()?;
    let pgrp: i32 = parts.next()?.parse().ok()?;
    Some((state, pgrp))
}

#[cfg(target_os = "linux")]
fn pgid_has_non_zombie_proc(pgid: i32) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return true;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if !name_str.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        if let Some((state, pgrp)) = parse_proc_stat(&stat)
            && pgrp == pgid
            && !matches!(state, 'Z' | 'X')
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use nix::sys::signal::kill;
    #[cfg(unix)]
    use std::io::BufRead;
    #[cfg(unix)]
    use tempfile::TempDir;

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

    #[cfg(unix)]
    #[test]
    fn command_probe_respects_timeout_and_kills_hanging_process() {
        let mut ok_cmd = Command::new("/bin/sh");
        ok_cmd.args(["-c", "true"]);
        assert!(run_command_with_timeout(
            &mut ok_cmd,
            Duration::from_secs(2)
        ));

        let mut fail_cmd = Command::new("/bin/sh");
        fail_cmd.args(["-c", "false"]);
        assert!(!run_command_with_timeout(
            &mut fail_cmd,
            Duration::from_secs(2)
        ));

        let start = Instant::now();
        let mut hang_cmd = Command::new("/bin/sh");
        hang_cmd.args(["-c", "sleep 10"]);
        assert!(!run_command_with_timeout(
            &mut hang_cmd,
            Duration::from_millis(150)
        ));
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "timeout helper took too long: {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn parse_ps_ppid_table_ignores_headers_and_malformed_lines() {
        let sample = "\
  PID  PPID
    0     0
    1     0
  100     1
  205   100
bad line
  300   abc
  400
  500   205   extra
  600   205
";
        assert_eq!(
            parse_ps_ppid_table(sample),
            vec![(1, 0), (100, 1), (205, 100), (600, 205)]
        );
    }

    #[test]
    fn descendants_from_ppid_edges_collects_subtree_and_handles_cycles() {
        let edges = vec![
            (10, 1),
            (20, 10),
            (30, 10),
            (40, 20),
            (50, 40),
            (60, 1),
            // Self-loop and cycle should not loop infinitely or re-include root
            (10, 20),
            (70, 70),
        ];
        assert_eq!(
            descendants_from_ppid_edges(10, &edges),
            vec![20, 30, 40, 50]
        );
        assert!(descendants_from_ppid_edges(999, &edges).is_empty());
        assert!(descendants_from_ppid_edges(0, &edges).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn collect_descendant_pids_finds_live_child_process() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "sleep 5"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn child");

        let child_pid = child.id();
        let descendants = collect_descendant_pids(std::process::id());
        assert!(
            descendants.contains(&child_pid),
            "expected {child_pid} in {descendants:?}"
        );

        let _ = child.kill();
        let _ = child.wait();
    }

    #[cfg(unix)]
    #[test]
    fn command_stdout_with_timeout_captures_output_and_kills_hanging_process() {
        let mut ok_cmd = Command::new("/bin/sh");
        ok_cmd.args(["-c", "printf 'hello\\n'"]);
        assert_eq!(
            run_command_stdout_with_timeout(&mut ok_cmd, Duration::from_secs(2), false),
            Some("hello\n".to_string())
        );

        let mut nonzero_cmd = Command::new("/bin/sh");
        nonzero_cmd.args(["-c", "printf 'partial'; exit 1"]);
        assert_eq!(
            run_command_stdout_with_timeout(&mut nonzero_cmd, Duration::from_secs(2), false),
            None
        );

        let mut allowed_nonzero_cmd = Command::new("/bin/sh");
        allowed_nonzero_cmd.args(["-c", "printf 'partial'; exit 1"]);
        assert_eq!(
            run_command_stdout_with_timeout(&mut allowed_nonzero_cmd, Duration::from_secs(2), true),
            Some("partial".to_string())
        );

        let start = Instant::now();
        let mut hang_cmd = Command::new("/bin/sh");
        hang_cmd.args(["-c", "sleep 10"]);
        assert_eq!(
            run_command_stdout_with_timeout(&mut hang_cmd, Duration::from_millis(150), true),
            None
        );
        assert!(start.elapsed() < Duration::from_secs(2));
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
