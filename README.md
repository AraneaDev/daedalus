# Daedalus

**Local-first semantic runtime & coding-agent workbench for Linux (Wayland / X11).**

Daedalus orchestrates **coding agents (`agy`, `claude`, `codex`, or `$SHELL`)**, **Docker Compose / dev service stacks**, and **test/build runners** inside isolated git worktree workspaces — backed by a persistent headless Rust daemon (`daedalusd`) and a native GPU-rendered terminal UI (`daedalus-ui`).

---

## Key Capabilities

- **Persistent Headless Daemon (`daedalusd`):** Owns PTYs, process groups/cgroups (`systemd-run --user --scope` with `setsid` + `killpg` fallback), and VT state machines over a Unix domain socket (`$XDG_RUNTIME_DIR/daedalus/daedalusd.sock`). Closing or restarting the UI never kills running agents, dev servers, or tests.
- **Dual-Backend VT Engine (`daedalus-vt`):** Pure-Rust `BuiltinVt` default backend (zero external C deps) plus feature-gated `--features ghostty-vt` `libghostty-vt` 1.0 C ABI FFI backend (`GHOSTTY_VT_DIR` / `pkg-config`).
- **Semantic Shell Introspection (OSC 7 & OSC 133):** Non-destructive `bash` and `zsh` hooks track CWD, prompt/command lifecycle, and exit codes. On non-zero exits in the `Runner` pane, Daedalus slices the plain-text, ANSI-stripped failure output and forwards it straight into the `Agent` pane via sanitized bracketed paste (`Ctrl+Shift+E`).
- **Native GPU Workbench (`daedalus-ui`):** `winit` + `wgpu` + `glyphon`/`cosmic-text` two-pass GPU renderer with a fixed 3-pane semantic layout:
  - **Left (55%):** `Agent` (`agy`, `claude`, `codex`, or `$SHELL` via `Ctrl+Shift+P`)
  - **Right-Top (22.5%):** `Service` (Docker Compose / dev server)
  - **Right-Bottom (22.5%):** `Runner` (tests / build)
- **Worktree Port & Compose Isolation:** One-click/hotkey git worktree creation (`Ctrl+Shift+W` or `daedalus-ctl worktree create`) allocates conflict-free port blocks (`slot_index * 100` probed via `TcpListener::bind` + cross-workspace port reservation), writes `.daedalus/compose.override.yml` (Compose v2 `!override`), and injects `COMPOSE_PROJECT_NAME`, `COMPOSE_FILE`, and remapped `PORT` / `*_PORT` / `*_URL` variables into every workspace PTY.
- **Live Runtime & External Status Sidebar:** Polls Docker container health/ports via `bollard` and local listening ports via `/proc/net/tcp{,6}`, plus ingests external tool status badges (e.g., `phrourion`, `cassandra`) over `$XDG_RUNTIME_DIR/daedalus/events.sock` (NDJSON) or `daedalus-ctl status publish`.

---

## Workspace Architecture

| Crate | Role |
|-------|------|
| `crates/daedalus-proto` | Shared domain IDs, RPC/event schema, 4-byte length-prefixed MessagePack codec, sanitized bracketed-paste helper |
| `crates/daedalus-vt` | `VtEngine` trait, `BuiltinVt` (`vte`), `CommandTracker` (OSC 7/133), feature-gated `libghostty-vt` FFI |
| `crates/daedalusd` | Headless daemon: `PaneActor` (60Hz coalesced deltas), `Supervisor`, `ShellIntegration`, `worktree`, `isolation`, `runtime_monitor`, `external_hook` |
| `crates/daedalus-ui` | Native Wayland/X11 GPU terminal UI (`winit` + `wgpu` + `glyphon`), `DaemonBridge`, 3-pane layout, sidebar, modals |
| `crates/daedalus-ctl` | Scriptable CLI control & smoke client |

---

## Quick Start

### Prerequisites (Linux)
- Rust toolchain (2024 edition, `rustc >= 1.85`)
- Linux GUI headers (for `daedalus-ui`):
  ```bash
  sudo apt-get install -y pkg-config libxkbcommon-dev libwayland-dev libxcb1-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev
  ```

### Build & Run
```bash
# Build all binaries
cargo build --release --workspace

# Launch the GPU workbench (auto-spawns daedalusd if not running)
./target/release/daedalus-ui --dir /path/to/your/repo --agent agy
# Supported presets: --agent agy | claude | codex | shell
```

To build with `libghostty-vt` (when installed via `pkg-config` or `GHOSTTY_VT_DIR`):
```bash
GHOSTTY_VT_DIR=/path/to/ghostty-prefix cargo build --release -p daedalusd --features ghostty-vt
```

---

## Keybindings (`daedalus-ui`)

| Shortcut | Action |
|----------|--------|
| `Alt+1` / `Alt+2` / `Alt+3` | Focus `Agent` / `Service` / `Runner` pane |
| `Ctrl+Shift+H/J/K/L` | Directional focus across panes |
| `Ctrl+Shift+Z` | Toggle pane zoom |
| `Ctrl+Shift+B` | Toggle left sidebar |
| `Ctrl+Shift+P` | Open/close Agent preset picker (`agy`, `claude`, `codex`, `shell`) |
| `Ctrl+Shift+W` | Open/close New Git Worktree modal |
| `Ctrl+Shift+N` | Create a new workspace in the current directory |
| `Ctrl+Shift+E` | Forward latest `Runner` failure trace to `Agent` (bracketed paste) |
| `Ctrl+Shift+C` / `V` | Copy selection / Bracketed-paste clipboard |
| `Shift+PageUp` / `PageDown` / `End` | Scrollback navigation |

---

## CLI & External Status Hook (`daedalus-ctl` / `events.sock`)

```bash
# Check daemon status
daedalus-ctl ping

# Discover or create an isolated git worktree workspace
daedalus-ctl worktree list --dir .
daedalus-ctl worktree create feat/billing --dir .

# Publish an external status badge via CLI
daedalus-ctl status publish --source phrourion --dir . --level warn --label "3 dirty"

# Or pipe 1-line NDJSON directly to $XDG_RUNTIME_DIR/daedalus/events.sock
printf '{"source":"cassandra","root_dir":"%s","level":"error","label":"retry 3/5"}\n' "$PWD" \
  | socat - UNIX-CONNECT:"${XDG_RUNTIME_DIR}/daedalus/events.sock"
```

---

## Development

```bash
git config core.hooksPath .githooks
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## License

MIT
