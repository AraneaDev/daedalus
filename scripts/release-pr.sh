#!/usr/bin/env bash
set -euo pipefail

sync_lockfile() {
    local branch=$1
    local script_dir
    script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
    python3 "$script_dir/sync-release-lock.py"
    if ! git diff --quiet -- Cargo.lock; then
        git config user.name 'github-actions[bot]'
        git config user.email '41898282+github-actions[bot]@users.noreply.github.com'
        git add Cargo.lock
        git commit -m 'chore: synchronize release workspace lockfile'
        git push origin "HEAD:refs/heads/$branch"
    fi
    printf 'head=%s\n' "$(git rev-parse HEAD)" >> "$GITHUB_OUTPUT"
}

verify_checks() {
    local pr=$1 branch=$2 head=$3 status runs
    # These are the GitHub Actions checks required by master branch protection.
    local required='(.name == "check" and .workflowName == "CI") or
                    (.name == "title" and .workflowName == "Commit style")'

    for ((attempt = 0; attempt < 90; attempt++)); do
        status=$(gh pr view "$pr" --json headRefOid,statusCheckRollup)
        if ! jq -e --arg head "$head" '.headRefOid == $head' <<< "$status" >/dev/null; then
            echo "::error::Release PR #$pr moved past $head. Rerun Release Please."
            return 1
        fi
        runs=$(gh run list --branch "$branch" --commit "$head" --event pull_request \
            --json workflowName,createdAt,status,conclusion)
        runs=$(jq '
            map(select(.workflowName == "CI" or .workflowName == "Commit style"))
            | group_by(.workflowName)
            | map(max_by(.createdAt))
        ' <<< "$runs")
        if jq -e '
            any(.[]; .conclusion == "action_required" or .status == "waiting")
        ' <<< "$runs" >/dev/null; then
            echo "::error::GitHub requires approval for release PR #$pr at $head. Select Approve workflows to run on the PR, then rerun this job."
            return 1
        fi
        if jq -e "
            [.statusCheckRollup[]? | select($required) | .name] | unique
            | . == [\"check\", \"title\"]
        " <<< "$status" >/dev/null; then
            echo "Required checks are attached to release PR #$pr at $head."
            return 0
        fi
        # GitHub can show a blocked workflow as a failed run with zero attached jobs.
        if [[ ${RELEASE_PR_TOKEN:-app} == github && $attempt -ge 6 ]] && jq -e '
            all(.[]; .status == "completed")
        ' <<< "$runs" >/dev/null; then
            echo "::error::GitHub did not attach the required checks to release PR #$pr at $head. Review the PR for Approve workflows to run, or configure the release GitHub App."
            return 1
        fi
        sleep 15
    done
    echo "::error::Release PR #$pr at $head did not receive both required checks (check and title). Review the pull request workflows and their approval status."
    return 1
}

case "$1" in
    sync) sync_lockfile "$2" ;;
    verify) verify_checks "$2" "$3" "$4" ;;
    *) echo "Usage: $0 sync BRANCH | verify PR BRANCH HEAD" >&2; exit 2 ;;
esac
