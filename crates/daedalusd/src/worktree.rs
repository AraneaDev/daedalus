use daedalus_proto::{GitWorkspaceStatus, GitWorktreeInfo};
use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};
use thiserror::Error;

const GIT_TIMEOUT: Duration = Duration::from_secs(5);
const GIT_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_FAILURE_CONTEXT_BYTES: usize = 4096;

#[derive(Debug, Error)]
pub enum WorktreeError {
    #[error("refusing to remove primary repository checkout: {0}")]
    PrimaryCheckout(PathBuf),
    #[error("path is not a linked git worktree: {0}")]
    NotAWorktree(PathBuf),
    #[error("worktree has uncommitted changes (use force to remove): {0}")]
    DirtyWorktree(PathBuf),
    #[error("git command failed: {0}")]
    GitFailed(String),
}

struct GitOutput {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

fn resolve_worktrees_dir(
    daedalus_dir: Option<&str>,
    xdg_data_home: Option<&str>,
    home: Option<&str>,
) -> PathBuf {
    if let Some(dir) = daedalus_dir
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    if let Some(xdg) = xdg_data_home
        && !xdg.is_empty()
    {
        return PathBuf::from(xdg).join("daedalus").join("worktrees");
    }
    if let Some(home_dir) = home
        && !home_dir.is_empty()
    {
        return PathBuf::from(home_dir)
            .join(".local")
            .join("share")
            .join("daedalus")
            .join("worktrees");
    }
    std::env::temp_dir().join("daedalus-worktrees")
}

/// Returns the default base directory where Daedalus creates managed Git worktrees.
pub fn default_worktrees_dir() -> PathBuf {
    resolve_worktrees_dir(
        std::env::var("DAEDALUS_WORKTREES_DIR").ok().as_deref(),
        std::env::var("XDG_DATA_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
}

/// Validates a branch name and converts it into a safe single-component directory slug.
pub fn sanitize_slug(branch: &str) -> Result<String, String> {
    if branch.is_empty() {
        return Err("branch name cannot be empty".to_string());
    }
    if branch.contains("..") {
        return Err("branch name cannot contain '..'".to_string());
    }
    if branch.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("branch name cannot contain control characters or whitespace".to_string());
    }

    let mapped: String = branch
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();

    let slug = mapped.trim_matches('-');
    if slug.is_empty() || slug == "." {
        return Err(format!(
            "branch name '{branch}' does not produce a valid directory slug"
        ));
    }

    Ok(slug.to_string())
}

fn strip_windows_verbatim_prefix(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(raw) = path.to_str() {
            if let Some(unc) = raw.strip_prefix(r"\\?\UNC\") {
                return PathBuf::from(format!(r"\\{unc}"));
            }
            if let Some(stripped) = raw.strip_prefix(r"\\?\") {
                return PathBuf::from(stripped);
            }
        }
    }
    path
}

fn canonicalize_path(path: &Path) -> PathBuf {
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    strip_windows_verbatim_prefix(resolved)
}

fn spawn_reader<R: Read + Send + 'static>(mut reader: R) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = reader.read_to_end(&mut buf);
        buf
    })
}

fn run_git_with_timeout(repo_dir: &Path, args: &[&OsStr]) -> Result<GitOutput, String> {
    run_git_with_timeout_for(repo_dir, args, GIT_TIMEOUT)
}

fn run_git_with_timeout_for(
    repo_dir: &Path,
    args: &[&OsStr],
    timeout: Duration,
) -> Result<GitOutput, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo_dir);
    for arg in args {
        cmd.arg(arg);
    }
    cmd.env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn git: {e}"))?;

    let stdout_handle = child.stdout.take().map(spawn_reader);
    let stderr_handle = child.stderr.take().map(spawn_reader);

    let deadline = Instant::now() + timeout;
    let mut poll_interval = Duration::from_millis(1);

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                let now = Instant::now();
                if now >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    if let Some(h) = stdout_handle {
                        let _ = h.join();
                    }
                    if let Some(h) = stderr_handle {
                        let _ = h.join();
                    }
                    return Err(format!(
                        "git command timed out after {}s",
                        timeout.as_secs()
                    ));
                }
                let remaining = deadline.saturating_duration_since(now);
                std::thread::sleep(poll_interval.min(remaining));
                poll_interval = (poll_interval * 2).min(Duration::from_millis(10));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("failed waiting for git: {e}"));
            }
        }
    };

    let stdout_bytes = stdout_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    let stderr_bytes = stderr_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();

    Ok(GitOutput {
        status,
        stdout: String::from_utf8_lossy(&stdout_bytes).into_owned(),
        stderr: String::from_utf8_lossy(&stderr_bytes).into_owned(),
    })
}

fn parse_worktree_porcelain(output: &str) -> Vec<GitWorktreeInfo> {
    #[derive(Default)]
    struct Stanza {
        path: Option<PathBuf>,
        head: String,
        branch: Option<String>,
        is_detached: bool,
    }

    let mut stanzas = Vec::new();
    let mut current = Stanza::default();

    let flush = |stanzas: &mut Vec<Stanza>, current: &mut Stanza| {
        if let Some(path) = current.path.take() {
            stanzas.push(Stanza {
                path: Some(path),
                head: std::mem::take(&mut current.head),
                branch: current.branch.take(),
                is_detached: std::mem::take(&mut current.is_detached),
            });
        } else {
            *current = Stanza::default();
        }
    };

    for raw_line in output.lines() {
        let line = raw_line.trim_end_matches('\r');
        if line.is_empty() {
            flush(&mut stanzas, &mut current);
            continue;
        }
        if let Some(path) = line.strip_prefix("worktree ") {
            if current.path.is_some() {
                flush(&mut stanzas, &mut current);
            }
            current.path = Some(PathBuf::from(path));
        } else if let Some(head) = line.strip_prefix("HEAD ") {
            current.head = head.to_string();
        } else if let Some(branch_ref) = line.strip_prefix("branch ") {
            let name = branch_ref.strip_prefix("refs/heads/").unwrap_or(branch_ref);
            current.branch = Some(name.to_string());
        } else if line == "detached" {
            current.is_detached = true;
        }
    }
    flush(&mut stanzas, &mut current);

    stanzas
        .into_iter()
        .enumerate()
        .filter_map(|(idx, s)| {
            Some(GitWorktreeInfo {
                path: s.path?,
                branch: s.branch,
                head: s.head,
                is_primary: idx == 0,
                is_detached: s.is_detached,
            })
        })
        .collect()
}

/// Discovers Git worktrees for the repository containing `repo_dir`.
///
/// Returns `Ok(Vec::new())` if `repo_dir` is not inside a Git repository.
pub fn discover_worktrees(repo_dir: &Path) -> Result<Vec<GitWorktreeInfo>, String> {
    if !repo_dir.is_dir() {
        return Err(format!(
            "repository path is not a directory: {}",
            repo_dir.display()
        ));
    }

    let args = [
        OsStr::new("worktree"),
        OsStr::new("list"),
        OsStr::new("--porcelain"),
    ];
    let out = run_git_with_timeout(repo_dir, &args)?;
    if !out.status.success() {
        let stderr_lower = out.stderr.to_ascii_lowercase();
        if stderr_lower.contains("not a git repository")
            || stderr_lower.contains("must be run in a work tree")
        {
            return Ok(Vec::new());
        }
        return Err(format!("git worktree list failed: {}", out.stderr.trim()));
    }

    Ok(parse_worktree_porcelain(&out.stdout))
}

/// Returns the currently checked-out branch name in `dir`, or `None` for detached HEAD or non-git directories.
pub fn current_branch(dir: &Path) -> Option<String> {
    if !dir.is_dir() {
        return None;
    }

    let args = [
        OsStr::new("symbolic-ref"),
        OsStr::new("--short"),
        OsStr::new("HEAD"),
    ];
    let out = run_git_with_timeout(dir, &args).ok()?;
    if !out.status.success() {
        return None;
    }

    let branch = out.stdout.trim();
    if branch.is_empty() {
        None
    } else {
        Some(branch.to_string())
    }
}

/// Creates a new Git worktree for `branch` under `<base_dir>/<repo_name>/<slug>` and returns its metadata.
pub fn create_worktree(
    repo_dir: &Path,
    branch: &str,
    create_branch: bool,
    base_dir: &Path,
) -> Result<GitWorktreeInfo, String> {
    if !repo_dir.is_dir() {
        return Err(format!(
            "repository path is not a directory: {}",
            repo_dir.display()
        ));
    }
    if branch.starts_with('-') {
        return Err("branch name cannot start with '-'".to_string());
    }
    let slug = sanitize_slug(branch)?;

    let toplevel_args = [OsStr::new("rev-parse"), OsStr::new("--show-toplevel")];
    let toplevel_out = run_git_with_timeout(repo_dir, &toplevel_args)?;
    if !toplevel_out.status.success() {
        return Err(format!(
            "failed to resolve git repository root: {}",
            toplevel_out.stderr.trim()
        ));
    }

    let repo_root_raw = PathBuf::from(toplevel_out.stdout.trim_end_matches(['\r', '\n']));
    let repo_root = canonicalize_path(&repo_root_raw);
    let primary_root = discover_worktrees(&repo_root)
        .ok()
        .and_then(|mut worktrees| (!worktrees.is_empty()).then(|| worktrees.remove(0).path))
        .map(|p| canonicalize_path(&p))
        .unwrap_or_else(|| repo_root.clone());
    let repo_name = primary_root
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .ok_or_else(|| {
            format!(
                "failed to determine repository name from {}",
                primary_root.display()
            )
        })?;

    let parent_dir = base_dir.join(repo_name);
    std::fs::create_dir_all(&parent_dir).map_err(|e| {
        format!(
            "failed to create worktree directory {}: {e}",
            parent_dir.display()
        )
    })?;

    let target = parent_dir.join(&slug);
    if target.exists() {
        return Err(format!(
            "worktree target directory already exists: {}",
            target.display()
        ));
    }

    let add_out = if create_branch {
        let args = [
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("-b"),
            OsStr::new(branch),
            OsStr::new("--"),
            target.as_os_str(),
        ];
        run_git_with_timeout(&repo_root, &args)?
    } else {
        let args = [
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("--"),
            target.as_os_str(),
            OsStr::new(branch),
        ];
        run_git_with_timeout(&repo_root, &args)?
    };

    if !add_out.status.success() {
        return Err(format!(
            "git worktree add failed: {}",
            add_out.stderr.trim()
        ));
    }

    let canonical_target = canonicalize_path(&target);
    let worktrees = discover_worktrees(&repo_root)?;
    worktrees
        .into_iter()
        .find(|wt| canonicalize_path(&wt.path) == canonical_target)
        .ok_or_else(|| {
            format!(
                "created worktree at {} was not found in git worktree list",
                target.display()
            )
        })
}

fn parse_git_status_v2(output: &str, is_worktree: bool) -> (GitWorkspaceStatus, Option<String>) {
    let mut dirty = false;
    let mut ahead = 0u32;
    let mut behind = 0u32;
    let mut branch = None;

    for raw_line in output.lines() {
        let line = raw_line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("# ") {
            if let Some(head) = rest.strip_prefix("branch.head ") {
                if head != "(detached)" && !head.is_empty() {
                    branch = Some(head.to_string());
                }
            } else if let Some(ab) = rest.strip_prefix("branch.ab ") {
                for token in ab.split_whitespace() {
                    if let Some(a) = token.strip_prefix('+') {
                        ahead = a.parse::<u32>().unwrap_or(0);
                    } else if let Some(b) = token.strip_prefix('-') {
                        behind = b.parse::<u32>().unwrap_or(0);
                    }
                }
            }
        } else {
            dirty = true;
        }
    }

    (
        GitWorkspaceStatus {
            is_worktree,
            dirty,
            ahead,
            behind,
        },
        branch,
    )
}

/// Queries `git status --porcelain=v2 --branch` (with a 2s timeout) and returns
/// `(GitWorkspaceStatus, Option<branch_name>)`, or `None` if `path` is not a git repository.
pub fn git_workspace_status_and_branch(
    path: &Path,
) -> Option<(GitWorkspaceStatus, Option<String>)> {
    if !path.is_dir() {
        return None;
    }

    let is_worktree = path.join(".git").is_file();
    let args = [
        OsStr::new("status"),
        OsStr::new("--porcelain=v2"),
        OsStr::new("--branch"),
    ];
    let out = run_git_with_timeout_for(path, &args, GIT_PROBE_TIMEOUT).ok()?;
    if !out.status.success() {
        return None;
    }
    Some(parse_git_status_v2(&out.stdout, is_worktree))
}

/// Queries [`GitWorkspaceStatus`] for `path` with a 2s timeout, returning `None` if not a git repo.
pub fn git_workspace_status(path: &Path) -> Option<GitWorkspaceStatus> {
    git_workspace_status_and_branch(path).map(|(status, _)| status)
}

/// Validates that `path` is a linked secondary worktree (never the primary checkout) and returns
/// the primary repository directory path so removal can run from it.
pub fn validate_worktree_removal(path: &Path, force: bool) -> Result<PathBuf, WorktreeError> {
    if !path.is_dir() {
        return Err(WorktreeError::NotAWorktree(path.to_path_buf()));
    }

    let dot_git = path.join(".git");
    if dot_git.is_dir() {
        return Err(WorktreeError::PrimaryCheckout(path.to_path_buf()));
    }
    if !dot_git.is_file() {
        return Err(WorktreeError::NotAWorktree(path.to_path_buf()));
    }

    let worktrees = discover_worktrees(path).map_err(WorktreeError::GitFailed)?;
    let canon_path = canonicalize_path(path);
    let matched = worktrees
        .iter()
        .find(|wt| canonicalize_path(&wt.path) == canon_path);
    match matched {
        Some(wt) if wt.is_primary => {
            return Err(WorktreeError::PrimaryCheckout(path.to_path_buf()));
        }
        Some(_) => {}
        None => {
            return Err(WorktreeError::NotAWorktree(path.to_path_buf()));
        }
    }

    let primary_repo = worktrees
        .iter()
        .find(|wt| wt.is_primary)
        .map(|wt| wt.path.clone())
        .ok_or_else(|| WorktreeError::NotAWorktree(path.to_path_buf()))?;

    if !force
        && let Some(status) = git_workspace_status(path)
        && status.dirty
    {
        return Err(WorktreeError::DirtyWorktree(path.to_path_buf()));
    }

    Ok(primary_repo)
}

/// Removes a linked secondary git worktree at `path`, refusing to remove a primary repo checkout.
pub fn remove_worktree(path: &Path, force: bool) -> Result<(), WorktreeError> {
    let primary_repo = validate_worktree_removal(path, force)?;
    let target = canonicalize_path(path);

    let mut args = vec![OsStr::new("worktree"), OsStr::new("remove")];
    if force {
        args.push(OsStr::new("--force"));
    }
    args.push(OsStr::new("--"));
    args.push(target.as_os_str());

    let out = run_git_with_timeout(&primary_repo, &args).map_err(WorktreeError::GitFailed)?;
    if !out.status.success() {
        let stderr_lower = out.stderr.to_ascii_lowercase();
        if !force && stderr_lower.contains("modified or untracked files") {
            return Err(WorktreeError::DirtyWorktree(path.to_path_buf()));
        }
        return Err(WorktreeError::GitFailed(format!(
            "git worktree remove failed: {}",
            out.stderr.trim()
        )));
    }

    Ok(())
}

fn truncate_utf8_with_suffix(mut text: String, max_bytes: usize, suffix: &str) -> String {
    if text.len() <= max_bytes {
        return text;
    }
    let target = max_bytes.saturating_sub(suffix.len());
    let mut cut = target.min(text.len());
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text.truncate(cut);
    text.push_str(suffix);
    text
}

/// Captures `(branch, bounded_diff_stat)` (`git status --short` + `git diff --stat`, capped at
/// 4 KiB with a 2s timeout) for enriching [`daedalus_proto::FailureTrace`].
pub fn git_failure_context(path: &Path) -> (Option<String>, Option<String>) {
    if !path.is_dir() {
        return (None, None);
    }

    let branch = {
        let args = [
            OsStr::new("symbolic-ref"),
            OsStr::new("--short"),
            OsStr::new("HEAD"),
        ];
        run_git_with_timeout_for(path, &args, GIT_PROBE_TIMEOUT)
            .ok()
            .filter(|out| out.status.success())
            .map(|out| out.stdout.trim().to_string())
            .filter(|b| !b.is_empty())
    };

    let status_args = [OsStr::new("status"), OsStr::new("--short")];
    let Ok(status_out) = run_git_with_timeout_for(path, &status_args, GIT_PROBE_TIMEOUT) else {
        return (branch, None);
    };
    if !status_out.status.success() {
        return (branch, None);
    }

    let diff_args = [OsStr::new("diff"), OsStr::new("--stat")];
    let diff_out = run_git_with_timeout_for(path, &diff_args, GIT_PROBE_TIMEOUT)
        .ok()
        .filter(|out| out.status.success());

    let status_trimmed = status_out.stdout.trim_end();
    let diff_trimmed = diff_out
        .as_ref()
        .map(|out| out.stdout.trim_end())
        .unwrap_or("");

    let mut combined = String::new();
    if !status_trimmed.is_empty() {
        combined.push_str(status_trimmed);
    }
    if !diff_trimmed.is_empty() {
        if !combined.is_empty() {
            combined.push_str("\n\n");
        }
        combined.push_str(diff_trimmed);
    }

    if combined.is_empty() {
        return (branch, None);
    }

    let bounded =
        truncate_utf8_with_suffix(combined, MAX_FAILURE_CONTEXT_BYTES, "\n...[truncated]");
    (branch, Some(bounded))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn run_git_test(dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("git should execute in test");
        assert!(
            output.status.success(),
            "git {:?} failed: stdout={}, stderr={}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_temp_repo(parent: &Path, name: &str) -> PathBuf {
        let repo = parent.join(name);
        std::fs::create_dir_all(&repo).unwrap();
        run_git_test(&repo, &["init", "-b", "main"]);
        std::fs::write(repo.join("README.md"), "# test\n").unwrap();
        run_git_test(&repo, &["add", "README.md"]);
        run_git_test(
            &repo,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "initial commit",
            ],
        );
        repo
    }

    #[test]
    fn resolves_default_worktrees_dir_in_precedence_order() {
        assert_eq!(
            resolve_worktrees_dir(Some("/custom/wt"), Some("/xdg"), Some("/home/u")),
            PathBuf::from("/custom/wt")
        );
        assert_eq!(
            resolve_worktrees_dir(Some(""), Some("/xdg"), Some("/home/u")),
            PathBuf::from("/xdg/daedalus/worktrees")
        );
        assert_eq!(
            resolve_worktrees_dir(None, Some(""), Some("/home/u")),
            PathBuf::from("/home/u/.local/share/daedalus/worktrees")
        );
        assert_eq!(
            resolve_worktrees_dir(None, None, None),
            std::env::temp_dir().join("daedalus-worktrees")
        );
    }

    #[test]
    fn sanitizes_branch_slugs_and_rejects_invalid_inputs() {
        assert_eq!(sanitize_slug("feat/auth").unwrap(), "feat-auth");
        assert_eq!(
            sanitize_slug("/feat/my_branch.v1/").unwrap(),
            "feat-my_branch.v1"
        );
        assert_eq!(
            sanitize_slug("--leading-and-trailing--").unwrap(),
            "leading-and-trailing"
        );
        assert_eq!(sanitize_slug("fix/issue@42").unwrap(), "fix-issue-42");

        assert!(sanitize_slug("").is_err());
        assert!(sanitize_slug("..").is_err());
        assert!(sanitize_slug("feat/../main").is_err());
        assert!(sanitize_slug("feat auth").is_err());
        assert!(sanitize_slug("feat\tauth").is_err());
        assert!(sanitize_slug("feat\nauth").is_err());
        assert!(sanitize_slug("feat\x00auth").is_err());
        assert!(sanitize_slug("///").is_err());
        assert!(sanitize_slug("---").is_err());
        assert!(sanitize_slug(".").is_err());
    }

    #[test]
    fn parses_git_worktree_porcelain_stanzas() {
        let porcelain = "\
worktree /repos/app\n\
HEAD 1111111111111111111111111111111111111111\n\
branch refs/heads/main\n\
\n\
worktree /worktrees/app/feat-auth\n\
HEAD 2222222222222222222222222222222222222222\n\
branch refs/heads/feat/auth\n\
locked manual reason\n\
\n\
worktree /worktrees/app/detached dir\r\n\
HEAD 3333333333333333333333333333333333333333\r\n\
detached\r\n";

        let parsed = parse_worktree_porcelain(porcelain);
        assert_eq!(parsed.len(), 3);
        assert_eq!(
            parsed[0],
            GitWorktreeInfo {
                path: PathBuf::from("/repos/app"),
                branch: Some("main".to_string()),
                head: "1111111111111111111111111111111111111111".to_string(),
                is_primary: true,
                is_detached: false,
            }
        );
        assert_eq!(
            parsed[1],
            GitWorktreeInfo {
                path: PathBuf::from("/worktrees/app/feat-auth"),
                branch: Some("feat/auth".to_string()),
                head: "2222222222222222222222222222222222222222".to_string(),
                is_primary: false,
                is_detached: false,
            }
        );
        assert_eq!(
            parsed[2],
            GitWorktreeInfo {
                path: PathBuf::from("/worktrees/app/detached dir"),
                branch: None,
                head: "3333333333333333333333333333333333333333".to_string(),
                is_primary: false,
                is_detached: true,
            }
        );
    }

    #[test]
    fn discover_worktrees_returns_empty_for_non_git_directory() {
        let dir = TempDir::new().unwrap();
        let worktrees = discover_worktrees(dir.path()).unwrap();
        assert!(worktrees.is_empty());
        assert_eq!(current_branch(dir.path()), None);
    }

    #[test]
    fn discovers_and_creates_worktrees_for_new_and_existing_branches() {
        let root = TempDir::new().unwrap();
        let base_dir = TempDir::new().unwrap();
        let repo = init_temp_repo(root.path(), "my-app");
        let canon_repo = std::fs::canonicalize(&repo).unwrap();

        assert_eq!(current_branch(&repo), Some("main".to_string()));

        let initial = discover_worktrees(&repo).unwrap();
        assert_eq!(initial.len(), 1);
        assert_eq!(std::fs::canonicalize(&initial[0].path).unwrap(), canon_repo);
        assert_eq!(initial[0].branch, Some("main".to_string()));
        assert!(!initial[0].head.is_empty());
        assert!(initial[0].is_primary);
        assert!(!initial[0].is_detached);

        // Create worktree with a brand new branch
        let created_new = create_worktree(&repo, "feat/auth", true, base_dir.path()).unwrap();
        assert!(!created_new.is_primary);
        assert!(!created_new.is_detached);
        assert_eq!(created_new.branch, Some("feat/auth".to_string()));
        assert_eq!(
            std::fs::canonicalize(&created_new.path).unwrap(),
            std::fs::canonicalize(base_dir.path().join("my-app").join("feat-auth")).unwrap()
        );
        assert_eq!(
            current_branch(&created_new.path),
            Some("feat/auth".to_string())
        );

        // Create an existing branch without checking it out in primary, then attach worktree
        run_git_test(&repo, &["branch", "hotfix/login"]);
        let created_existing =
            create_worktree(&repo, "hotfix/login", false, base_dir.path()).unwrap();
        assert!(!created_existing.is_primary);
        assert!(!created_existing.is_detached);
        assert_eq!(created_existing.branch, Some("hotfix/login".to_string()));
        assert_eq!(
            std::fs::canonicalize(&created_existing.path).unwrap(),
            std::fs::canonicalize(base_dir.path().join("my-app").join("hotfix-login")).unwrap()
        );

        let all = discover_worktrees(&repo).unwrap();
        assert_eq!(all.len(), 3);
        assert!(all[0].is_primary);
        assert!(!all[1].is_primary);
        assert!(!all[2].is_primary);

        // Reject duplicate target and leading dash branch
        assert!(create_worktree(&repo, "feat/auth", true, base_dir.path()).is_err());
        assert!(create_worktree(&repo, "-bad-flag", true, base_dir.path()).is_err());

        // Detached HEAD returns None for current_branch and is_detached = true in discovery
        run_git_test(&created_existing.path, &["checkout", "--detach"]);
        assert_eq!(current_branch(&created_existing.path), None);
        let after_detach = discover_worktrees(&repo).unwrap();
        let detached_entry = after_detach
            .iter()
            .find(|wt| {
                std::fs::canonicalize(&wt.path).unwrap()
                    == std::fs::canonicalize(&created_existing.path).unwrap()
            })
            .unwrap();
        assert!(detached_entry.is_detached);
        assert_eq!(detached_entry.branch, None);
    }

    #[test]
    fn creates_chained_worktree_under_primary_repo_name() {
        let root = TempDir::new().unwrap();
        let base_dir = TempDir::new().unwrap();
        let repo = init_temp_repo(root.path(), "primary-repo");

        let wt1 = create_worktree(&repo, "feat/one", true, base_dir.path()).unwrap();
        assert_eq!(
            std::fs::canonicalize(&wt1.path).unwrap(),
            std::fs::canonicalize(base_dir.path().join("primary-repo").join("feat-one")).unwrap()
        );

        let wt2 = create_worktree(&wt1.path, "feat/two", true, base_dir.path()).unwrap();
        assert_eq!(
            std::fs::canonicalize(&wt2.path).unwrap(),
            std::fs::canonicalize(base_dir.path().join("primary-repo").join("feat-two")).unwrap()
        );
        assert!(!base_dir.path().join("feat-one").join("feat-two").exists());
    }

    #[test]
    fn parses_git_status_v2_ahead_behind_and_dirty_lines() {
        let clean = "\
# branch.oid 1111111111111111111111111111111111111111\n\
# branch.head feat/v0.0.3\n\
# branch.upstream origin/feat/v0.0.3\n\
# branch.ab +3 -2\n";
        assert_eq!(
            parse_git_status_v2(clean, true),
            (
                GitWorkspaceStatus {
                    is_worktree: true,
                    dirty: false,
                    ahead: 3,
                    behind: 2,
                },
                Some("feat/v0.0.3".to_string()),
            )
        );

        let dirty = "\
# branch.oid 1111111111111111111111111111111111111111\n\
# branch.head (detached)\n\
1 .M N... 100644 100644 100644 abc def src/lib.rs\n\
? untracked.txt\n";
        assert_eq!(
            parse_git_status_v2(dirty, false),
            (
                GitWorkspaceStatus {
                    is_worktree: false,
                    dirty: true,
                    ahead: 0,
                    behind: 0,
                },
                None,
            )
        );
    }

    #[test]
    fn git_workspace_status_and_remove_worktree_lifecycle() {
        let root = TempDir::new().unwrap();
        let base_dir = TempDir::new().unwrap();
        let non_git = TempDir::new().unwrap();

        assert_eq!(git_workspace_status(non_git.path()), None);
        assert!(matches!(
            remove_worktree(non_git.path(), false),
            Err(WorktreeError::NotAWorktree(_))
        ));

        let repo = init_temp_repo(root.path(), "app");
        let primary_status = git_workspace_status(&repo).unwrap();
        assert!(!primary_status.is_worktree);
        assert!(!primary_status.dirty);

        // Must refuse to remove primary repo checkout even with force = true
        assert!(matches!(
            remove_worktree(&repo, false),
            Err(WorktreeError::PrimaryCheckout(_))
        ));
        assert!(matches!(
            remove_worktree(&repo, true),
            Err(WorktreeError::PrimaryCheckout(_))
        ));

        let wt_clean = create_worktree(&repo, "feat/clean", true, base_dir.path()).unwrap();
        let clean_status = git_workspace_status(&wt_clean.path).unwrap();
        assert!(clean_status.is_worktree);
        assert!(!clean_status.dirty);

        // Clean worktree removes cleanly without force
        remove_worktree(&wt_clean.path, false).unwrap();
        assert!(!wt_clean.path.exists());

        // Dirty worktree fails without force, succeeds with force
        let wt_dirty = create_worktree(&repo, "feat/dirty", true, base_dir.path()).unwrap();
        std::fs::write(wt_dirty.path.join("README.md"), "# modified\n").unwrap();
        std::fs::write(wt_dirty.path.join("untracked.txt"), "hi\n").unwrap();
        let dirty_status = git_workspace_status(&wt_dirty.path).unwrap();
        assert!(dirty_status.is_worktree);
        assert!(dirty_status.dirty);

        assert!(matches!(
            remove_worktree(&wt_dirty.path, false),
            Err(WorktreeError::DirtyWorktree(_))
        ));
        assert!(wt_dirty.path.exists());

        remove_worktree(&wt_dirty.path, true).unwrap();
        assert!(!wt_dirty.path.exists());
    }

    #[test]
    fn git_failure_context_captures_branch_and_bounded_diff_stat() {
        let non_git = TempDir::new().unwrap();
        assert_eq!(git_failure_context(non_git.path()), (None, None));

        let root = TempDir::new().unwrap();
        let repo = init_temp_repo(root.path(), "ctx-app");
        assert_eq!(git_failure_context(&repo), (Some("main".to_string()), None));

        std::fs::write(repo.join("README.md"), "# modified\nsecond line\n").unwrap();
        std::fs::write(repo.join("extra.txt"), "new file\n").unwrap();
        let (branch, diff_stat) = git_failure_context(&repo);
        assert_eq!(branch.as_deref(), Some("main"));
        let stat = diff_stat.expect("dirty repo should have failure context");
        assert!(stat.contains("README.md"));
        assert!(stat.contains("extra.txt"));
        assert!(stat.len() <= MAX_FAILURE_CONTEXT_BYTES);

        // Verify truncation helper respects byte limit and UTF-8 boundaries
        let long = "é".repeat(3000); // 6000 bytes
        let truncated =
            truncate_utf8_with_suffix(long, MAX_FAILURE_CONTEXT_BYTES, "\n...[truncated]");
        assert!(truncated.len() <= MAX_FAILURE_CONTEXT_BYTES);
        assert!(truncated.ends_with("\n...[truncated]"));
    }
}
