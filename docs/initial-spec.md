# Specification: Project Daedalus (Semantic Runtime & Agent Workbench)

## 1. Executive Summary & Core Thesis

**Daedalus** is a local-first developer workbench that orchestrates **coding agents, Docker Compose stacks, and test/build runners** inside isolated git worktree workspaces, powered by embedded `libghostty-vt` (1.0 C ABI) terminal emulation.

Unlike generic terminal multiplexers (`tmux`, `zellij`) or basic multi-pane agent wrappers, Daedalus treats a workspace as a **typed runtime graph** rather than a collection of opaque shells:

1. **Persistent Headless Daemon (`daedalusd`):** Owns PTYs, process groups, and Docker lifecycles so running agents, containers, and watchers survive UI crashes or restarts.
2. **Worktree-Scoped Port & Environment Isolation:** Running `main` alongside `feat/billing` automatically allocates non-colliding port blocks, isolated Compose project names, and matching `.env` overrides across both containers and agent PTYs.
3. **Semantic VT Introspection (`libghostty-vt`):** Uses OSC 7 (working directory) and OSC 133 (shell prompt/command lifecycle + exit code) to detect test failures, surface status badges in the UI, and forward failure traces directly to the active agent pane.

---

## 2. Target Tech Stack (Linux / Wayland First)

* **Language:** Rust (2024 edition) — single workspace (`Cargo.toml` workspace with `daedalusd` daemon + `daedalus-ui` client + `daedalus-proto` shared IPC types).
* **VT Emulation:** `libghostty-vt` (1.0 C ABI via `libghostty-rs` or thin `bindgen` FFI wrapper) for ANSI parsing, cell grid state, scrollback reflow, and OSC 7 / OSC 133 extraction.
* **Daemon & Process Supervision (`daedalusd`):**
* `tokio` (async runtime) + Unix Domain Socket IPC (`interprocess` or `tokio::net::UnixListener` with MessagePack/JSON-RPC).
* `portable-pty` for PTY allocation.
* Linux process isolation via `setsid()` + cgroups v2 (`systemd-run --user --scope` or direct cgroupfs) to guarantee zero orphan child processes on teardown.
* `bollard` for Docker Engine API (container status, health checks, port bindings, Compose project grouping).


* **Frontend UI (`daedalus-ui`):**
* `wgpu` + `winit` (native Wayland/X11 GPU rendering, zero Electron/WebView overhead) with `glyphon` / `cosmic-text` for subpixel/ligature terminal font rendering, **or** `gpui` / `iced` with a custom `wgpu` shader canvas for the terminal grid.



---

## 3. System Architecture

```text
┌─────────────────────────────────────────────────────────────┐
│                        daedalus-ui                          │
│  ┌──────────────┐  ┌──────────────────────────────────────┐ │
│  │ Project/     │  │ Workspace Canvas (Typed Slots)       │ │
│  │ Worktree     │  │ ┌────────────────┐ ┌───────────────┐ │ │
│  │ Sidebar      │  │ │  Agent Slot    │ │ Service Slot  │ │ │
│  │              │  │ │  (Claude/Codex)│ │ (Docker/Dev)  │ │ │
│  │ - main       │  │ │                │ ├───────────────┤ │ │
│  │ - feat/auth  │  │ │                │ │ Runner Slot   │ │ │
│  │   ● 2 ports  │  │ │                │ │ (Tests/Build) │ │ │
│  │   ✗ 1 failed │  │ └────────────────┘ └───────────────┘ │ │
│  └──────────────┘  └──────────────────────────────────────┘ │
└──────────────────────────────▲──────────────────────────────┘
                               │ Unix Socket (Grid Deltas + Control RPC)
┌──────────────────────────────▼──────────────────────────────┐
│                         daedalusd                           │
│  ┌─────────────────┐  ┌────────────────┐  ┌──────────────┐  │
│  │ Session & PTY   │  │ libghostty-vt  │  │ Port & Env   │  │
│  │ Supervisor      ├─►│ State Machines ├─►│ Allocator    │  │
│  │ (cgroups/setsid)│  │ (Grid/OSC 133) │  │ (Offset Pool)│  │
│  └─────────────────┘  └────────────────┘  └──────┬───────┘  │
│                                                  │          │
│                                           ┌──────▼───────┐  │
│                                           │ Docker/Compose│ │
│                                           │ Controller   │  │
│                                           └──────────────┘  │
└─────────────────────────────────────────────────────────────┘

```

### 3.1 Core Domain Model

* **Project:** Root git repository path + auto-detected runtime manifest (`docker-compose.yml`, `package.json`, `Cargo.toml`, `composer.json`, `pyproject.toml`, `Makefile`).
* **Workspace (Worktree Instance):** Bound to a specific directory (primary repo or `git worktree`), assigned:
* `slot_index: u16` (0 for primary, 1..N for concurrent worktrees).
* `port_offset: u16` (`slot_index * 100`, e.g., `3000 -> 3100`, `5432 -> 5532`).
* `compose_project_name: String` (e.g., `myrepo-wt1`).
* `injected_env: HashMap<String, String>` (applied uniformly to all PTYs and Docker Compose invocations in that workspace).


* **Pane Slot (Typed):**
1. `Agent`: Interactive CLI agent (`claude`, `codex`, `opencode`, or user shell). Tracks busy/idle state via PTY activity + OSC 133 prompt state.
2. `Service`: Long-running dev server (`npm run dev`, `cargo run`) or `docker compose up` watcher. Tracks bound TCP ports via `/proc/net/tcp` + `bollard`.
3. `Runner`: Test/build watcher or on-demand test runner (`vitest`, `cargo test`, `phpunit`, `pytest`). Captures command exit codes and output ranges via OSC 133.



---

## 4. Key Subsystems & Behavior

### 4.1 Worktree Port & Environment Isolation Engine

When a Workspace is created or opened:

1. Scan `docker-compose.yml` (if present) and `.env` / `.env.example` for port mappings (`HOST:CONTAINER`) and `*_PORT` / `*_URL` variables.
2. Allocate a deterministic `port_offset` (verifying via `TcpListener::bind` that target host ports are free; increment block if occupied).
3. Generate an ephemeral override file (`.daedalus/compose.override.yml`) remapping exposed host ports and setting `COMPOSE_PROJECT_NAME`.
4. Export remapped `PORT`, `DATABASE_URL`, `REDIS_URL`, etc. into the environment of every PTY spawned inside that workspace so the **Agent**, **Dev Server**, and **Test Runner** all talk to the isolated database/services automatically.

### 4.2 `libghostty-vt` Integration & OSC 133 Hooks

* Each PTY output stream feeds directly into a dedicated `ghostty_vt` terminal instance inside `daedalusd`.
* **Shell Integration Injection:** When spawning interactive shells (`zsh`, `bash`, `fish`), `daedalusd` injects a lightweight init script (`ZDOTDIR` / `--rcfile` / `XDG_DATA_DIRS`) that emits:
* `OSC 7`: Current working directory updates.
* `OSC 133;A` (prompt start), `OSC 133;B` (command start), `OSC 133;C` (command executed), `OSC 133;D;<exit_code>` (command finished).


* **Cross-Pane Semantic Actions:**
* When a `Runner` pane receives `OSC 133;D;<non-zero>`, `daedalusd` slices the grid lines between `133;C` and `133;D`, strips ANSI formatting to plain text, stores it as `last_failure_trace`, and flags the workspace status badge in `daedalus-ui`.
* Triggering **"Send Failure to Agent"** (via UI button or keybinding `Ctrl+Shift+E`) formats the command, exit code, and trimmed output trace and writes it into the `Agent` PTY stdin (bracketed paste mode `\x1b[200~ ... \x1b[201~`).



### 4.3 Clean Lifecycle & Teardown

* Every spawned process runs inside its own process group (`setsid`) and user cgroup scope (`daedalus-<workspace-id>.scope`).
* Closing a workspace sends `SIGTERM` to the cgroup, waits 3 seconds, issues `SIGKILL` to the cgroup (guaranteeing zero orphaned Vite/Node/Cargo/PHP workers), and optionally runs `docker compose down` for that workspace's `COMPOSE_PROJECT_NAME`.

---

## 5. MVP Scope & Phased Build Plan

### Phase 1: Daemon & `libghostty-vt` Core (`daedalusd`)

* [ ] Set up Cargo workspace (`daedalusd`, `daedalus-proto`, `daedalus-ui`).
* [ ] Create Rust FFI bindings to `libghostty-vt` 1.0 C headers (`ghostty/vt.h`), wrapping terminal creation, `feed_bytes()`, resize, cursor state, and dirty-row grid snapshots.
* [ ] Implement PTY manager with `portable-pty`, cgroup/process-group cleanup, and Unix socket IPC server.
* [ ] Add shell integration injection for `bash` and `zsh` emitting OSC 7 and OSC 133 markers; verify exit-code capture and command output slicing.

### Phase 2: Native GPU Terminal UI (`daedalus-ui`)

* [ ] Build Wayland/X11 window with `winit` + `wgpu` rendering the `libghostty-vt` cell grid (foreground/background RGB colors, bold/italic/underline, cursor, selection + clipboard, scrollback).
* [ ] Implement fixed 3-pane semantic layout per workspace: **Left (55%): Agent PTY**, **Right-Top (22.5%): Service/Docker PTY**, **Right-Bottom (22.5%): Test Runner PTY**, plus keyboard focus navigation and pane zoom toggle.
* [ ] Build left sidebar listing Projects, Git Worktrees, live bound ports, and Runner pass/fail status indicators.

### Phase 3: Worktree Isolation & Cross-Pane Workflows

* [ ] Implement Git worktree discovery and one-click worktree creation (`git worktree add`).
* [ ] Build port-offset allocator + `.daedalus/compose.override.yml` generator + Docker health polling via `bollard`.
* [ ] Implement **"Send Failure to Agent"** (`Ctrl+Shift+E`) using OSC 133 command-range extraction.
* [ ] Add optional Unix socket hook to ingest status events from external tools (e.g., `phrourion` git status, `cassandra` retry alerts).

---

## 6. Explicit Non-Goals (For MVP)

* Do **not** build a custom shell, plugin scripting language, or multi-machine SSH multiplexer in v1.
* Do **not** build arbitrary recursive split-tree layouts in v1—stick to the opinionated 3-slot semantic layout (with pane zoom) so cross-pane actions remain predictable.
* Do **not** depend on Electron, Tauri, or browser DOM rendering.
