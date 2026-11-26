use super::{ShellIntegration, ShellLaunchSpec};
use std::{collections::HashMap, path::Path};

const DEFAULT_XDG_DATA_DIRS: &str = "/usr/local/share:/usr/share";

pub(super) fn build_fish_xdg_data_dirs(shell_dir: &Path, existing: Option<&str>) -> String {
    let prefix = shell_dir.to_string_lossy();
    let tail = existing
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_XDG_DATA_DIRS);
    format!("{prefix}:{tail}")
}

#[cfg(any(windows, test))]
pub(super) fn resolve_windows_default_shell(
    shell_env: Option<&str>,
    comspec_env: Option<&str>,
    path_env: Option<&std::ffi::OsStr>,
) -> String {
    if let Some(shell) = shell_env.filter(|s| !s.is_empty()) {
        return shell.to_string();
    }
    if let Some(paths) = path_env {
        for exe in ["pwsh.exe", "powershell.exe"] {
            for dir in std::env::split_paths(paths) {
                if dir.join(exe).is_file() {
                    return exe.to_string();
                }
            }
        }
    }
    comspec_env
        .filter(|s| !s.is_empty())
        .unwrap_or("powershell.exe")
        .to_string()
}

fn default_shell_path() -> String {
    #[cfg(windows)]
    {
        resolve_windows_default_shell(
            std::env::var("SHELL").ok().as_deref(),
            std::env::var("COMSPEC").ok().as_deref(),
            std::env::var_os("PATH").as_deref(),
        )
    }
    #[cfg(not(windows))]
    {
        std::env::var("SHELL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "/bin/bash".to_string())
    }
}

impl ShellIntegration {
    pub fn build_launch_spec(
        &self,
        command: Option<&[String]>,
        shell_override: Option<&str>,
    ) -> ShellLaunchSpec {
        if let Some(cmd) = command
            && let Some((program, args)) = cmd.split_first()
        {
            return ShellLaunchSpec {
                direct: true,
                program: program.clone(),
                args: args.to_vec(),
                extra_env: HashMap::new(),
            };
        }

        let shell_path = shell_override
            .map(ToString::to_string)
            .unwrap_or_else(default_shell_path);

        let base_name = shell_path
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(shell_path.as_str())
            .to_ascii_lowercase();

        match base_name.as_str() {
            "bash" | "bash.exe" => ShellLaunchSpec {
                direct: false,
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
                    direct: false,
                    program: shell_path,
                    args: vec!["-i".to_string()],
                    extra_env,
                }
            }
            "fish" => {
                let mut extra_env = HashMap::new();
                extra_env.insert(
                    "XDG_DATA_DIRS".to_string(),
                    build_fish_xdg_data_dirs(
                        &self.shell_dir,
                        std::env::var("XDG_DATA_DIRS").ok().as_deref(),
                    ),
                );
                ShellLaunchSpec {
                    direct: false,
                    program: shell_path,
                    args: vec!["-i".to_string()],
                    extra_env,
                }
            }
            "pwsh" | "pwsh.exe" | "powershell" | "powershell.exe" => {
                let mut args = Vec::new();
                if cfg!(windows) || base_name.ends_with(".exe") {
                    args.push("-ExecutionPolicy".to_string());
                    args.push("Bypass".to_string());
                }
                args.extend([
                    "-NoLogo".to_string(),
                    "-NoExit".to_string(),
                    "-File".to_string(),
                    self.powershell_script.to_string_lossy().into_owned(),
                ]);
                ShellLaunchSpec {
                    direct: false,
                    program: shell_path,
                    args,
                    extra_env: HashMap::new(),
                }
            }
            _ => ShellLaunchSpec {
                direct: false,
                program: shell_path,
                args: Vec::new(),
                extra_env: HashMap::new(),
            },
        }
    }
}
