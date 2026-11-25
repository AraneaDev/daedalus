use super::git_command::{GIT_PROBE_TIMEOUT, run_git_with_timeout_for};
use std::ffi::OsStr;
use std::path::Path;

const MAX_FAILURE_CONTEXT_BYTES: usize = 4096;

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
    use super::super::tests::init_temp_repo;
    use super::*;
    use tempfile::TempDir;

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
