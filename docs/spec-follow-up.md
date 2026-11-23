# Initial specification follow-up

## Baseline

Reviewed against the 0.0.6 feature branch. These are proposed patch-sized milestones. Each can ship
independently after its acceptance checks pass.

| Original requirement | Implementation evidence | Assessment |
|---|---|---|
| Persistent daemon, PTYs, local IPC, grid deltas | `crates/daedalusd/src/{server,pane_actor,supervisor}.rs`, `crates/daedalus-proto/src/{codec,transport}.rs` | Implemented. Pane processes survive UI reconnects. Daemon restarts restore workspace metadata. |
| Agent, Service and Runner panes, focus, zoom, clipboard, scrollback, terminal styles | `crates/daedalus-ui/src/{layout,input,selection}.rs` and `renderer/` | Implemented using the built-in terminal engine. |
| Worktree creation, port allocation, Compose and environment isolation | `crates/daedalusd/src/{worktree,isolation,registry}.rs` | Implemented. Preserve collision and environment-remapping tests. |
| Command lifecycle, failure badge, forward-to-agent, external status socket | `crates/daedalusd/src/{shell_integration,external_hook}.rs`, `crates/daedalus-vt/src/semantic.rs`, UI bridge and renderer | Implemented with the built-in engine. Preserve behavior when adding the native backend. |
| Linux process-tree teardown and optional Compose cleanup | `supervisor.rs` sends TERM, waits up to three seconds, then KILL. The registry closes panes and supports Compose teardown. | Per-pane systemd scopes and a process-group fallback differ from the proposed single workspace scope. Verify descendant cleanup under both modes. |
| Project identity, runtime discovery, and project/worktree sidebar | `manifest.rs` reads explicit project configuration, `isolation.rs` finds Compose files, sidebar cards display workspaces and runtime status. | General project discovery and grouping require work. |
| Native Ghostty terminal state | `crates/daedalus-vt/src/ghostty/mod.rs` delegates to `BuiltinVt`. | Bindings scaffold only. |
| Agent selection | Current supported presets are agy, Claude, Codex and shell. | Preserve these four. The original spec also mentioned OpenCode as an example. |

## Proposed implementation order

1. **Project discovery and sidebar grouping.** Resolve the primary repository and linked worktrees
   in `worktree.rs`, identify Compose, Cargo, npm, Composer, Python and Make manifests in
   `manifest.rs`, and expose project identity and detected runtimes through additive, defaulted
   protocol fields. Preserve explicit manifest settings and shell fallback. Display workspaces under
   their owning repository with existing branch, port and runner status. Test two independent
   repositories with the same basename, linked worktrees, mixed manifests, a directory without Git,
   and reading old persisted state. Opening a project must never execute a discovered manifest as a
   side effect of discovery.
2. **Native terminal backend.** Pin a tested upstream Ghostty C ABI revision and compile a minimal
   ownership/feed/resize probe using the headers in `crates/daedalus-vt/build.rs`. Map terminal
   grid, dirty rows, cursor, cell attributes, scrollback, resize/reflow and search to the existing
   `VtEngine` interface. Drive OSC handling once per byte stream. Run identical transcript fixtures
   against both backends, covering Unicode, styles, alternate screen, split OSC sequences and runner
   failure extraction. Add native-feature CI with the pinned library before documenting native
   support or changing the default backend.
3. **Lifecycle acceptance.** Exercise closing a workspace with multiple shell children, a
   TERM-resistant grandchild, and a second workspace that must stay alive. Run the test under a real
   user systemd manager and under the process-group fallback. In a Docker-enabled job, close one
   workspace with and without Compose teardown and check its isolated sibling's containers and
   ports. If survivors are observed, fix the ownership/teardown path and retain the reproducer.
4. **Release acceptance.** Start the packaged UI, create two worktrees, run a failing test, forward
   its trace to the selected agent and reconnect to the same panes. Verify agy, Claude and Codex
   selection plus shell fallback. Record Linux Wayland and X11 results and the exact backend tested.
   Keep the fixed three-pane layout and local-first architecture.

## Other commitments from the retired roadmap

Track these independently of the original Linux MVP: Intel macOS packaging and Homebrew
distribution, Windows package-manager manifests, native platform process/port APIs where
command-line fallbacks remain, and extension schemas for custom sidebar widgets and status
providers. SSH attachment and manifest actions shipped in the 0.0.6 feature series. Evaluate mTLS
separately if a direct authenticated transport is needed.
