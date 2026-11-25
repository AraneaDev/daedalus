#!/usr/bin/env bash
# Capture the actual Linux workbench on a private X display using a disposable project.
set -euo pipefail

root=$(git rev-parse --show-toplevel)
cd "$root"
if [[ ${1:-} != --capture ]]; then
    for tool in xvfb-run xauth xdotool import convert python3; do
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
command = ["sh", "-c", "while [ ! -f .git/capture ]; do sleep .1; done; printf '$ git status --short --branch\n'; git status --short --branch; printf '\n$ git diff -- health.py\n'; git --no-pager diff --color=always -- health.py; touch .git/agent-ready; while [ ! -f .git/preview ]; do sleep .1; done; printf '\\033[2J\\033[HProbe report written to:\nhealth-report.png\n'; exec cat"]
[service]
command = ["sh", "-c", "while [ ! -f .git/capture ]; do sleep .1; done; printf '$ python3 -m json.tool response.json\n'; python3 -m json.tool response.json; touch .git/service-ready; exec cat"]
[service.widget]
label = "Health endpoint"
[runner]
command = ["sh", "-c", "while [ ! -f .git/capture ]; do sleep .1; done; printf '$ python3 -m unittest -v\n'; python3 -B -m unittest -v && touch .git/runner-ready; while [ ! -f .git/fail ]; do sleep .1; done; printf '\\033[2J\\033[H'; echo FAIL: health_check; echo Expected HTTP 200, received 503; exit 3"]
[runner.widget]
preset = "command"
label = "Health checks"
show_duration = false
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
# Use a deterministic host tool inventory for the environment review.
mkdir -p "$scratch/bin"
for tool in sh git python3 cat sleep touch uname env; do ln -s "$(command -v "$tool")" "$scratch/bin/$tool"; done
ln -s /bin/true "$scratch/bin/npm"
PATH="$scratch/bin" "$bin/daedalusd" --runtime-dir "$scratch/daemon" --state-file "$scratch/state.json" \
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
window=$(xdotool search --sync --onlyvisible --name '^Daedalus Workbench$' | head -1)
xdotool windowsize --sync "$window" 1000 570
xdotool windowfocus --sync "$window"
xdotool key ctrl+shift+z
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
xdotool key ctrl+shift+z
capture focused-workbench
# Use a small project artifact to exercise daemon-side PNG preview end to end.
convert -size 240x128 xc:'#17242c' -fill '#afc9c1' -font DejaVu-Sans -pointsize 13 \
    -annotate +14+23 'Health probe latency' -stroke '#355059' -strokewidth 1 \
    -draw 'line 14,49 225,49 line 14,76 225,76 line 14,103 225,103' \
    -stroke '#f3ab65' -strokewidth 3 -fill none \
    -draw 'polyline 14,86 38,86 58,67 77,81 94,63 113,77 134,55 153,74 175,61 199,74 225,67' \
    PNG32:health-report.png
touch .git/preview
sleep .4
xdotool mousemove --window "$window" 9 59 mousedown 1 mousemove --window "$window" 175 59 mouseup 1
xdotool key ctrl+shift+i
xdotool mousemove 1439 899
capture image-preview
# Check the actual plot stroke so a closed panel or path selection fails the capture.
if ! convert "$scratch/image-preview.png" -crop 320x220+435+20 +repage -format %c histogram:info:- | \
    awk 'toupper($0) ~ /#F3AB65/ && $1 + 0 >= 100 { found = 1 } END { exit !found }'; then
    echo 'The image preview did not display the health report chart.' >&2
    exit 1
fi
xdotool key ctrl+shift+i
xdotool key ctrl+shift+w
xdotool type --delay 30 'feature/add-metrics'
capture new-worktree
if cmp -s "$scratch/workbench.png" "$scratch/new-worktree.png"; then
    echo "The worktree dialog did not appear in the second capture." >&2
    exit 1
fi
xdotool key Escape
xdotool key ctrl+shift+s
sleep .4
# Add a real preset so the screenshot shows editable configuration.
xdotool mousemove --window "$window" 734 153 click 1
xdotool key ctrl+a
xdotool type --delay 30 'Reviewer'
xdotool mousemove 1439 899
capture workspace-settings
xdotool key Escape
touch .git/fail
sleep 1
xdotool key ctrl+shift+r
capture failed-run

# Start the first-workspace flow from an unconfigured example repository.
kill "$ui_pid"
wait "$ui_pid" || true
ui_pid=
mkdir -p "$scratch/example/orbit"
cd "$scratch/example/orbit"
echo '{"scripts":{"dev":"vite","test":"vitest"}}' > package.json
echo '{}' > package-lock.json
echo -e '[agent]\ncommand = ["codex"]' > .daedalus.toml
"$bin/daedalus-ui" --config "$scratch/ui.toml" --no-auto-spawn >"$scratch/review.log" 2>&1 &
ui_pid=$!
window=$(xdotool search --sync --onlyvisible --name '^Daedalus Workbench$' | head -1)
xdotool windowsize --sync "$window" 1000 570
xdotool windowfocus --sync "$window"
sleep 1
xdotool key Escape ctrl+shift+n
sleep .4
xdotool key ctrl+a
xdotool type --delay 2 "$PWD"
xdotool key Return
capture environment-setup

for name in workbench focused-workbench image-preview new-worktree workspace-settings environment-setup failed-run; do
    install -m644 "$scratch/$name.png" "$out/$name.png"
done
