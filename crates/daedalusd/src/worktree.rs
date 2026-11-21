use daedalus_proto::GitWorktreeInfo;
use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

const GIT_TIMEOUT: Duration = Duration::from_secs(5);

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

fn spawn_reader<R: Read + Send + 'static>(mut reader: R) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = reader.read_to_end(&mut buf);
        buf
    })
}

fn run_git_with_timeout(repo_dir: &Path, args: &[&OsStr]) -> Result<GitOutput, String> {
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

    let deadline = Instant::now() + GIT_TIMEOUT;
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
                    return Err("git command timed out after 5s".to_string());
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
        return Err(format!(
            "git worktree list failed: {}",
            out.stderr.trim()
        ));
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
    let repo_root = std::fs::canonicalize(&repo_root_raw).unwrap_or(repo_root_raw);
    let repo_name = repo_root
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .ok_or_else(|| {
            format!(
                "failed to determine repository name from {}",
                repo_root.display()
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

    let canonical_target = std::fs::canonicalize(&target).unwrap_or_else(|_| target.clone());
    let worktrees = discover_worktrees(&repo_root)?;
    worktrees
        .into_iter()
        .find(|wt| {
            let wt_canon = std::fs::canonicalize(&wt.path).unwrap_or_else(|_| wt.path.clone());
            wt_canon == canonical_target
        })
        .ok_or_else(|| {
            format!(
                "created worktree at {} was not found in git worktree list",
                target.display()
            )
        })
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
        assert_eq!(current_branch(&created_new.path), Some("feat/auth".to_string()));

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
}
