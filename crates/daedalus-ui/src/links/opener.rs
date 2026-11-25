use super::{DetectedLink, is_safe_http_url};
use std::path::Path;
use std::process::{Command, Stdio};

/// Splits an editor command template into argv tokens respecting single/double quotes and
/// (on non-Windows targets) backslash escapes without invoking a shell.
pub(super) fn split_command_tokens_for_os(input: &str, is_windows: bool) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_token = false;
    let mut quote: Option<char> = None;
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        match quote {
            Some('\'') => {
                if ch == '\'' {
                    quote = None;
                } else {
                    current.push(ch);
                }
            }
            Some('"') => {
                if ch == '"' {
                    quote = None;
                } else if ch == '\\' {
                    if let Some(&next) = chars.peek()
                        && (next == '"' || (!is_windows && next == '\\'))
                    {
                        current.push(chars.next().unwrap());
                    } else {
                        current.push(ch);
                    }
                } else {
                    current.push(ch);
                }
            }
            None => match ch {
                '\'' | '"' => {
                    quote = Some(ch);
                    in_token = true;
                }
                '\\' if !is_windows => {
                    in_token = true;
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                }
                c if c.is_whitespace() => {
                    if in_token {
                        tokens.push(std::mem::take(&mut current));
                        in_token = false;
                    }
                }
                _ => {
                    current.push(ch);
                    in_token = true;
                }
            },
            Some(_) => unreachable!(),
        }
    }

    if in_token {
        tokens.push(current);
    }

    tokens
}

/// Returns the platform's default system opener binary
/// (`open` on macOS, `explorer.exe` on Windows, `xdg-open` on Linux).
pub fn default_system_opener_for_os(is_macos: bool, is_windows: bool) -> &'static str {
    if is_macos {
        "open"
    } else if is_windows {
        "explorer.exe"
    } else {
        "xdg-open"
    }
}

/// Returns the platform's default system opener binary (`open` on macOS, `xdg-open` on Linux).
pub fn default_system_opener(is_macos: bool) -> &'static str {
    default_system_opener_for_os(is_macos, false)
}

pub(super) fn build_open_command_with_env(
    link: &DetectedLink,
    editor: Option<&str>,
    visual_env: Option<&str>,
    editor_env: Option<&str>,
    is_macos: bool,
    is_windows: bool,
) -> Option<(String, Vec<String>)> {
    let system_opener = default_system_opener_for_os(is_macos, is_windows);
    match link {
        DetectedLink::Url(url) => {
            if !is_safe_http_url(url) {
                return None;
            }
            Some((system_opener.to_string(), vec![url.clone()]))
        }
        DetectedLink::File { path, line, col } => {
            let path_str = path.to_string_lossy().into_owned();
            let chosen_editor = [editor, visual_env, editor_env]
                .into_iter()
                .flatten()
                .map(str::trim)
                .find(|s| !s.is_empty());

            let Some(editor_cmd) = chosen_editor else {
                return Some((system_opener.to_string(), vec![path_str]));
            };

            let tokens = split_command_tokens_for_os(editor_cmd, is_windows);
            let Some((program, rest)) = tokens.split_first() else {
                return Some((system_opener.to_string(), vec![path_str]));
            };

            let has_placeholders = rest
                .iter()
                .any(|t| t.contains("{path}") || t.contains("{line}") || t.contains("{col}"));

            if has_placeholders {
                let line_str = line.unwrap_or(1).to_string();
                let col_str = col.unwrap_or(1).to_string();
                let mut had_path = false;
                let mut args = Vec::with_capacity(rest.len() + 1);
                for arg in rest {
                    if arg.contains("{path}") {
                        had_path = true;
                    }
                    let substituted = arg
                        .replace("{line}", &line_str)
                        .replace("{col}", &col_str)
                        .replace("{path}", &path_str);
                    args.push(substituted);
                }
                if !had_path {
                    args.push(path_str);
                }
                return Some((program.clone(), args));
            }

            let mut args = rest.to_vec();
            let raw_name = if is_windows {
                program.rsplit(['/', '\\']).next()
            } else {
                Path::new(program).file_name().and_then(|n| n.to_str())
            };
            let basename = raw_name.unwrap_or(program.as_str()).to_ascii_lowercase();
            let basename = basename
                .strip_suffix(".exe")
                .or_else(|| basename.strip_suffix(".cmd"))
                .or_else(|| basename.strip_suffix(".bat"))
                .unwrap_or(&basename);

            match basename {
                "code" | "code-insiders" | "codium" | "cursor" | "windsurf" => {
                    if let Some(l) = line {
                        args.push("--goto".to_string());
                        let mut target = format!("{path_str}:{l}");
                        if let Some(c) = col {
                            target.push_str(&format!(":{c}"));
                        }
                        args.push(target);
                    } else {
                        args.push(path_str);
                    }
                }
                "hx" | "helix" | "zed" | "subl" | "sublime_text" => {
                    if let Some(l) = line {
                        let mut target = format!("{path_str}:{l}");
                        if let Some(c) = col {
                            target.push_str(&format!(":{c}"));
                        }
                        args.push(target);
                    } else {
                        args.push(path_str);
                    }
                }
                "nvim" | "vim" | "vi" | "nano" | "emacs" | "emacsclient" | "kate" => {
                    if let Some(l) = line {
                        args.push(format!("+{l}"));
                    }
                    args.push(path_str);
                }
                _ => {
                    args.push(path_str);
                }
            }

            Some((program.clone(), args))
        }
    }
}

/// Builds the `(program, argv)` invocation for opening `link` without invoking a shell.
///
/// Uses `editor`, falling back to `$VISUAL`, `$EDITOR`, and finally the platform opener
/// (`open` on macOS, `explorer.exe` on Windows, `xdg-open` on Linux).
pub fn build_open_command(
    link: &DetectedLink,
    editor: Option<&str>,
) -> Option<(String, Vec<String>)> {
    let visual_env = std::env::var("VISUAL").ok();
    let editor_env = std::env::var("EDITOR").ok();
    build_open_command_with_env(
        link,
        editor,
        visual_env.as_deref(),
        editor_env.as_deref(),
        cfg!(target_os = "macos"),
        cfg!(target_os = "windows"),
    )
}

/// Spawns the external opener (`open` on macOS, `explorer.exe` on Windows, `xdg-open` on Linux,
/// or configured editor) for `link` directly via `Command::new(program).args(args)` without shell
/// interpolation.
pub fn open_link(link: &DetectedLink, editor: Option<&str>) -> std::io::Result<()> {
    let Some((program, args)) = build_open_command(link, editor) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "unsupported link target",
        ));
    };

    Command::new(&program)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    Ok(())
}
