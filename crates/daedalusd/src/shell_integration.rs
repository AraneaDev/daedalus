mod launch;

use std::collections::HashMap;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const BASH_INIT_SCRIPT: &str = include_str!("shell_integration/bash-init.bash");

const ZSH_INIT_SCRIPT: &str = include_str!("shell_integration/zshrc");

const FISH_INIT_SCRIPT: &str = include_str!("shell_integration/daedalus.fish");

const POWERSHELL_INIT_SCRIPT: &str = include_str!("shell_integration/daedalus.ps1");

#[derive(Debug, Clone)]
pub struct ShellIntegration {
    shell_dir: PathBuf,
    bash_rc: PathBuf,
    zsh_dir: PathBuf,
    powershell_script: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellLaunchSpec {
    pub direct: bool,
    pub program: String,
    pub args: Vec<String>,
    pub extra_env: HashMap<String, String>,
}

impl ShellIntegration {
    pub fn prepare(runtime_dir: &Path) -> std::io::Result<Self> {
        let shell_dir = runtime_dir.join("shell");
        let zsh_dir = shell_dir.join("zsh");
        let fish_dir = shell_dir.join("fish");
        let fish_vendor_dir = fish_dir.join("vendor_conf.d");
        let powershell_dir = shell_dir.join("powershell");
        fs::create_dir_all(&zsh_dir)?;
        fs::create_dir_all(&fish_vendor_dir)?;
        fs::create_dir_all(&powershell_dir)?;
        #[cfg(unix)]
        {
            fs::set_permissions(&shell_dir, fs::Permissions::from_mode(0o700))?;
            fs::set_permissions(&zsh_dir, fs::Permissions::from_mode(0o700))?;
            fs::set_permissions(&fish_dir, fs::Permissions::from_mode(0o700))?;
            fs::set_permissions(&fish_vendor_dir, fs::Permissions::from_mode(0o700))?;
            fs::set_permissions(&powershell_dir, fs::Permissions::from_mode(0o700))?;
        }

        let bash_rc = shell_dir.join("bash-init.sh");
        let powershell_script = powershell_dir.join("daedalus.ps1");
        fs::write(&bash_rc, BASH_INIT_SCRIPT)?;
        fs::write(zsh_dir.join(".zshrc"), ZSH_INIT_SCRIPT)?;
        fs::write(fish_vendor_dir.join("daedalus.fish"), FISH_INIT_SCRIPT)?;
        fs::write(&powershell_script, POWERSHELL_INIT_SCRIPT)?;

        Ok(Self {
            shell_dir,
            bash_rc,
            zsh_dir,
            powershell_script,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use launch::{build_fish_xdg_data_dirs, resolve_windows_default_shell};
    use tempfile::TempDir;

    #[test]
    fn prepares_scripts_and_builds_specs_for_bash_zsh_and_direct_agents() {
        let dir = TempDir::new().unwrap();
        let integration = ShellIntegration::prepare(dir.path()).unwrap();

        let bash = integration.build_launch_spec(None, Some("/bin/bash"));
        assert_eq!(bash.program, "/bin/bash");
        assert_eq!(bash.args[0], "--rcfile");
        assert!(Path::new(&bash.args[1]).exists());

        let zsh = integration.build_launch_spec(None, Some("/usr/bin/zsh"));
        assert_eq!(zsh.program, "/usr/bin/zsh");
        assert_eq!(zsh.args, vec!["-i"]);
        assert!(
            Path::new(zsh.extra_env.get("ZDOTDIR").unwrap())
                .join(".zshrc")
                .exists()
        );

        let fish = integration.build_launch_spec(None, Some("/usr/bin/fish"));
        assert_eq!(fish.program, "/usr/bin/fish");
        assert_eq!(fish.args, vec!["-i"]);
        let xdg_data_dirs = fish.extra_env.get("XDG_DATA_DIRS").unwrap();
        let expected_shell_dir = dir.path().join("shell");
        assert!(
            xdg_data_dirs.starts_with(&format!("{}:", expected_shell_dir.display())),
            "unexpected XDG_DATA_DIRS: {xdg_data_dirs}"
        );
        let fish_script_path = expected_shell_dir
            .join("fish")
            .join("vendor_conf.d")
            .join("daedalus.fish");
        assert!(fish_script_path.is_file());
        let fish_script = fs::read_to_string(&fish_script_path).unwrap();
        assert!(fish_script.contains("133;A"));
        assert!(fish_script.contains("133;B"));
        assert!(fish_script.contains("133;C"));
        assert!(fish_script.contains("133;D;%d"));
        assert!(fish_script.contains("7;file://"));
        assert!(fish_script.contains("fish_preexec"));
        assert!(fish_script.contains("fish_postexec"));
        assert!(fish_script.contains("fish_prompt"));

        let agy = integration
            .build_launch_spec(Some(&["agy".to_string(), "--version".to_string()]), None);
        assert_eq!(agy.program, "agy");
        assert_eq!(agy.args, vec!["--version"]);
        assert!(agy.extra_env.is_empty());
    }

    #[test]
    fn fish_xdg_data_dirs_prepends_or_uses_standard_fallback() {
        let shell_dir = Path::new("/run/user/1000/daedalus/shell");
        assert_eq!(
            build_fish_xdg_data_dirs(shell_dir, Some("/custom/share:/usr/share")),
            "/run/user/1000/daedalus/shell:/custom/share:/usr/share"
        );
        assert_eq!(
            build_fish_xdg_data_dirs(shell_dir, Some("")),
            "/run/user/1000/daedalus/shell:/usr/local/share:/usr/share"
        );
        assert_eq!(
            build_fish_xdg_data_dirs(shell_dir, None),
            "/run/user/1000/daedalus/shell:/usr/local/share:/usr/share"
        );
    }

    #[test]
    fn prepares_powershell_script_and_builds_launch_specs_for_pwsh_and_powershell() {
        let dir = TempDir::new().unwrap();
        let integration = ShellIntegration::prepare(dir.path()).unwrap();

        let expected_ps1 = dir
            .path()
            .join("shell")
            .join("powershell")
            .join("daedalus.ps1");
        assert!(expected_ps1.is_file());
        let ps1_contents = fs::read_to_string(&expected_ps1).unwrap();
        assert!(ps1_contents.contains("133;A"));
        assert!(ps1_contents.contains("133;B"));
        assert!(ps1_contents.contains("133;C"));
        assert!(ps1_contents.contains("133;D;"));
        assert!(ps1_contents.contains("7;file://"));
        assert!(ps1_contents.contains("PSReadLine"));

        let pwsh_win =
            integration.build_launch_spec(None, Some(r"C:\Program Files\PowerShell\7\pwsh.exe"));
        assert_eq!(pwsh_win.program, r"C:\Program Files\PowerShell\7\pwsh.exe");
        assert_eq!(
            pwsh_win.args,
            vec![
                "-ExecutionPolicy".to_string(),
                "Bypass".to_string(),
                "-NoLogo".to_string(),
                "-NoExit".to_string(),
                "-File".to_string(),
                expected_ps1.to_string_lossy().into_owned(),
            ]
        );
        assert!(pwsh_win.extra_env.is_empty());

        let powershell_exe = integration.build_launch_spec(None, Some("PowerShell.EXE"));
        assert_eq!(powershell_exe.program, "PowerShell.EXE");
        assert_eq!(
            powershell_exe.args,
            vec![
                "-ExecutionPolicy".to_string(),
                "Bypass".to_string(),
                "-NoLogo".to_string(),
                "-NoExit".to_string(),
                "-File".to_string(),
                expected_ps1.to_string_lossy().into_owned(),
            ]
        );

        let pwsh_unix = integration.build_launch_spec(None, Some("/usr/bin/pwsh"));
        assert_eq!(pwsh_unix.program, "/usr/bin/pwsh");
        assert!(pwsh_unix.args.ends_with(&[
            "-NoLogo".to_string(),
            "-NoExit".to_string(),
            "-File".to_string(),
            expected_ps1.to_string_lossy().into_owned(),
        ]));
    }

    #[test]
    fn windows_default_shell_prefers_pwsh_then_powershell_then_comspec() {
        let dir = TempDir::new().unwrap();
        let empty_dir = dir.path().join("empty");
        fs::create_dir_all(&empty_dir).unwrap();

        assert_eq!(
            resolve_windows_default_shell(
                Some(r"C:\Program Files\Git\bin\bash.exe"),
                Some(r"C:\Windows\System32\cmd.exe"),
                Some(empty_dir.as_os_str()),
            ),
            r"C:\Program Files\Git\bin\bash.exe"
        );
        assert_eq!(
            resolve_windows_default_shell(
                None,
                Some(r"C:\Windows\System32\cmd.exe"),
                Some(empty_dir.as_os_str()),
            ),
            r"C:\Windows\System32\cmd.exe"
        );
        assert_eq!(
            resolve_windows_default_shell(None, Some(""), Some(empty_dir.as_os_str())),
            "powershell.exe"
        );

        let ps_dir = dir.path().join("ps");
        fs::create_dir_all(&ps_dir).unwrap();
        fs::write(ps_dir.join("powershell.exe"), b"").unwrap();
        assert_eq!(
            resolve_windows_default_shell(
                None,
                Some(r"C:\Windows\System32\cmd.exe"),
                Some(ps_dir.as_os_str()),
            ),
            "powershell.exe"
        );

        fs::write(ps_dir.join("pwsh.exe"), b"").unwrap();
        assert_eq!(
            resolve_windows_default_shell(
                None,
                Some(r"C:\Windows\System32\cmd.exe"),
                Some(ps_dir.as_os_str()),
            ),
            "pwsh.exe"
        );
    }

    #[cfg(unix)]
    fn run_bash_with_bashrc(bashrc_contents: &str) -> Vec<daedalus_vt::SemanticEvent> {
        use crate::supervisor::{IsolationMode, Supervisor};
        use daedalus_proto::{PaneId, PaneRole, WorkspaceId};
        use daedalus_vt::{VtConfig, create_vt_engine};
        use std::io::{Read, Write};
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        let dir = TempDir::new().unwrap();
        let fake_home = dir.path().join("home");
        fs::create_dir_all(&fake_home).unwrap();
        fs::write(fake_home.join(".bashrc"), bashrc_contents).unwrap();

        let integration = ShellIntegration::prepare(dir.path()).unwrap();
        let launch = integration.build_launch_spec(None, Some("/bin/bash"));
        let supervisor = Supervisor::with_mode(IsolationMode::PgidFallback);

        let mut env = HashMap::new();
        env.insert("HOME".to_string(), fake_home.to_string_lossy().into_owned());
        env.insert("PS1".to_string(), "$ ".to_string());

        let mut spawned = supervisor
            .spawn_pty(
                WorkspaceId::new(),
                PaneId::new(),
                PaneRole::Runner,
                dir.path(),
                80,
                24,
                &launch,
                &env,
            )
            .unwrap();

        let mut reader = spawned.reader;
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                if tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });

        let mut vt = create_vt_engine(VtConfig::default());
        let mut all_events = Vec::new();
        let mut saw_first_prompt = false;
        let mut sent_cmd = false;

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(50)) {
                let events = vt.feed(&chunk);
                for ev in events {
                    if matches!(ev, daedalus_vt::SemanticEvent::PromptStart) {
                        saw_first_prompt = true;
                    }
                    all_events.push(ev);
                }
                if saw_first_prompt && !sent_cmd {
                    sent_cmd = true;
                    spawned
                        .writer
                        .write_all(b"echo hello_prompt_cmd\n")
                        .unwrap();
                    spawned.writer.flush().unwrap();
                }
                if all_events
                    .iter()
                    .any(|e| matches!(e, daedalus_vt::SemanticEvent::CommandFinished { .. }))
                {
                    break;
                }
            }
        }

        drop(spawned.writer);
        drop(spawned.master);
        spawned.handle.teardown();
        all_events
    }

    #[cfg(unix)]
    #[test]
    fn bash_init_preserves_user_prompt_command_without_spurious_preexec() {
        use daedalus_vt::SemanticEvent;

        for bashrc in [
            "__user_prompt_hook() { printf ''; }\nPROMPT_COMMAND='__user_prompt_hook'\n",
            "__user_prompt_hook() { printf ''; }\nPROMPT_COMMAND=('__user_prompt_hook')\n",
        ] {
            let events = run_bash_with_bashrc(bashrc);
            let first_prompt_idx = events
                .iter()
                .position(|e| matches!(e, SemanticEvent::PromptStart))
                .expect("expected initial PromptStart");
            // No CommandExecuted (133;C) may be emitted during initial PROMPT_COMMAND execution
            assert!(
                !events[..first_prompt_idx]
                    .iter()
                    .any(|e| matches!(e, SemanticEvent::CommandExecuted)),
                "spurious CommandExecuted before first prompt: {events:?}"
            );
            // The real command must emit CommandExecuted and CommandFinished { exit_code: 0 }
            let cmd_exec_idx = events
                .iter()
                .position(|e| matches!(e, SemanticEvent::CommandExecuted))
                .expect("expected CommandExecuted for real command");
            assert!(
                cmd_exec_idx > first_prompt_idx,
                "CommandExecuted should occur after initial prompt: {events:?}"
            );
            assert!(
                events[cmd_exec_idx..]
                    .iter()
                    .any(|e| matches!(e, SemanticEvent::CommandFinished { exit_code: 0 })),
                "expected CommandFinished(0) after CommandExecuted: {events:?}"
            );
        }
    }
}
