# Initial specification follow-up

## Baseline

The original review covered the 0.0.6 feature branch. Status notes below identify work shipped
since that baseline and preserve acceptance scenarios for later release checks.

| Original requirement | Implementation evidence | Assessment |
|---|---|---|
| Persistent daemon, PTYs, local IPC, grid deltas | `crates/daedalusd/src/{server,pane_actor,supervisor}.rs`, `crates/daedalus-proto/src/{codec,transport}.rs` | Implemented. Pane processes survive UI reconnects. Daemon restarts restore workspace metadata. |
| Agent, Service and Runner panes, focus, zoom, clipboard, scrollback, terminal styles | `crates/daedalus-ui/src/{layout,input,selection}.rs` and `renderer/` | Implemented using the built-in terminal engine. |
| Worktree creation, port allocation, Compose and environment isolation | `crates/daedalusd/src/{worktree,isolation,registry}.rs` | Implemented. Preserve collision and environment-remapping tests. |
| Command lifecycle, failure badge, forward-to-agent, external status socket | `crates/daedalusd/src/{shell_integration,external_hook}.rs`, `crates/daedalus-vt/src/semantic.rs`, UI bridge and renderer | Implemented with the built-in engine. Preserve behavior when adding the native backend. |
| Linux process-tree teardown and optional Compose cleanup | `supervisor.rs` sends TERM, waits up to three seconds, then KILL. The registry closes panes and supports Compose teardown. | Per-pane systemd scopes and a process-group fallback differ from the proposed single workspace scope. Verify descendant cleanup under both modes. |
| Project identity, runtime discovery, and project/worktree sidebar | `manifest.rs` reads explicit project configuration, `isolation.rs` finds Compose files, sidebar cards display workspaces and runtime status. | Shipped. The daemon reports detected tools and the sidebar groups linked worktrees. |
| Native Ghostty terminal state | [Native VT workflow](../.github/workflows/ghostty-vt.yml) builds pinned Ghostty and runs native transcript tests. | Shipped as an optional backend. Built-in VT is the default. |
| Agent selection | Current supported presets are agy, Claude, Codex and shell. | Preserve these four. The original spec also mentioned OpenCode as an example. |

## Milestones and acceptance

1. **Project discovery and sidebar grouping — shipped.** The daemon detects project tools
   without launching their commands. The sidebar groups workspaces by repository and keeps
   names distinct when repositories share a basename.
2. **Native terminal backend — shipped as an option.** Native VT CI builds a pinned Ghostty
   C ABI and tests the feature-gated backend. Keep built-in VT as the default and record the
   backend used for each acceptance check.
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
