use super::git_command::{GIT_PROBE_TIMEOUT, run_git_with_timeout, run_git_with_timeout_for};
use daedalus_proto::GitWorkspaceStatus;
use std::ffi::OsStr;
use std::path::Path;

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

fn parse_git_status_v2(output: &str, is_worktree: bool) -> GitObservation {
    let mut changes = 0;
    let mut ahead = 0u32;
    let mut behind = 0u32;
    let mut branch = None;
    let mut upstream = false;

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
                upstream = true;
                for token in ab.split_whitespace() {
                    if let Some(a) = token.strip_prefix('+') {
                        ahead = a.parse::<u32>().unwrap_or(0);
                    } else if let Some(b) = token.strip_prefix('-') {
                        behind = b.parse::<u32>().unwrap_or(0);
                    }
                }
            }
        } else {
            changes += 1;
        }
    }

    GitObservation {
        status: GitWorkspaceStatus {
            is_worktree,
            dirty: changes != 0,
            ahead,
            behind,
        },
        branch,
        changes,
        upstream,
    }
}

pub(crate) struct GitObservation {
    pub status: GitWorkspaceStatus,
    pub branch: Option<String>,
    pub changes: usize,
    pub upstream: bool,
}

/// Queries `git status --porcelain=v2 --branch` (with a 2s timeout) and returns
/// `(GitWorkspaceStatus, Option<branch_name>)`, or `None` if `path` is not a git repository.
pub fn git_workspace_status_and_branch(
    path: &Path,
) -> Option<(GitWorkspaceStatus, Option<String>)> {
    git_observation(path).map(|observation| (observation.status, observation.branch))
}

pub(crate) fn git_observation(path: &Path) -> Option<GitObservation> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_git_status_v2_ahead_behind_and_dirty_lines() {
        let clean = "\
# branch.oid 1111111111111111111111111111111111111111\n\
# branch.head feat/v0.0.3\n\
# branch.upstream origin/feat/v0.0.3\n\
# branch.ab +3 -2\n";
        let clean = parse_git_status_v2(clean, true);
        assert!(clean.upstream);
        assert_eq!(clean.changes, 0);
        assert_eq!(
            (clean.status, clean.branch),
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
        let dirty = parse_git_status_v2(dirty, false);
        assert!(!dirty.upstream);
        assert_eq!(dirty.changes, 2);
        assert_eq!(
            (dirty.status, dirty.branch),
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
}
