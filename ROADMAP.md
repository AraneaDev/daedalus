# Daedalus Roadmap

> **Versioning policy:** While Daedalus is in early pre-release (`v0.0.x`), all milestones ship as patch increments (`v0.0.2`, `v0.0.3`, …) with no minor or major bumps until the core cross-platform architecture stabilizes.

---

## v0.0.1 — Linux MVP (Shipped)

- **Headless Daemon (`daedalusd`):** Persistent Unix-socket daemon (`$XDG_RUNTIME_DIR/daedalus/daedalusd.sock`), per-pane Tokio actors with 60Hz coalesced `GridDelta` streaming, `systemd-run --user --scope` + `setsid`/`killpg` orphan-free teardown.
- **Dual-Backend VT Core (`daedalus-vt`):** Pure-Rust `BuiltinVt` default backend + feature-gated `--features ghostty-vt` `libghostty-vt` 1.0 FFI scaffold, non-destructive `bash` & `zsh` OSC 7 / OSC 133 hooks, and plain-text `FailureTrace` slicing.
- **Native GPU Workbench (`daedalus-ui`):** Wayland/X11 `winit` + `wgpu 30` + `glyphon 0.12` two-pass renderer, fixed 3-pane semantic layout (`55%` Agent / `22.5%` Service / `22.5%` Runner + zoom + sidebar), `agy`/`claude`/`codex`/`shell` Agent preset picker (`Ctrl+Shift+P`), scrollback, drag/word selection + clipboard (`arboard`), auto-spawning `DaemonBridge`, and `Ctrl+Shift+E` sanitized bracketed-paste failure forwarding.
- **Worktree Isolation, Runtime Health & External Hooks:** One-click/hotkey git worktrees (`Ctrl+Shift+W`), conflict-free port-offset allocation + Compose v2 `!override` `.daedalus/compose.override.yml` + `.env` remapping, `bollard` Docker health + `/proc/net/tcp{,6}` local port monitor, and NDJSON `events.sock` + `daedalus-ctl status publish`.

---

## v0.0.2 — Daily-Driver Workbench (Shipped)

- **Native `libghostty-vt` FFI Scaffold:** Feature-gated `--features ghostty-vt` `GhosttyVt` FFI scaffold remains ready for when `libghostty-vt` 1.0 headers are available (default `BuiltinVt` pure-Rust backend handles cell grid, scrollback reflow, search, and OSC 7/133 extraction).
- **Project Manifest & Persistence:**
  - Per-repo `.daedalus.toml` (default Agent preset, Service startup command, Runner command, custom port/env templates).
  - Persist workspace/project metadata across `daedalusd` restarts (`~/.local/state/daedalus/state.json`).
- **Terminal & UI Polish:**
  - Scrollback search (`Ctrl+Shift+F`), clickable `file:line` & URLs, runtime font-size zoom (`Ctrl+`/`-`/`0`), user config & themes (`~/.config/daedalus/config.toml`).
  - `fish` shell integration (OSC 7 + OSC 133).
  - Horizontal scrolling/left-truncation for long branch names in the worktree modal & full multi-character IME input.
- **Release Packaging:** Automated Linux release archives (`.tar.gz` / `.deb`) attached to GitHub Releases.

---

## v0.0.3 — Deeper Agent & Worktree Workflows (Shipped)

- **First-Class `agy`, `claude`, and `codex` Enhancements:**
  - Agent state detection (thinking, waiting for approval, idle) surfaced in sidebar & pane headers.
  - Structured failure-context packets (command, exit code, git diff summary, trimmed output) and optional MCP/hook bridge.
- **Worktree Lifecycle Controls:**
  - One-click worktree cleanup from the sidebar (`git worktree remove` + optional `docker compose down`).
  - Multi-worktree status overview and quick branch switching.
- **External Tool Ecosystem:** Built-in presets and richer badge rendering for `phrourion`, `cassandra`, and custom user scripts.

---

## v0.0.4 — macOS Native Support (Apple Silicon & Intel) (Shipped)

Because `daedalus-ui` already uses `winit` + `wgpu` (Metal backend) + `glyphon` + `arboard`, most of the macOS port lives in `daedalusd`:

- **Platform Abstractions (`daedalusd`):** Extract `PlatformSupervisor` and `PlatformPortScanner` traits to isolate Linux-only code (`systemd-run`, `/proc`).
- **macOS Supervision & Ports:**
  - Process isolation via `setsid()` + process-group `killpg` and optional `launchd` user agent for persistent daemon startup.
  - Local bound-port discovery via `libproc` (`PROC_PIDLISTFDS` + `PROC_PIDFDSOCKETINFO`) instead of `/proc/net/tcp`.
  - Automatic Docker socket discovery for Docker Desktop, OrbStack, and Colima in `bollard`.
- **macOS UI & Packaging:** `Cmd` keybinding mapping (`Cmd+Shift+...`, `Cmd+C/V`), native titlebar/HiDPI polish, `.app` bundle, and Homebrew formula.

---

## v0.0.5 — Windows Native Support (ConPTY, Named Pipes & WSL2 Bridge) (Shipped)

- **Cross-Platform IPC (`daedalus-proto`):** Abstract transport over Unix Domain Sockets (Linux/macOS/Windows `AF_UNIX`) and Windows Named Pipes (`\\.\pipe\daedalus`).
- **Windows Daemon (`daedalusd`):**
  - PTY allocation via `portable-pty` ConPTY backend.
  - Orphan-free process-tree teardown via Windows **Job Objects** (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`), matching Linux cgroups v2 semantics.
  - PowerShell (`pwsh`) and Git Bash OSC 7 / OSC 133 shell integration injection.
  - Local bound-port discovery via Windows IP Helper API (`GetExtendedTcpTable`).
- **WSL2 Bridge Mode:** Native Windows `daedalus-ui` (DX12 / Vulkan `wgpu`) attaching directly to `daedalusd` running inside WSL2 for Linux-native Docker/worktree workflows with a native Windows GPU frontend.
- **Windows Packaging:** `.msi` installer, `winget`, and Scoop manifests.

---

## v0.0.6+ — Remote Daemons & Extensibility

- **Remote `daedalusd` Attachment:** Connect a local `daedalus-ui` to a remote Linux `daedalusd` over SSH port/socket forwarding or mTLS, keeping all PTYs, worktrees, and Docker stacks on the remote devbox.
- **Hook & Action SDK:** Lightweight schema for custom sidebar widgets, status providers, and cross-pane actions.
