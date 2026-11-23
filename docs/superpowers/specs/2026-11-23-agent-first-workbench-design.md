# Returning to a workspace

## Intent

Open the workspace you last used. Within one glance, answer what the agent did while you were away,
what the services are doing now, and whether you need to act. Keep other worktrees reachable in one
click. A cross-project overview is a later option for days spent supervising many agents.

## Composition and visual language

Keep the native renderer and real PTYs. Draw on Aranea Desktop’s obsidian field, mint selection,
violet filaments and restrained warm attention accents. Give the attributed agent excerpt the
strongest type on the return screen. Place silk artwork at the far edge of that handoff surface,
away from terminal output. Group service observations in a ruled list with a source and an explicit
state on each row. Use Inter or a system sans for the interface and JetBrains Mono or the user’s
terminal font for commands. Preserve accessible contrast, theme choice and keyboard focus. The
header exposes the current workspace selector, New workspace and Workspace settings on both desktop
and narrow screens. Selecting a workspace restores its active terminal and handoff. New workspace
collects the project folder, display name, agent and optional service/test commands. Settings opens
the same form prefilled for the selected workspace. Opening or saving setup must never execute a
command as a side effect. The HTML sketch keeps workspace changes in memory for the preview session.

Visual references:
[Aranea Desktop gallery](https://github.com/AraneaDev/aranea-desktop/tree/master/screenshots) and
[theme palette](https://github.com/AraneaDev/aranea-desktop/blob/master/colors.toml). The sketch
embeds a reduced image from Aranea’s night wallpaper with its license notice.

Fill the entire app viewport at every supported size. The visible terminal text and input surface
must occupy at least two thirds of the viewport area. Exclude tabs, navigation, headers, borders and
other chrome when measuring. The active terminal expands as the window grows. Keep the workspace
switcher in the header. The desktop handoff and service rail takes up to 21% of viewport width,
capped at 460 logical pixels, and scrolls independently. A long recap, expanded history or failed
run cannot shrink the terminal.

In narrow or short windows, give the full-width terminal about three quarters of viewport height and
put the handoff and services in the remaining scrollable space. Preserve keyboard focus, pane
shortcuts and zoom when switching layouts. Test both return scenarios, expanded history, pane
changes, at least 320 logical pixels of width, short laptop windows and ultrawide screens. Check the
rendered terminal area divided by the actual viewport area, with a minimum of two thirds.

## Two illustrative returns

| Quiet return | Needs attention |
| --- | --- |
| **Orbit** · feature/healthcheck | **Orbit** · feature/healthcheck |
| **Since you left** · Agent produced output. Open the agent transcript at the relevant output. Runner finished the configured test command, exit 0. | **Since you left** · Agent requested input where reported. Runner finished the configured test command, exit 1. |
| **Services** · API listening on 8080. Database healthy, from its container health check. | **Services** · Database unhealthy, from its container health check. Open service output. |
| **Last run** · Tests passed. Open runner output. | **Last run** · Tests failed. Inspect trace or send the captured runner failure to the current agent. |

These are fixture states. A failed service alone never enables forwarding a runner failure.

## Evidence and history

The current protocol supplies pane roles and agent states, command completions and exit codes, port
observations, container health, and workspace Git state. Current snapshots cannot reconstruct all
activity after a UI disconnect. Add a bounded per-workspace in-memory journal in the daemon with a
daemon epoch, monotonic sequence and timestamp. Record meaningful transitions once. Keep at most 256
entries per workspace, coalesce agent output bursts, and exclude raw keystrokes, environment
variables and transcript bodies. Retain only bounded display metadata. Continue serving terminals
through existing snapshot and scrollback APIs.

Maintain a local cursor per daemon endpoint, epoch and workspace for each UI profile. Freeze it on
loss of focus, workspace switch or disconnect. On return, show observations after the saved cursor
up to a fixed high-water mark. Advance the cursor after the recap has been shown in a focused
window, while keeping that recap visible for the visit. Fetching or background redraw alone never
acknowledges it. Live arrivals during review get a separate new-activity affordance. On first visit
show current state and begin tracking. If history expired, the daemon restarted, or the peer lacks
journal support, explain the gap and show available current state. A disconnected window labels its
retained observations as last known.

For unstructured agent output, show a verbatim excerpt labeled Agent terminal, limited to two
nonblank lines and 240 characters. Read visible plain text from retained rows referenced by the
latest output burst. Exclude echoed user input when identifiable and skip prompt-only rows. If
meaningful output cannot be distinguished or the rows have expired, show the output event and offer
to open the terminal. An excerpt describes what the terminal displayed. Its assertions stay
attributed to that source.

Attention counts represent distinct current conditions: an agent waiting or in error, an unhealthy
service, or the latest runner failure awaiting a successful run. Repeated observations of one
condition count once. Unread events and worktree dirtiness do not count. Unknown and stale
observations carry a freshness label. A container row without a known pane mapping can offer the
workspace-wide “View Service terminal” only when that pane exists. Its label must distinguish the
shared terminal from container-specific logs.

Provider-reported activity with a reliable source may quote a bounded agent update. Otherwise show
evidence such as agent output, observed command completion in the agent pane, or reported waiting
state. Link to retained output where feasible and identify when that excerpt has expired. Never
invent a narrative of edits. Workspace Git dirtiness belongs to the worktree. Runner commands get a
test-specific label only when identified as tests by explicit configuration. A listening port,
process exit, container state and health check each use their own wording. Missing telemetry
displays unknown or unavailable. An inferred approval signal should invite the user to check the
agent.

## Acceptance

Test a first visit, background focus and return, switching worktrees, closing and reopening the UI,
two independent UI profiles, replay overlapping live events, journal rollover, daemon restart and
old peers. Verify that a quiet return stays quiet and stale data cannot enable an action on a
different pane. Cover healthy, listening, unknown, unhealthy and disconnected service states. Verify
the existing trace forwarding path only acts on the intended failure.

Capture the real quiet and attention states at desktop and narrow widths. Check label contrast,
keyboard-only navigation, accessible names and focus, and announcements for important transitions
without repeated announcements for output bursts. A user should be able to identify the latest
observed agent activity and the service needing attention without reading raw output, then reach the
supporting terminal in one action.

## Decisions for review

The user selected an agent-first initial focus with situational workspace switching. The native
stack, bounded journal, layout, typography, palette and acknowledgement rules are proposed design
choices. Exact adaptive widths, theme token values and drawing primitives are implementation
decisions. This iteration covers the active workspace. Agent-provider transcript parsers, remote
model summarization and a dedicated fleet dashboard are outside its scope.

### Multiple agents and commands per workspace

Workspace settings hold an ordered list of named agents and an ordered list of named commands. Two
agents can use the same provider. Each entry has a stable identity so renaming a terminal preserves
its selection and output attribution. The terminal strip exposes every assigned entry by name and
scrolls horizontally when needed. Switching workspaces restores the selected terminal. Sending a
failure requires an agent destination within that workspace.

The native implementation should persist these definitions, migrate existing agent, service and
runner settings into entries, and associate terminal events with the originating entry. Saving
settings does not start a process. The interactive preview keeps assignments in memory and simulates
terminal selection and failure delivery.
