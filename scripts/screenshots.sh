#!/usr/bin/env bash
# Capture the actual Linux workbench on a private X display using a disposable project.
set -euo pipefail

root=$(git rev-parse --show-toplevel)
cd "$root"
if [[ ${1:-} != --capture ]]; then
    for tool in xvfb-run xauth xdotool import python3; do
        if ! command -v "$tool" >/dev/null; then
            echo "Screenshot capture requires $tool." >&2
            exit 1
        fi
    done
    cargo build --locked --workspace
    exec xvfb-run -a -s '-screen 0 1440x900x24 -nolisten tcp' "$root/scripts/screenshots.sh" --capture
fi

out="$root/docs/images"
scratch=$(mktemp -d)
daemon_pid=
ui_pid=
cleanup() {
    if [[ -n $ui_pid ]]; then kill "$ui_pid" 2>/dev/null || true; wait "$ui_pid" 2>/dev/null || true; fi
    if [[ -n $daemon_pid ]]; then kill "$daemon_pid" 2>/dev/null || true; wait "$daemon_pid" 2>/dev/null || true; fi
    rm -rf -- "$scratch"
}
trap cleanup EXIT
trap 'exit 1' INT TERM
export XDG_RUNTIME_DIR="$scratch/runtime" XDG_CONFIG_HOME="$scratch/config" XDG_STATE_HOME="$scratch/state"
export DAEDALUS_SOCKET="$scratch/daemon.sock" GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
unset WAYLAND_DISPLAY DAEDALUS_AUTH_TOKEN
project="$scratch/orbit"
mkdir -p "$project" "$out" "$XDG_RUNTIME_DIR" "$XDG_CONFIG_HOME" "$XDG_STATE_HOME"
chmod 700 "$XDG_RUNTIME_DIR"
bin=$(cargo metadata --format-version 1 --no-deps --locked | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"] + "/debug")')
cd "$project"
git init -q -b feature/healthcheck
cat > health.py <<'PY'
def health():
    return {"status": "ok"}
PY
cat > test_health.py <<'PY'
import unittest
from health import health


class HealthTests(unittest.TestCase):
    def test_endpoint_reports_ready(self):
        self.assertEqual(health()["status"], "ok")
PY
cat > .daedalus.toml <<'TOML'
[agent]
command = ["sh", "-c", "while [ ! -f .git/capture ]; do sleep .1; done; printf '$ git status --short --branch\n'; git status --short --branch; printf '\n$ git diff -- health.py\n'; git --no-pager diff --color=always -- health.py; touch .git/agent-ready; exec cat"]
[service]
command = ["sh", "-c", "while [ ! -f .git/capture ]; do sleep .1; done; printf '$ python3 -m json.tool response.json\n'; python3 -m json.tool response.json; touch .git/service-ready; exec cat"]
[runner]
command = ["sh", "-c", "while [ ! -f .git/capture ]; do sleep .1; done; printf '$ python3 -m unittest -v\n'; python3 -B -m unittest -v && touch .git/runner-ready; exec cat"]
[[actions]]
id = "test"
label = "Run health checks"
target = "runner"
command = "python3 -m unittest -v"
TOML
echo '{"service":"orbit","endpoint":"/health","status":"ok"}' > response.json
git add .
git -c user.name=Example -c user.email=demo@example.invalid -c commit.gpgsign=false commit -qm 'Add health endpoint'
echo '# Readiness probe for the service.' >> health.py
echo 'font_family = "DejaVu Sans Mono"' > "$scratch/ui.toml"
"$bin/daedalusd" --runtime-dir "$scratch/daemon" --state-file "$scratch/state.json" \
    --events-socket "$scratch/events.sock" >"$scratch/daemon.log" 2>&1 &
daemon_pid=$!
for ((i=0; i<100; i++)); do
    if "$bin/daedalus-ctl" ping >/dev/null 2>&1; then break; fi
    sleep .1
done
"$bin/daedalus-ui" --dir "$project" --config "$scratch/ui.toml" --agent shell --no-auto-spawn >"$scratch/ui.log" 2>&1 &
ui_pid=$!
# Wait for all three PTYs before printing. Their initial fit to the window can
# move early output into scrollback, particularly in the two shorter panes.
for ((i=0; i<150; i++)); do
    listing=$("$bin/daedalus-ctl" workspace list 2>/dev/null || true)
    if [[ $listing == *panes=3* ]]; then break; fi
    sleep .1
done
sleep 1
touch .git/capture
for ((i=0; i<200; i++)); do
    window=$(xdotool search --onlyvisible --name '^Daedalus Workbench$' 2>/dev/null | head -1 || true)
    if [[ -n $window && -f .git/agent-ready && -f .git/service-ready && -f .git/runner-ready ]]; then break; fi
    if ! kill -0 "$ui_pid" 2>/dev/null; then break; fi
    sleep .1
done
if [[ -z $window || ! -f .git/agent-ready || ! -f .git/service-ready || ! -f .git/runner-ready ]]; then
    cat "$scratch/ui.log" "$scratch/daemon.log" >&2
    exit 1
fi
xdotool windowfocus --sync "$window"
xdotool mousemove 1439 899
capture() {
    sleep 1
    import -window "$window" -strip "$scratch/$1.png"
}
capture workbench
xdotool key ctrl+shift+w
xdotool type --delay 30 'feature/add-metrics'
capture new-worktree
if cmp -s "$scratch/workbench.png" "$scratch/new-worktree.png"; then
    echo "The worktree dialog did not appear in the second capture." >&2
    exit 1
fi
xdotool key Escape ctrl+shift+s
# Exercise the saved assignment flow and capture a readable agent definition.
xdotool mousemove --window "$window" 940 385 click 1
xdotool type --delay 30 'Reviewer'
xdotool mousemove --window "$window" 720 580 click 1
for ((i=0; i<50; i++)); do
    if grep -rq 'name = "Reviewer"' "$XDG_CONFIG_HOME"; then break; fi
    sleep .1
done
if ! grep -rq 'name = "Reviewer"' "$XDG_CONFIG_HOME"; then
    echo "The named assignment was not saved." >&2
    exit 1
fi
xdotool key ctrl+shift+s
xdotool mousemove --window "$window" 505 350 click 1
xdotool mousemove 1439 899
capture workspace-settings
if cmp -s "$scratch/workbench.png" "$scratch/workspace-settings.png"; then
    echo "The workspace settings dialog did not appear." >&2
    exit 1
fi
for name in workbench new-worktree workspace-settings; do install -m644 "$scratch/$name.png" "$out/$name.png"; done
