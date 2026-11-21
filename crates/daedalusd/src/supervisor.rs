use crate::shell_integration::ShellLaunchSpec;
use daedalus_proto::{PaneId, PaneRole, WorkspaceId};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tracing::warn;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolationMode {
    SystemdScope,
    PgidFallback,
}

impl IsolationMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SystemdScope => "systemd-scope",
            Self::PgidFallback => "pgid-fallback",
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
    systemd_unit: Option<String>,
    torn_down: bool,
}

pub struct SpawnedPty {
    pub master: Box<dyn MasterPty + Send>,
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
    pub handle: ProcessHandle,
}

impl Supervisor {
    pub fn detect() -> Self {
        let ok = Command::new("systemd-run")
            .args(["--user", "--scope", "--quiet", "--", "true"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        if ok {
            Self {
                mode: IsolationMode::SystemdScope,
            }
        } else {
            warn!(
                "systemd user session unavailable; falling back to setsid + process-group (killpg) isolation"
            );
            Self {
                mode: IsolationMode::PgidFallback,
            }
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
            IsolationMode::PgidFallback => {
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
    pub fn try_wait_exit(&mut self) -> Option<i32> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(status.exit_code() as i32),
            _ => None,
        }
    }

    pub fn wait_exit(&mut self) -> Option<i32> {
        self.child.wait().ok().map(|s| s.exit_code() as i32)
    }

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

    fn has_live_processes(&self) -> bool {
        self.has_live_pgid_processes() || self.has_active_systemd_unit()
    }

    fn has_active_systemd_unit(&self) -> bool {
        self.systemd_unit
            .as_deref()
            .is_some_and(|unit| run_systemctl_quiet(&["is-active", "--quiet", unit]))
    }

    fn has_live_pgid_processes(&self) -> bool {
        let Some(pid) = self.pid else {
            return false;
        };
        let pgid = pid as i32;
        if killpg(Pid::from_raw(pgid), None).is_err() {
            return false;
        }
        // In container environments where PID 1 does not reap reparented zombies,
        // killpg(..., None) still succeeds if only zombies remain in the group.
        pgid_has_non_zombie_proc(pgid)
    }

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

    fn reset_failed_unit(&self) {
        if let Some(unit) = &self.systemd_unit {
            let _ = run_systemctl_quiet(&["reset-failed", unit]);
        }
    }
}

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

fn parse_proc_stat(stat: &str) -> Option<(char, i32)> {
    let after_comm = stat.rfind(')')?;
    let mut parts = stat[after_comm + 1..].split_whitespace();
    let state = parts.next()?.chars().next()?;
    let _ppid = parts.next()?;
    let pgrp: i32 = parts.next()?.parse().ok()?;
    Some((state, pgrp))
}

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
    use nix::sys::signal::kill;
    use std::io::BufRead;
    use tempfile::TempDir;

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

    #[test]
    fn teardown_kills_background_orphan_descendants() {
        assert_orphan_killed("sleep 60 & echo CHILD_PID=$!; wait");
    }

    #[test]
    fn teardown_kills_sighup_immune_orphan_descendants() {
        assert_orphan_killed("(trap '' HUP; exec sleep 60) & echo CHILD_PID=$!; wait");
    }
}
