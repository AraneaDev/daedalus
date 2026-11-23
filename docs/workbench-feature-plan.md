# Workbench feature implementation plan

Build eight independently verifiable features on the agent-first workbench. The daemon owns project
metadata, terminal lifetimes and port observations. Clients render that state. Packages and terminal
backends have separate verification requirements.

| Step | Change | Acceptance check |
| --- | --- | --- |
| 1 | Wire the focused tab and a keyboard shortcut to the daemon's ClosePane RPC. Remove the session after acknowledgment. | Close a focused pane, retain its siblings, move focus, then close the last pane and reconnect. |
| 2 | Resolve repository identity on the daemon and group sidebar entries by repository. | Main checkout and linked worktree share a group. Independent repositories with identical names and non-Git folders remain distinguishable. |
| 3 | Discover tools from project files and expose them alongside explicit settings. | A mixed project reports multiple tools without running any project commands. Older metadata remains readable. |
| 4 | Define manifest declarations for named status providers and their status channels. Reuse the existing publish and clear paths. | Registered channels receive updates. Existing integrations can still publish a plain badge. |
| 5 | Let a sidebar widget reference status channels and render workspace-specific rows. | Build and test results update independently. Missing or cleared results have a defined display. Widget rows and workspace selection stay aligned while scrolling. |
| 6 | Use operating-system process and socket APIs for macOS and Windows. | Attribute a child process's TCP listener to its workspace while excluding sibling listeners; exercise IPv4 and IPv6. |
| 7 | Package all three binaries on an Intel macOS runner and verify their architectures. | Install the package, launch the UI and reconnect to the same daemon workspace. Keep artifacts distinct from Apple Silicon packages. |
| 8 | Pin a Ghostty C ABI and use its terminal state for grid reads, input, resize, history and search. | Compare representative transcripts with the built-in backend and run a native-feature CI job against the pin. |

Verification includes focused regression tests and the locked workspace suite. Native-platform
execution results must identify the platform used. OpenCode and mutual TLS are deferred. Homebrew
and WinGet were skipped.
