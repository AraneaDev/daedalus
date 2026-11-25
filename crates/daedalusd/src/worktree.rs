mod failure_context;
mod git_command;
mod observation;

pub use failure_context::git_failure_context;
use git_command::run_git_with_timeout;
pub(crate) use observation::{GitObservation, git_observation};
pub use observation::{current_branch, git_workspace_status, git_workspace_status_and_branch};

use daedalus_proto::GitWorktreeInfo;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum WorktreeError {
    #[error("refusing to remove primary repository checkout: {0}")]
    PrimaryCheckout(PathBuf),
    #[error("path is not a linked git worktree: {0}")]
    NotAWorktree(PathBuf),
    #[error("worktree has uncommitted changes (use force to remove): {0}")]
    DirtyWorktree(PathBuf),
    #[error("worktree is locked (unlock it before removing): {0}")]
    LockedWorktree(PathBuf),
    #[error("git command failed: {0}")]
    GitFailed(String),
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

fn canonicalize_path(path: &Path) -> PathBuf {
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    daedalus_proto::strip_windows_verbatim_prefix(resolved)
}

struct WorktreeEntry {
    info: GitWorktreeInfo,
    locked: bool,
}

fn parse_worktree_porcelain(output: &str) -> Vec<WorktreeEntry> {
    #[derive(Default)]
    struct Stanza {
        path: Option<PathBuf>,
        head: String,
        branch: Option<String>,
        is_detached: bool,
        locked: bool,
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
                locked: std::mem::take(&mut current.locked),
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
        } else if line == "locked" || line.starts_with("locked ") {
            current.locked = true;
        }
    }
    flush(&mut stanzas, &mut current);

    stanzas
        .into_iter()
        .enumerate()
        .filter_map(|(idx, s)| {
            Some(WorktreeEntry {
                locked: s.locked,
                info: GitWorktreeInfo {
                    path: s.path?,
                    branch: s.branch,
                    head: s.head,
                    is_primary: idx == 0,
                    is_detached: s.is_detached,
                },
            })
        })
        .collect()
}

/// Discovers Git worktrees for the repository containing `repo_dir`.
///
/// Returns `Ok(Vec::new())` if `repo_dir` is not inside a Git repository.
pub fn discover_worktrees(repo_dir: &Path) -> Result<Vec<GitWorktreeInfo>, String> {
    Ok(list_worktrees(repo_dir)?
        .into_iter()
        .map(|entry| entry.info)
        .collect())
}

/// All linked worktrees resolve to the same canonical primary checkout.
pub(crate) fn primary_checkout(repo_dir: &Path) -> Option<PathBuf> {
    discover_worktrees(repo_dir)
        .ok()?
        .into_iter()
        .find(|entry| entry.is_primary)
        .map(|entry| canonicalize_path(&entry.path))
}

fn list_worktrees(repo_dir: &Path) -> Result<Vec<WorktreeEntry>, String> {
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

/// Creates a new Git worktree for `branch` under `<base_dir>/<repo_name>/<slug>` and returns its metadata.
pub fn create_worktree(
    repo_dir: &Path,
    branch: &str,
    create_branch: bool,
    base_dir: &Path,
) -> Result<GitWorktreeInfo, String> {
    create_worktree_for_registration(repo_dir, branch, create_branch, base_dir)
        .map(|created| created.info)
}

pub(crate) struct CreatedWorktree {
    pub(crate) info: GitWorktreeInfo,
    pub(crate) canonical_target: PathBuf,
    primary_root: PathBuf,
    new_branch: Option<CreatedBranch>,
}

struct CreatedBranch {
    name: String,
    start: Option<String>,
}

pub(crate) fn create_worktree_for_registration(
    repo_dir: &Path,
    branch: &str,
    create_branch: bool,
    base_dir: &Path,
) -> Result<CreatedWorktree, String> {
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
    let primary_root = primary_checkout(&repo_root).unwrap_or_else(|| repo_root.clone());
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

    // Record the original tip before hooks or another Git process can change the new branch.
    let new_branch = create_branch.then(|| CreatedBranch {
        name: branch.to_owned(),
        start: run_git_with_timeout(
            &repo_root,
            &[
                OsStr::new("rev-parse"),
                OsStr::new("--verify"),
                OsStr::new("HEAD"),
            ],
        )
        .ok()
        .filter(|out| out.status.success())
        .map(|out| out.stdout.trim().to_owned()),
    });
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
    let recovery = || {
        format!(
            "git added the checkout at {}. Inspect it with git worktree list before retrying",
            target.display()
        )
    };
    let info = discover_worktrees(&repo_root)
        .map_err(|error| format!("{error}. {}", recovery()))?
        .into_iter()
        .find(|wt| canonicalize_path(&wt.path) == canonical_target)
        .ok_or_else(|| {
            format!(
                "created worktree was not found in git worktree list. {}",
                recovery()
            )
        })?;
    Ok(CreatedWorktree {
        info,
        canonical_target,
        primary_root,
        new_branch,
    })
}

impl CreatedWorktree {
    /// Compensates for failed registration, preserving changes made since Git added the checkout.
    pub(crate) fn rollback(self) -> Result<(), String> {
        let path = &self.info.path;
        if canonicalize_path(path) != self.canonical_target
            || canonicalize_path(
                &validate_worktree_removal(path, false).map_err(|e| e.to_string())?,
            ) != self.primary_root
            || !discover_worktrees(&self.primary_root)?.contains(&self.info)
            || self.new_branch.as_ref().is_some_and(|branch| {
                branch.start.as_ref() != Some(&self.info.head)
                    || Some(&branch.name) != self.info.branch.as_ref()
            })
        {
            return Err("the checkout no longer matches the one created by this request".into());
        }

        // Git's non-forced removal permits ignored files. Preserve those too.
        let status = run_git_with_timeout(
            path,
            &[
                OsStr::new("status"),
                OsStr::new("--porcelain"),
                OsStr::new("--untracked-files=all"),
                OsStr::new("--ignored=matching"),
                OsStr::new("--ignore-submodules=none"),
            ],
        )?;
        if !status.status.success() {
            return Err(format!(
                "could not check checkout: {}",
                status.stderr.trim()
            ));
        }
        if !status.stdout.is_empty() {
            return Err("the checkout contains local files or changes".into());
        }

        remove_worktree(path, false).map_err(|e| e.to_string())?;
        if let Some(CreatedBranch { name: branch, .. }) = self.new_branch {
            if discover_worktrees(&self.primary_root)?
                .iter()
                .any(|worktree| worktree.branch.as_deref() == Some(&branch))
            {
                return Err(format!("branch {branch} is now checked out elsewhere"));
            }
            let branch_ref = format!("refs/heads/{branch}");
            // Compare the tip atomically so an updated branch is never discarded.
            let removed = run_git_with_timeout(
                &self.primary_root,
                &[
                    OsStr::new("update-ref"),
                    OsStr::new("--no-deref"),
                    OsStr::new("-d"),
                    OsStr::new(&branch_ref),
                    OsStr::new(&self.info.head),
                ],
            )?;
            if !removed.status.success() {
                return Err(format!(
                    "could not roll back branch {branch}: {}",
                    removed.stderr.trim()
                ));
            }
        }
        Ok(())
    }
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

    let worktrees = list_worktrees(path).map_err(WorktreeError::GitFailed)?;
    let canon_path = canonicalize_path(path);
    let matched = worktrees
        .iter()
        .find(|wt| canonicalize_path(&wt.info.path) == canon_path);
    match matched {
        Some(wt) if wt.info.is_primary => {
            return Err(WorktreeError::PrimaryCheckout(path.to_path_buf()));
        }
        Some(wt) if wt.locked => return Err(WorktreeError::LockedWorktree(path.to_owned())),
        Some(_) => {}
        None => {
            return Err(WorktreeError::NotAWorktree(path.to_path_buf()));
        }
    }

    let primary_repo = worktrees
        .iter()
        .find(|wt| wt.info.is_primary)
        .map(|wt| wt.info.path.clone())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
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

    pub(super) fn init_temp_repo(parent: &Path, name: &str) -> PathBuf {
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

        let entries = parse_worktree_porcelain(porcelain);
        assert!(entries[1].locked);
        let parsed: Vec<_> = entries.into_iter().map(|entry| entry.info).collect();
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
        assert_eq!(
            primary_checkout(&created_new.path),
            Some(canonicalize_path(&repo))
        );
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
    fn create_rejects_invalid_roots_and_preserves_existing_targets_and_branches() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        assert!(
            create_worktree(&missing, "a", true, dir.path())
                .unwrap_err()
                .contains("not a directory")
        );
        assert!(
            discover_worktrees(&missing)
                .unwrap_err()
                .contains("not a directory")
        );
        assert!(current_branch(&missing).is_none());
        assert!(matches!(
            validate_worktree_removal(&missing, true),
            Err(WorktreeError::NotAWorktree(_))
        ));
        assert!(
            create_worktree(dir.path(), "a", true, dir.path())
                .unwrap_err()
                .contains("failed to resolve git repository root")
        );
        let repo = init_temp_repo(dir.path(), "repo");
        let base = dir.path().join("worktrees");
        let target = base.join("repo").join("topic");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("user-file"), "preserved").unwrap();
        let err = create_worktree(&repo, "topic", true, &base).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(
            std::fs::read_to_string(target.join("user-file")).unwrap(),
            "preserved"
        );
        let err = create_worktree(&repo, "unknown-existing-branch", false, &base).unwrap_err();
        assert!(err.contains("git worktree add failed"), "{err}");
        let blocked_base = dir.path().join("regular-file");
        std::fs::write(&blocked_base, "keep").unwrap();
        assert!(
            create_worktree(&repo, "new", true, &blocked_base)
                .unwrap_err()
                .contains("failed to create worktree directory")
        );
        assert_eq!(discover_worktrees(&repo).unwrap().len(), 1);
    }

    #[test]
    fn worktree_rollback_preserves_ignored_files_and_new_commits() {
        let root = TempDir::new().unwrap();
        let base = root.path().join("worktrees");
        let repo = init_temp_repo(root.path(), "repo");
        std::fs::write(repo.join(".git/info/exclude"), "local-cache\n").unwrap();

        let ignored = create_worktree_for_registration(&repo, "ignored", true, &base).unwrap();
        let file = ignored.info.path.join("local-cache");
        std::fs::write(&file, "keep this").unwrap();
        assert!(ignored.rollback().unwrap_err().contains("local files"));
        assert_eq!(std::fs::read_to_string(file).unwrap(), "keep this");

        let advanced = create_worktree_for_registration(&repo, "advanced", true, &base).unwrap();
        let path = advanced.info.path.clone();
        run_git_test(
            &path,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.com",
                "commit",
                "--allow-empty",
                "-m",
                "new work",
            ],
        );
        assert!(
            advanced
                .rollback()
                .unwrap_err()
                .contains("no longer matches")
        );
        assert_eq!(current_branch(&path).as_deref(), Some("advanced"));
    }

    #[cfg(unix)]
    #[test]
    fn worktree_rollback_preserves_commits_made_by_checkout_hooks() {
        use std::os::unix::fs::PermissionsExt;

        let root = TempDir::new().unwrap();
        let repo = init_temp_repo(root.path(), "repo");
        let hook = repo.join(".git/hooks/post-checkout");
        std::fs::write(
            &hook,
            "#!/bin/sh\ngit -c user.name=Fixture -c user.email=fixture@example.com commit --allow-empty -m hook\n",
        )
        .unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();

        let created =
            create_worktree_for_registration(&repo, "hook", true, &root.path().join("worktrees"))
                .unwrap();
        let path = created.info.path.clone();
        assert!(
            created
                .rollback()
                .unwrap_err()
                .contains("no longer matches")
        );
        assert_eq!(current_branch(&path).as_deref(), Some("hook"));
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
}
