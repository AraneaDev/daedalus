use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const BASH_INIT_SCRIPT: &str = r#"# Daedalus non-destructive bash shell integration (OSC 7 + OSC 133)
if [ -r /etc/profile ]; then
    . /etc/profile
fi
if [ -r "$HOME/.bashrc" ]; then
    . "$HOME/.bashrc"
fi

__daedalus_urlencode() {
    local str="$1" i c hex
    local LC_ALL=C
    for (( i=0; i<${#str}; i++ )); do
        c="${str:i:1}"
        case "$c" in
            [a-zA-Z0-9.~_/-]) printf '%s' "$c" ;;
            *) printf -v hex '%%%02X' "'$c"; printf '%s' "$hex" ;;
        esac
    done
}

__daedalus_in_cmd=1
__daedalus_first_prompt=1

__daedalus_preexec() {
    case "$BASH_COMMAND" in
        __daedalus_precmd_start|__daedalus_precmd_end) return ;;
    esac
    if [ "$__daedalus_in_cmd" -eq 0 ]; then
        __daedalus_in_cmd=1
        printf '\033]133;C\007'
    fi
}

__daedalus_precmd_start() {
    local exit_code=$?
    __daedalus_in_cmd=1
    if [ "$__daedalus_first_prompt" -eq 0 ]; then
        printf '\033]133;D;%d\007' "$exit_code"
    fi
    __daedalus_first_prompt=0
    local encoded_pwd
    encoded_pwd="$(__daedalus_urlencode "$PWD")"
    printf '\033]7;file://%s%s\007' "${HOSTNAME:-localhost}" "$encoded_pwd"
    return "$exit_code"
}

__daedalus_precmd_end() {
    __daedalus_in_cmd=0
}

if [[ "$PS1" != *"133;A"* ]]; then
    PS1="\[\033]133;A\007\]${PS1}\[\033]133;B\007\]"
fi

if [[ "$(declare -p PROMPT_COMMAND 2>/dev/null)" == "declare -a"* ]]; then
    PROMPT_COMMAND=("__daedalus_precmd_start" "${PROMPT_COMMAND[@]}" "__daedalus_precmd_end")
elif [ -n "${PROMPT_COMMAND:-}" ]; then
    PROMPT_COMMAND=$'__daedalus_precmd_start\n'"${PROMPT_COMMAND}"$'\n__daedalus_precmd_end'
else
    PROMPT_COMMAND=$'__daedalus_precmd_start\n__daedalus_precmd_end'
fi
trap '__daedalus_preexec' DEBUG
"#;

const ZSH_INIT_SCRIPT: &str = r#"# Daedalus non-destructive zsh shell integration (OSC 7 + OSC 133)
export ZDOTDIR="${DAEDALUS_ORIG_ZDOTDIR:-$HOME}"
if [ -r "$ZDOTDIR/.zshrc" ]; then
    source "$ZDOTDIR/.zshrc"
fi

autoload -Uz add-zsh-hook

__daedalus_first_prompt=1

__daedalus_precmd() {
    local exit_code=$?
    if [[ "$__daedalus_first_prompt" -eq 0 ]]; then
        printf '\033]133;D;%d\007' "$exit_code"
    fi
    __daedalus_first_prompt=0
    local encoded_pwd="${PWD// /%20}"
    printf '\033]7;file://%s%s\007' "${HOST:-localhost}" "$encoded_pwd"
}

__daedalus_preexec() {
    printf '\033]133;C\007'
}

if [[ "$PROMPT" != *"133;A"* ]]; then
    PROMPT=$'%{\e]133;A\a%}'"${PROMPT}"$'%{\e]133;B\a%}'
fi

add-zsh-hook precmd __daedalus_precmd
add-zsh-hook preexec __daedalus_preexec
"#;

#[derive(Debug, Clone)]
pub struct ShellIntegration {
    bash_rc: PathBuf,
    zsh_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellLaunchSpec {
    pub program: String,
    pub args: Vec<String>,
    pub extra_env: HashMap<String, String>,
}

impl ShellIntegration {
    pub fn prepare(runtime_dir: &Path) -> std::io::Result<Self> {
        let shell_dir = runtime_dir.join("shell");
        let zsh_dir = shell_dir.join("zsh");
        fs::create_dir_all(&zsh_dir)?;
        fs::set_permissions(&shell_dir, fs::Permissions::from_mode(0o700))?;
        fs::set_permissions(&zsh_dir, fs::Permissions::from_mode(0o700))?;

        let bash_rc = shell_dir.join("bash-init.sh");
        fs::write(&bash_rc, BASH_INIT_SCRIPT)?;
        fs::write(zsh_dir.join(".zshrc"), ZSH_INIT_SCRIPT)?;

        Ok(Self { bash_rc, zsh_dir })
    }

    pub fn build_launch_spec(
        &self,
        command: Option<&[String]>,
        shell_override: Option<&str>,
    ) -> ShellLaunchSpec {
        if let Some(cmd) = command
            && let Some((program, args)) = cmd.split_first()
        {
            return ShellLaunchSpec {
                program: program.clone(),
                args: args.to_vec(),
                extra_env: HashMap::new(),
            };
        }

        let shell_path = shell_override
            .map(|s| s.to_string())
            .or_else(|| std::env::var("SHELL").ok().filter(|s| !s.is_empty()))
            .unwrap_or_else(|| "/bin/bash".to_string());

        let base_name = Path::new(&shell_path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("bash");

        match base_name {
            "bash" => ShellLaunchSpec {
                program: shell_path,
                args: vec![
                    "--rcfile".to_string(),
                    self.bash_rc.to_string_lossy().into_owned(),
                    "-i".to_string(),
                ],
                extra_env: HashMap::new(),
            },
            "zsh" => {
                let mut extra_env = HashMap::new();
                if let Ok(orig) = std::env::var("ZDOTDIR") {
                    extra_env.insert("DAEDALUS_ORIG_ZDOTDIR".to_string(), orig);
                }
                extra_env.insert(
                    "ZDOTDIR".to_string(),
                    self.zsh_dir.to_string_lossy().into_owned(),
                );
                ShellLaunchSpec {
                    program: shell_path,
                    args: vec!["-i".to_string()],
                    extra_env,
                }
            }
            _ => ShellLaunchSpec {
                program: shell_path,
                args: Vec::new(),
                extra_env: HashMap::new(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

        let agy = integration
            .build_launch_spec(Some(&["agy".to_string(), "--version".to_string()]), None);
        assert_eq!(agy.program, "agy");
        assert_eq!(agy.args, vec!["--version"]);
        assert!(agy.extra_env.is_empty());
    }

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
