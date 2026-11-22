use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use daedalus_proto::GridRow;

/// Target detected under a clicked terminal cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetectedLink {
    Url(String),
    File {
        path: PathBuf,
        line: Option<u32>,
        col: Option<u32>,
    },
}

/// Detects an `http(s)://` URL or existing `<path>[:<line>[:<col>]]` at character column `col`
/// within `row_text`.
///
/// Relative file paths are resolved against `cwd` first, then `root_dir`, and must exist as
/// regular files on disk (`is_file()`).
pub fn detect_link_at(
    row_text: &str,
    col: usize,
    cwd: Option<&Path>,
    root_dir: Option<&Path>,
) -> Option<DetectedLink> {
    detect_at(row_text, col, Some((cwd, root_dir)))
}

fn detect_at(
    row_text: &str,
    col: usize,
    file_context: Option<(Option<&Path>, Option<&Path>)>,
) -> Option<DetectedLink> {
    let (click_byte, clicked_ch) = row_text.char_indices().nth(col)?;
    if clicked_ch.is_whitespace() {
        return None;
    }

    let tok_start = row_text[..click_byte]
        .char_indices()
        .rev()
        .find(|(_, ch)| ch.is_whitespace())
        .map(|(idx, ch)| idx + ch.len_utf8())
        .unwrap_or(0);

    let tok_end = row_text[click_byte..]
        .char_indices()
        .find(|(_, ch)| ch.is_whitespace())
        .map(|(offset, _)| click_byte + offset)
        .unwrap_or(row_text.len());

    let token = &row_text[tok_start..tok_end];
    let rel_click = click_byte - tok_start;

    if let Some(url) = extract_url_at(token, rel_click) {
        return Some(DetectedLink::Url(url));
    }

    let (cwd, root_dir) = file_context?;
    extract_file_at(token, rel_click, cwd, root_dir)
}

/// Detects a link at cell column `col` in a protocol [`GridRow`].
pub fn detect_link_in_row(
    row: &GridRow,
    col: usize,
    cwd: Option<&Path>,
    root_dir: Option<&Path>,
) -> Option<DetectedLink> {
    row_link(row, col, Some((cwd, root_dir)))
}

/// Detects a web link in a remote pane without resolving paths on the client.
pub fn detect_url_in_row(row: &GridRow, col: usize) -> Option<DetectedLink> {
    row_link(row, col, None)
}

fn row_link(
    row: &GridRow,
    col: usize,
    file_context: Option<(Option<&Path>, Option<&Path>)>,
) -> Option<DetectedLink> {
    if col >= row.cells.len() {
        return None;
    }

    let mut target_idx = col;
    if row.cells[target_idx].width == 0 && target_idx > 0 {
        target_idx -= 1;
    }

    let mut row_text = String::with_capacity(row.cells.len());
    let mut target_char_col = None;
    let mut char_count = 0usize;

    for (idx, cell) in row.cells.iter().enumerate() {
        if cell.width == 0 {
            continue;
        }
        if idx == target_idx {
            target_char_col = Some(char_count);
        }
        let text = if cell.text.is_empty() {
            " "
        } else {
            cell.text.as_str()
        };
        row_text.push_str(text);
        char_count += text.chars().count();
    }

    let char_col = target_char_col?;
    detect_at(&row_text, char_col, file_context)
}

fn is_url_stopper(ch: char) -> bool {
    matches!(ch, '<' | '>' | '"' | '\'' | '`')
}

fn trim_url_trailing_punctuation(candidate: &str) -> &str {
    let mut end = candidate.len();
    while end > 0 {
        let slice = &candidate[..end];
        let Some(last) = slice.chars().next_back() else {
            break;
        };
        let strip = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' | '>' | '`' => true,
            ')' => slice.matches(')').count() > slice.matches('(').count(),
            ']' => slice.matches(']').count() > slice.matches('[').count(),
            '}' => slice.matches('}').count() > slice.matches('{').count(),
            _ => false,
        };
        if strip {
            end -= last.len_utf8();
        } else {
            break;
        }
    }
    &candidate[..end]
}

fn extract_url_at(token: &str, rel_click: usize) -> Option<String> {
    let mut search_from = 0usize;
    while search_from < token.len() {
        let rest = &token[search_from..];
        let next_https = rest.find("https://");
        let next_http = rest.find("http://");
        let rel_scheme = match (next_https, next_http) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => break,
        };
        let scheme_start = search_from + rel_scheme;
        if scheme_start > rel_click {
            break;
        }

        let scheme_len = if token[scheme_start..].starts_with("https://") {
            "https://".len()
        } else {
            "http://".len()
        };

        let raw_end = token[scheme_start..]
            .char_indices()
            .find(|(_, ch)| is_url_stopper(*ch))
            .map(|(offset, _)| scheme_start + offset)
            .unwrap_or(token.len());

        let trimmed = trim_url_trailing_punctuation(&token[scheme_start..raw_end]);
        let trimmed_end = scheme_start + trimmed.len();

        if rel_click >= scheme_start
            && rel_click < trimmed_end
            && trimmed.len() > scheme_len
            && has_valid_url_host(&trimmed[scheme_len..])
        {
            return Some(trimmed.to_string());
        }

        search_from = raw_end.max(scheme_start + scheme_len);
    }

    None
}

fn has_valid_url_host(after_scheme: &str) -> bool {
    let host_part = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    !host_part.is_empty() && !host_part.starts_with('.')
}

fn is_leading_wrapper(ch: char) -> bool {
    matches!(ch, '(' | '[' | '{' | '<' | '\'' | '"' | '`')
}

fn is_trailing_wrapper_or_punct(ch: char) -> bool {
    matches!(
        ch,
        ')' | ']' | '}' | '>' | '\'' | '"' | '`' | ',' | ';' | ':'
    )
}

fn extract_file_at(
    token: &str,
    rel_click: usize,
    cwd: Option<&Path>,
    root_dir: Option<&Path>,
) -> Option<DetectedLink> {
    let mut start = 0usize;
    for (idx, ch) in token.char_indices() {
        if is_leading_wrapper(ch) {
            start = idx + ch.len_utf8();
        } else {
            break;
        }
    }

    let mut end = token.len();
    while end > start {
        let Some(last) = token[start..end].chars().next_back() else {
            break;
        };
        if is_trailing_wrapper_or_punct(last) {
            end -= last.len_utf8();
        } else {
            break;
        }
    }

    if start >= end || rel_click < start || rel_click >= end {
        return None;
    }

    let candidate = &token[start..end];
    parse_file_candidate(candidate, cwd, root_dir).or_else(|| {
        candidate
            .strip_suffix('.')
            .and_then(|stripped| parse_file_candidate(stripped, cwd, root_dir))
    })
}

fn parse_positive_u32(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<u32>().ok().filter(|&n| n > 0)
}

fn parse_file_candidate(
    candidate: &str,
    cwd: Option<&Path>,
    root_dir: Option<&Path>,
) -> Option<DetectedLink> {
    // 1. `<path>:<line>:<col>`
    if let Some((prefix, col_str)) = candidate.rsplit_once(':')
        && let Some(col_num) = parse_positive_u32(col_str)
        && let Some((path_str, line_str)) = prefix.rsplit_once(':')
        && let Some(line_num) = parse_positive_u32(line_str)
        && let Some(path) = resolve_existing_file(path_str, cwd, root_dir)
    {
        return Some(DetectedLink::File {
            path,
            line: Some(line_num),
            col: Some(col_num),
        });
    }

    // 2. `<path>:<line>`
    if let Some((path_str, line_str)) = candidate.rsplit_once(':')
        && let Some(line_num) = parse_positive_u32(line_str)
        && let Some(path) = resolve_existing_file(path_str, cwd, root_dir)
    {
        return Some(DetectedLink::File {
            path,
            line: Some(line_num),
            col: None,
        });
    }

    // 3. `<path>:<line>[:<col>]:<inline_text>` (e.g. ripgrep/grep output without spaces)
    for (colon_idx, _) in candidate.match_indices(':') {
        let path_str = &candidate[..colon_idx];
        let rest = &candidate[colon_idx + 1..];
        let mut parts = rest.split(':');
        let Some(line_str) = parts.next() else {
            continue;
        };
        let Some(line_num) = parse_positive_u32(line_str) else {
            continue;
        };
        let col_num = parts.next().and_then(parse_positive_u32);
        if let Some(path) = resolve_existing_file(path_str, cwd, root_dir) {
            return Some(DetectedLink::File {
                path,
                line: Some(line_num),
                col: col_num,
            });
        }
    }

    // 4. Plain `<path>`
    resolve_existing_file(candidate, cwd, root_dir).map(|path| DetectedLink::File {
        path,
        line: None,
        col: None,
    })
}

fn canonicalize_existing_file(path: &Path) -> PathBuf {
    daedalus_proto::strip_windows_verbatim_prefix(
        std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
    )
}

fn resolve_existing_file(
    raw_path: &str,
    cwd: Option<&Path>,
    root_dir: Option<&Path>,
) -> Option<PathBuf> {
    if raw_path.is_empty() {
        return None;
    }

    let path = Path::new(raw_path);
    if path.is_absolute() {
        return path.is_file().then(|| canonicalize_existing_file(path));
    }

    if let Some(cwd) = cwd {
        let joined = cwd.join(path);
        if joined.is_file() {
            return Some(canonicalize_existing_file(&joined));
        }
    }

    if let Some(root) = root_dir
        && cwd != Some(root)
    {
        let joined = root.join(path);
        if joined.is_file() {
            return Some(canonicalize_existing_file(&joined));
        }
    }

    None
}

fn is_safe_http_url(url: &str) -> bool {
    let valid_scheme = url
        .get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"))
        || url
            .get(..7)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://"));

    valid_scheme && !url.chars().any(char::is_control) && !url.chars().any(char::is_whitespace)
}

/// Splits an editor command template into argv tokens respecting single/double quotes and
/// (on non-Windows targets) backslash escapes without invoking a shell.
fn split_command_tokens_for_os(input: &str, is_windows: bool) -> Vec<String> {
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

fn build_open_command_with_env(
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

#[cfg(test)]
mod tests {
    use super::*;
    use daedalus_proto::{Cell, CellFlags, Color};
    use tempfile::tempdir;

    #[test]
    fn detects_urls_and_strips_trailing_punctuation_with_balanced_parens() {
        let row = "See (https://en.wikipedia.org/wiki/Rust_(programming_language)). and https://example.com/a,";
        let wiki_col = row.find("https://en").unwrap() + 10;
        assert_eq!(
            detect_link_at(row, wiki_col, None, None),
            Some(DetectedLink::Url(
                "https://en.wikipedia.org/wiki/Rust_(programming_language)".to_string()
            ))
        );

        let ex_col = row.find("https://example").unwrap() + 5;
        assert_eq!(
            detect_link_at(row, ex_col, None, None),
            Some(DetectedLink::Url("https://example.com/a".to_string()))
        );

        // Clicking on whitespace or leading prose returns None
        assert_eq!(detect_link_at(row, 0, None, None), None);
        assert_eq!(detect_link_at(row, 3, None, None), None);
    }

    #[test]
    fn detects_markdown_and_quoted_urls() {
        let row =
            r#"Check [docs](https://example.com/docs?x=1&y=2) or "http://localhost:8080/health";"#;
        let docs_col = row.find("https://").unwrap() + 8;
        assert_eq!(
            detect_link_at(row, docs_col, None, None),
            Some(DetectedLink::Url(
                "https://example.com/docs?x=1&y=2".to_string()
            ))
        );

        let local_col = row.find("http://").unwrap() + 7;
        assert_eq!(
            detect_link_at(row, local_col, None, None),
            Some(DetectedLink::Url(
                "http://localhost:8080/health".to_string()
            ))
        );
    }

    #[test]
    fn detects_file_line_col_resolving_cwd_then_root_and_verifying_existence() {
        let root = tempdir().unwrap();
        let sub = root.path().join("crates/ui");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(root.path().join("docs")).unwrap();

        let cwd_file = sub.join("main.rs");
        let root_file = root.path().join("docs/guide.md");
        std::fs::write(&cwd_file, "fn main() {}\n").unwrap();
        std::fs::write(&root_file, "# Guide\n").unwrap();

        // Relative to cwd with line + col
        let row1 = "   --> main.rs:42:13";
        let col1 = row1.find("main.rs").unwrap() + 2;
        assert_eq!(
            detect_link_at(row1, col1, Some(&sub), Some(root.path())),
            Some(DetectedLink::File {
                path: canonicalize_existing_file(&cwd_file),
                line: Some(42),
                col: Some(13),
            })
        );

        // Fallback to root_dir when not found in cwd
        let row2 = "See docs/guide.md:10, for details";
        let col2 = row2.find("docs/guide.md").unwrap() + 5;
        assert_eq!(
            detect_link_at(row2, col2, Some(&sub), Some(root.path())),
            Some(DetectedLink::File {
                path: canonicalize_existing_file(&root_file),
                line: Some(10),
                col: None,
            })
        );

        // Ripgrep-style inline snippet `path:line:code`
        let row3 = "main.rs:7:fn main() {}";
        assert_eq!(
            detect_link_at(row3, 3, Some(&sub), Some(root.path())),
            Some(DetectedLink::File {
                path: canonicalize_existing_file(&cwd_file),
                line: Some(7),
                col: None,
            })
        );

        // Non-existent file returns None
        let missing = "   --> nonexistent.rs:12:1";
        assert_eq!(
            detect_link_at(missing, 10, Some(&sub), Some(root.path())),
            None
        );
    }

    #[test]
    fn remote_link_detection_ignores_existing_absolute_paths() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let path = file.path().to_str().unwrap();
        assert!(matches!(
            detect_link_at(path, 0, None, None),
            Some(DetectedLink::File { .. })
        ));
        assert_eq!(detect_at(path, 0, None), None);
    }

    #[test]
    fn detects_link_in_grid_row_with_wide_chars() {
        let cells = vec![
            Cell {
                text: "界".to_string(),
                fg: Color::Default,
                bg: Color::Default,
                flags: CellFlags::empty(),
                width: 2,
            },
            Cell {
                text: String::new(),
                fg: Color::Default,
                bg: Color::Default,
                flags: CellFlags::empty(),
                width: 0,
            },
            Cell::default(),
            Cell {
                text: "h".to_string(),
                ..Cell::default()
            },
            Cell {
                text: "t".to_string(),
                ..Cell::default()
            },
            Cell {
                text: "t".to_string(),
                ..Cell::default()
            },
            Cell {
                text: "p".to_string(),
                ..Cell::default()
            },
            Cell {
                text: "s".to_string(),
                ..Cell::default()
            },
            Cell {
                text: ":".to_string(),
                ..Cell::default()
            },
            Cell {
                text: "/".to_string(),
                ..Cell::default()
            },
            Cell {
                text: "/".to_string(),
                ..Cell::default()
            },
            Cell {
                text: "a".to_string(),
                ..Cell::default()
            },
            Cell {
                text: ".".to_string(),
                ..Cell::default()
            },
            Cell {
                text: "c".to_string(),
                ..Cell::default()
            },
            Cell {
                text: "o".to_string(),
                ..Cell::default()
            },
            Cell {
                text: "m".to_string(),
                ..Cell::default()
            },
        ];
        let row = GridRow {
            row_index: 0,
            abs_row: 0,
            cells,
        };
        assert_eq!(
            detect_url_in_row(&row, 5),
            detect_link_in_row(&row, 5, None, None),
        );
        assert_eq!(
            detect_link_in_row(&row, 5, None, None),
            Some(DetectedLink::Url("https://a.com".to_string()))
        );
    }

    #[test]
    fn build_open_command_handles_urls_and_rejects_unsafe_schemes() {
        let ok = DetectedLink::Url("https://example.com/a?x=1&y=$(whoami);id".to_string());
        assert_eq!(
            build_open_command_with_env(&ok, None, None, None, false, false),
            Some((
                "xdg-open".to_string(),
                vec!["https://example.com/a?x=1&y=$(whoami);id".to_string()]
            ))
        );
        assert_eq!(
            build_open_command_with_env(&ok, None, None, None, true, false),
            Some((
                "open".to_string(),
                vec!["https://example.com/a?x=1&y=$(whoami);id".to_string()]
            ))
        );
        assert_eq!(
            build_open_command_with_env(&ok, None, None, None, false, true),
            Some((
                "explorer.exe".to_string(),
                vec!["https://example.com/a?x=1&y=$(whoami);id".to_string()]
            ))
        );
        assert_eq!(
            build_open_command(&ok, None),
            Some((
                default_system_opener_for_os(
                    cfg!(target_os = "macos"),
                    cfg!(target_os = "windows")
                )
                .to_string(),
                vec!["https://example.com/a?x=1&y=$(whoami);id".to_string()]
            ))
        );

        let bad = DetectedLink::Url("javascript:alert(1)".to_string());
        assert_eq!(
            build_open_command_with_env(&bad, None, None, None, false, false),
            None
        );
        assert_eq!(
            build_open_command_with_env(&bad, None, None, None, true, false),
            None
        );
        assert_eq!(
            build_open_command_with_env(&bad, None, None, None, false, true),
            None
        );
    }

    #[test]
    fn build_open_command_substitutes_placeholders_and_falls_back_to_env_or_system_opener() {
        assert_eq!(default_system_opener(false), "xdg-open");
        assert_eq!(default_system_opener(true), "open");
        assert_eq!(default_system_opener_for_os(false, false), "xdg-open");
        assert_eq!(default_system_opener_for_os(true, false), "open");
        assert_eq!(default_system_opener_for_os(false, true), "explorer.exe");

        let link = DetectedLink::File {
            path: PathBuf::from("/tmp/my project/main.rs"),
            line: Some(42),
            col: Some(7),
        };

        // Explicit template with placeholders
        assert_eq!(
            build_open_command_with_env(
                &link,
                Some("code --goto {path}:{line}:{col}"),
                None,
                None,
                false,
                false
            ),
            Some((
                "code".to_string(),
                vec![
                    "--goto".to_string(),
                    "/tmp/my project/main.rs:42:7".to_string()
                ]
            ))
        );

        // Quoted binary path + placeholders without {col}
        assert_eq!(
            build_open_command_with_env(
                &link,
                Some("\"/opt/my editor/nvim\" +{line} {path}"),
                None,
                None,
                true,
                false
            ),
            Some((
                "/opt/my editor/nvim".to_string(),
                vec!["+42".to_string(), "/tmp/my project/main.rs".to_string()]
            ))
        );

        // $VISUAL fallback without placeholders (nvim -> +line path)
        assert_eq!(
            build_open_command_with_env(&link, None, Some("nvim"), Some("nano"), false, false),
            Some((
                "nvim".to_string(),
                vec!["+42".to_string(), "/tmp/my project/main.rs".to_string()]
            ))
        );

        // $EDITOR fallback without placeholders (code -> --goto path:line:col)
        assert_eq!(
            build_open_command_with_env(&link, None, None, Some("code"), true, false),
            Some((
                "code".to_string(),
                vec![
                    "--goto".to_string(),
                    "/tmp/my project/main.rs:42:7".to_string()
                ]
            ))
        );

        // Windows `.cmd` / `.exe` editor wrapper suffix still matches editor presets
        assert_eq!(
            build_open_command_with_env(&link, None, None, Some("code.cmd"), false, true),
            Some((
                "code.cmd".to_string(),
                vec![
                    "--goto".to_string(),
                    "/tmp/my project/main.rs:42:7".to_string()
                ]
            ))
        );

        // No editor configured -> platform opener fallback
        assert_eq!(
            build_open_command_with_env(&link, None, None, None, false, false),
            Some((
                "xdg-open".to_string(),
                vec!["/tmp/my project/main.rs".to_string()]
            ))
        );
        assert_eq!(
            build_open_command_with_env(&link, None, None, None, true, false),
            Some((
                "open".to_string(),
                vec!["/tmp/my project/main.rs".to_string()]
            ))
        );
        assert_eq!(
            build_open_command_with_env(&link, None, None, None, false, true),
            Some((
                "explorer.exe".to_string(),
                vec!["/tmp/my project/main.rs".to_string()]
            ))
        );
    }

    #[test]
    fn windows_editor_command_preserves_backslashes() {
        let link = DetectedLink::File {
            path: PathBuf::from(r"C:\Users\tim\repo\main.rs"),
            line: Some(18),
            col: Some(3),
        };

        assert_eq!(
            build_open_command_with_env(&link, Some(r"C:\tools\nvim.exe"), None, None, false, true,),
            Some((
                r"C:\tools\nvim.exe".to_string(),
                vec!["+18".to_string(), r"C:\Users\tim\repo\main.rs".to_string()]
            ))
        );

        assert_eq!(
            split_command_tokens_for_os(
                r#"C:\tools\nvim.exe --server \\.\pipe\nvim +{line} {path}"#,
                true,
            ),
            vec![
                r"C:\tools\nvim.exe".to_string(),
                "--server".to_string(),
                r"\\.\pipe\nvim".to_string(),
                "+{line}".to_string(),
                "{path}".to_string(),
            ]
        );

        assert_eq!(
            split_command_tokens_for_os(
                r#""C:\Program Files\Neovim\bin\nvim.exe" "\\server\share\init.lua""#,
                true,
            ),
            vec![
                r"C:\Program Files\Neovim\bin\nvim.exe".to_string(),
                r"\\server\share\init.lua".to_string(),
            ]
        );
    }
}
