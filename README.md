# Daedalus

Daedalus is a native workbench for running a coding agent, a development service, and tests side by
side. Each Git worktree has its own panes, ports, and Compose project. A persistent daemon owns the
processes, so you can close the window and reconnect later.

## Install

Download the installer for your machine from
[GitHub Releases](https://github.com/AraneaDev/daedalus/releases/latest). Native installers start
with the 0.0.7 series. Earlier releases provide portable archives.

| Platform | Asset | Installation |
| --- | --- | --- |
| Ubuntu 24.04 or compatible Debian-based Linux, x86-64 | `.deb` | Run `sudo apt install ./daedalus-<version>-amd64.deb`. Launch **Daedalus** from the applications menu. |
| Arch Linux, x86-64 (v0.0.9 onward) | `.pkg.tar.zst` | Run `sudo pacman -U ./daedalus-<version>-1-x86_64.pkg.tar.zst`. Launch **Daedalus** from the applications menu. |
| macOS, Apple Silicon | `.pkg` | Open the package, follow Installer, then open **Daedalus** from Applications. CLI tools are linked in `/usr/local/bin`. |
| Windows, x64 | `.msi` | Run the installer, then open **Daedalus** from Start. Open a new terminal to use the CLI. |

Packages include the UI, daemon and control CLI. The macOS and Windows installers are unsigned. Each
asset has a sibling SHA-256 checksum. Compare the downloaded file with its checksum using
`sha256sum` on Linux, `shasum -a 256` on macOS, or `Get-FileHash` in PowerShell.

## First workspace

From a terminal in a repository, run:

```sh
daedalus-ui --dir . --agent shell
```

The UI starts the local daemon automatically. The left pane hosts the agent, the upper-right pane
hosts a service, and the lower-right pane runs tests. Use the shell preset to try the workbench
immediately. To use Claude, Codex or agy, install the corresponding CLI and choose its preset with
the agent picker. Docker and Compose are needed only for container workflows.

## Keybindings (`daedalus-ui`)

| Shortcut | Action |
|----------|--------|
| `Alt+1` / `Alt+2` / `Alt+3` | Focus `Agent` / `Service` / `Runner` pane |
| `Ctrl+Shift+H/J/K/L` | Directional focus across panes |
| `Ctrl+Shift+Z` | Toggle pane zoom |
| `Ctrl+Shift+B` | Toggle left sidebar |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | Switch to the next / previous workspace |
| `Ctrl+Shift+P` | Open/close Agent preset picker (`agy`, `claude`, `codex`, `shell`) |
| `Ctrl+Shift+W` | Open/close New Git Worktree modal |
| `Ctrl+Shift+A` | Open workspace action palette |
| `Ctrl+Shift+N` | Create a new workspace in the current directory |
| `Ctrl+Shift+E` | Forward latest `Runner` failure trace to `Agent` (bracketed paste) |
| `Ctrl+Shift+C` / `V` | Copy selection / Bracketed-paste clipboard |
| `Ctrl+Shift+F` | Search the focused pane. Enter advances, Shift+Enter goes back, and Esc closes search. |
| `Shift+PageUp` / `PageDown` / `End` | Scrollback navigation |

---

In narrow windows, the panes stack vertically. Click **Workspaces** to show or hide the sidebar. For
long lists, scroll over the sidebar or use its page arrows.

On macOS, use Cmd for the workbench shortcuts shown with Ctrl+Shift.

The native accessibility tree exposes the active workspace, focused pane, connection status, and
open dialog or search context. In **New Git Worktree**, type the branch name and activate **Create**
or **Cancel** with the pointer, keyboard, or an assistive tool. Create becomes available when a
branch name is present and the daemon is connected.

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

## Remote workspaces and custom actions

Start `daedalusd` on the remote host, then pass its Unix socket path to either client. The example
assumes the SSH alias `devbox` and remote UID 1000. OpenSSH must be installed on the client and
socket forwarding enabled on the server.

```sh
daedalus-ui --socket ssh://devbox/run/user/1000/daedalus/daedalusd.sock
daedalus-ctl --socket ssh://devbox/run/user/1000/daedalus/daedalusd.sock ping
```

To require a bearer token, set `DAEDALUS_AUTH_TOKEN` in the daemon environment and supply the same
value in the client environment.

Define actions in the workspace root's `.daedalus.toml`. For example, this action submits the
workspace test command to the runner pane.

```toml
[[actions]]
id = "test"
label = "Run workspace tests"
target = "runner"
command = "cargo test --workspace"
```

Open the action palette with **Ctrl+Shift+A** (**Cmd+Shift+A** on macOS), select an action, and
press Enter. From the CLI, use `daedalus-ctl action list --workspace <uuid>` or
`daedalus-ctl action run test --workspace <uuid>`. When connecting remotely, use the workspace UUID
reported by the remote daemon.

## Build from source

Use stable Rust with the 2024 edition. On Ubuntu, install
`pkg-config libxkbcommon-dev libwayland-dev libxcb1-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev`.
macOS builds use the Xcode command-line tools. Windows builds use the MSVC Rust toolchain and Visual
Studio C++ build tools.

```sh
cargo build --release --workspace --locked
```

Run the binaries from `target/release`. The default terminal backend is implemented in Rust. The
experimental Ghostty feature generates C bindings and currently delegates terminal operations to the
built-in engine.

## Development

CI checks the default Rust backend on Linux, macOS and Windows. From a checkout, run:

```sh
git config core.hooksPath .githooks
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --locked --no-deps
cargo build --workspace --locked
```

Linux CI also checks Markdown structure and local links, Python lint and formatting, packaging
scripts, workflow definitions and the RustSec advisory database. To run the same checks, install
ShellCheck, then run:

```sh
python3 -m venv /tmp/daedalus-quality
. /tmp/daedalus-quality/bin/activate
pip install -r scripts/quality-tools.txt
scripts/check-sources.sh
cargo install cargo-audit --version 0.23.0 --locked
python3 scripts/check_audit.py
```

The audit fails on vulnerabilities, yanked dependencies and new advisory warnings. Five transitive
warnings from font rendering and Linux accessibility are printed on every run. Their exact versions
and advisory IDs are recorded in `scripts/check_audit.py` and need review at each version change.

To measure workspace coverage and test whether the suite detects changes to frame encoding,
decoding and terminal erasure, run:

```sh
rustup component add llvm-tools-preview
cargo install cargo-llvm-cov --version 0.9.0 --locked
cargo install cargo-mutants --version 27.1.0 --locked
scripts/check-test-quality.sh
```

The coverage floors are 79% of lines, 80% of functions and 80% of regions across the workspace. The
mutation gate fails on surviving or timed-out mutations. Mutation results are written under
`target/quality/`. Native Ghostty checks require the external Ghostty C library and headers.

The [specification follow-up](docs/spec-follow-up.md) records the remaining implementation work and
its acceptance tests.

## License

MIT
