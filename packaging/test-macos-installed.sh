#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 3 ]]; then
    echo "Usage: bash packaging/test-macos-installed.sh UI DAEMON CLI" >&2
    exit 2
fi
ui=$1
daemon=$2
cli=$3
timeout=${DAEDALUS_SMOKE_TIMEOUT_SECONDS:-25}
if [[ ! "$timeout" =~ ^[1-9][0-9]*$ ]]; then
    echo "DAEDALUS_SMOKE_TIMEOUT_SECONDS must be a positive integer" >&2
    exit 2
fi

# Keep Unix domain socket paths short on macOS.
root=$(mktemp -d /tmp/daedalus-pkg.XXXXXX)
daemon_pid=
ui_pid=
ui_phase=
daemon_log="$root/daemon.log"
first_ui_log="$root/ui-first.log"
second_ui_log="$root/ui-second.log"
check_log="$root/check.log"

stop_process() {
    local pid=$1
    [[ -n "$pid" ]] || return 0
    if kill "$pid" 2>/dev/null; then
        for _ in {1..30}; do
            kill -0 "$pid" 2>/dev/null || break
            sleep 0.1
        done
        kill -KILL "$pid" 2>/dev/null || true
    fi
    wait "$pid" 2>/dev/null || true
}

cleanup() {
    local status=$?
    local log
    stop_process "$ui_pid"
    stop_process "$daemon_pid"
    if (( status != 0 )); then
        for log in "$daemon_log" "$first_ui_log" "$second_ui_log" "$check_log"; do
            if [[ -f "$log" ]]; then
                printf '\n--- %s ---\n' "${log##*/}" >&2
                tail -n 50 "$log" >&2
            fi
        done
    fi
    rm -rf -- "$root"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

export DAEDALUS_RUNTIME_DIR="$root"
export DAEDALUS_SOCKET="$root/daemon.sock"
export DAEDALUS_EVENTS_SOCKET="$root/events.sock"
export DAEDALUS_STATE_FILE="$root/state.json"
export DAEDALUS_CONFIG="$root/config.toml"
unset DAEDALUS_AUTH_TOKEN
project="$root/installer-smoke"
mkdir "$project"

check_smoke() {
    python3 - "$cli" "$@" <<'PY'
import json
import subprocess
import sys
from pathlib import Path
from uuid import UUID

cli, mode, *paths = sys.argv[1:]


def run_cli(*args):
    try:
        return subprocess.run(
            [cli, *args], check=True, capture_output=True, text=True, timeout=3
        ).stdout
    except subprocess.CalledProcessError as error:
        sys.exit(f"Installed CLI exited with status {error.returncode}: {error.stderr.strip()}")
    except subprocess.TimeoutExpired:
        sys.exit("The installed CLI did not reply within three seconds.")


if mode == "ping":
    run_cli("ping")
elif mode == "inventory":
    try:
        entries = json.loads(run_cli("workspace", "list", "--json"))
        if not isinstance(entries, list) or len(entries) != 1:
            raise ValueError("expected exactly one workspace")
        workspace = entries[0]
        if workspace["name"] != "installer-smoke":
            raise ValueError("expected a workspace named installer-smoke")
        workspace_id = str(UUID(workspace["id"]))
        panes = workspace["pane_ids"]
        if not isinstance(panes, list):
            raise ValueError("expected a list of pane IDs")
        pane_ids = sorted({str(UUID(pane)) for pane in panes})
        if len(panes) != 3 or len(pane_ids) != 3:
            raise ValueError("expected exactly three distinct pane IDs")
    except (AttributeError, KeyError, TypeError, ValueError) as error:
        sys.exit(f"Invalid workspace inventory: {error}")
    print(json.dumps({"workspace_id": workspace_id, "pane_ids": pane_ids}))
elif mode == "hydration":
    identity, log = map(Path, paths)
    expected = json.loads(identity.read_text())
    missing = set(expected["pane_ids"])
    marker = " daedalus_ui::bridge::session_init: terminal snapshot hydrated "
    with log.open(encoding="utf-8", errors="replace") as records:
        for line in records:
            _, found, raw_fields = line.partition(marker)
            if not found:
                continue
            fields = dict(field.split("=", 1) for field in raw_fields.split() if "=" in field)
            if fields.get("workspace_id") == expected["workspace_id"]:
                missing.discard(fields.get("pane_id"))
    if missing:
        sys.exit("Missing hydration records for panes: " + ", ".join(sorted(missing)))
PY
}

require_running() {
    if ! kill -0 "$daemon_pid" 2>/dev/null; then
        echo "The installed daemon exited during the smoke test." >&2
        exit 1
    fi
    if [[ -n "$ui_pid" ]] && ! kill -0 "$ui_pid" 2>/dev/null; then
        echo "The $ui_phase UI exited during the smoke test." >&2
        exit 1
    fi
}

wait_for() {
    local description=$1
    shift
    local deadline=$((SECONDS + timeout))
    while true; do
        require_running
        if "$@" 2>"$check_log"; then
            require_running
            return
        fi
        if (( SECONDS >= deadline )); then
            echo "Timed out waiting for $description." >&2
            exit 1
        fi
        sleep 0.2
    done
}

start_ui() {
    ui_phase=$1
    NO_COLOR=1 RUST_LOG=info,daedalus_ui::bridge::session_init=debug \
        "$ui" --dir "$project" --agent shell --no-auto-spawn >"$2" 2>&1 &
    ui_pid=$!
}

"$daemon" >"$daemon_log" 2>&1 &
daemon_pid=$!
wait_for "the installed daemon to answer ping" check_smoke ping

identity="$root/identity.json"
start_ui first "$first_ui_log"
wait_for "the first UI to create the workspace and panes" check_smoke inventory >"$identity"
stop_process "$ui_pid"
ui_pid=

start_ui reopened "$second_ui_log"
wait_for "the reopened UI to hydrate the original panes" check_smoke hydration "$identity" "$second_ui_log"
if ! check_smoke inventory >"$root/reopened.json" 2>"$check_log" ||
    ! cmp -s "$identity" "$root/reopened.json"; then
    echo "Reconnect changed the workspace or pane IDs, or the inventory could not be read." >&2
    printf 'Original inventory: %s\n' "$(cat "$identity")" >&2
    printf 'Reopened inventory: %s\n' "$(cat "$root/reopened.json")" >&2
    exit 1
fi
require_running
printf 'The reopened UI hydrated all 3 original panes: %s\n' "$(cat "$identity")"
