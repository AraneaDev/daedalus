#[cfg(all(unix, any(test, not(any(target_os = "macos", target_os = "linux")))))]
use nix::{
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

#[cfg_attr(all(unix, not(target_os = "linux")), allow(dead_code))]
pub(crate) fn run_command_with_timeout(cmd: &mut Command, timeout: Duration) -> bool {
    run_command_until(cmd, Instant::now() + timeout).is_some_and(|status| status.success())
}

/// Wait for a quiet command within its caller's deadline and reap it on timeout.
pub(crate) fn run_command_until(cmd: &mut Command, deadline: Instant) -> Option<ExitStatus> {
    if Instant::now() >= deadline {
        return None;
    }
    let Ok(mut child) = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return None;
    };

    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                std::thread::sleep(remaining.min(Duration::from_millis(25)));
            }
            Err(_) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}

#[cfg(all(unix, any(test, not(any(target_os = "macos", target_os = "linux")))))]
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

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
}
