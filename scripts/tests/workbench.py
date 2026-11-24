"""Drive the Linux workbench under Xvfb with a disposable daemon and package manager."""

import os
import shutil
import subprocess
import sys
import time
from contextlib import contextmanager
from pathlib import Path
from tempfile import TemporaryDirectory

from Xlib import XK, X, display, protocol
from Xlib.ext import xtest


def until(description, check):
    deadline = time.monotonic() + 25
    while time.monotonic() < deadline:
        result = check()
        if result:
            return result
        time.sleep(0.05)
    raise AssertionError(f"Timed out waiting for {description}")


@contextmanager
def running(binary, args, project, env):
    with (project.parent / f"{binary.name}.log").open("w+") as log:
        process = subprocess.Popen(
            [binary, *args], cwd=project, env=env, stdout=log, stderr=subprocess.STDOUT
        )
        try:
            yield process
        except BaseException:
            log.seek(0)
            print(log.read(), file=sys.stderr)
            raise
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()


class Workbench:
    def __init__(self, screen, process):
        self.screen = screen

        def visible_window():
            if process.poll() is not None:
                raise AssertionError(
                    f"Workbench exited during startup: {process.returncode}"
                )
            for window in screen.screen().root.query_tree().children:
                if (
                    window.get_wm_name() == "Daedalus Workbench"
                    and window.get_attributes().map_state == X.IsViewable
                ):
                    return window
            return None

        self.window = until("mapped workbench", visible_window)
        self.window.set_input_focus(X.RevertToParent, X.CurrentTime)
        screen.sync()

    def key(self, *names):
        keys = [self.screen.keysym_to_keycode(XK.string_to_keysym(n)) for n in names]
        for key in keys:
            xtest.fake_input(self.screen, X.KeyPress, key)
        for key in reversed(keys):
            xtest.fake_input(self.screen, X.KeyRelease, key)
        self.screen.sync()

    def shortcut(self, name):
        self.key("Control_L", "Shift_L", name)

    def click(self, x, y):
        xtest.fake_input(self.screen, X.MotionNotify, x=x, y=y)
        for event in (X.ButtonPress, X.ButtonRelease):
            xtest.fake_input(self.screen, event, 1)
        self.screen.sync()

    def pixel(self, x, y):
        return self.window.get_image(x, y, 1, 1, X.ZPixmap, 0xFFFFFFFF).data[:3]

    def wait_dialog(self, position, visible=True):
        # Sample the focused border after redraw on the Xvfb TrueColor visual.
        accent = bytes.fromhex("b2e589")
        until("dialog transition", lambda: (self.pixel(*position) == accent) == visible)

    def close(self, process):
        event = protocol.event.ClientMessage(
            window=self.window,
            client_type=self.screen.intern_atom("WM_PROTOCOLS"),
            data=(
                32,
                [self.screen.intern_atom("WM_DELETE_WINDOW"), X.CurrentTime, 0, 0, 0],
            ),
        )
        self.window.send_event(event)
        self.screen.flush()
        assert process.wait(timeout=15) == 0


def fixture(scratch):
    project = scratch / "orbit"
    project.mkdir()
    for name in ("config", "runtime", "state", "bin"):
        (scratch / name).mkdir(mode=0o700)
    for name in ("sh", "touch", "cat", "mkdir", "chmod", "env", "git", "hostname"):
        (scratch / "bin" / name).symlink_to(shutil.which(name))
    npm = scratch / "bin/npm"
    npm.write_text(
        """#!/bin/sh
set -eu
case "$*" in
    'install -g @openai/codex')
        cat > "$PATH/codex" <<'AGENT'
#!/bin/sh
touch agent-started
exec cat
AGENT
        chmod +x "$PATH/codex" ;;
    ci) mkdir node_modules ;;
    test) echo ran >> test-runs ;;
    'run dev') touch server-started ;;
    *) exit 3 ;;
esac
"""
    )
    npm.chmod(0o755)
    (project / "package.json").write_text('{"scripts":{"dev":"demo","test":"fixture"}}')
    (project / "package-lock.json").write_text("{}")
    (project / ".daedalus.toml").write_text('[agent]\ncommand = ["codex"]\n')
    env = dict(
        os.environ,
        PATH=str(scratch / "bin"),
        HOME=str(scratch),
        XDG_CONFIG_HOME=str(scratch / "config"),
        XDG_RUNTIME_DIR=str(scratch / "runtime"),
        XDG_STATE_HOME=str(scratch / "state"),
        DAEDALUS_SOCKET=str(scratch / "daemon.sock"),
        WINIT_X11_SCALE_FACTOR="1",
    )
    for name in ("WAYLAND_DISPLAY", "DAEDALUS_CONFIG", "DAEDALUS_AUTH_TOKEN"):
        env.pop(name, None)
    return project, env


def exercise(binaries, scratch, screen):
    project, env = fixture(scratch)

    def cli(*args):
        return subprocess.run(
            [binaries / "daedalus-ctl", *args],
            env=env,
            capture_output=True,
            text=True,
            timeout=5,
        )

    args = [
        "--runtime-dir",
        scratch / "daemon",
        "--state-file",
        scratch / "state.json",
        "--events-socket",
        scratch / "events.sock",
    ]
    # The UI suggests the daemon host's home as the folder, not its own working dir.
    with running(binaries / "daedalusd", args, project, dict(env, HOME=str(project))):
        until("daemon", lambda: cli("ping").returncode == 0)
        project_dialog = (460, 332)
        review_dialog = (370, 200)
        recent_dialog = (460, 292)
        with running(
            binaries / "daedalus-ui", ["--no-auto-spawn"], project, env
        ) as client:
            ui = Workbench(screen, client)
            ui.wait_dialog(project_dialog)
            ui.key("Escape")
            ui.wait_dialog(project_dialog, visible=False)
            assert "panes=" not in cli("workspace", "list").stdout
            ui.shortcut("n")
            ui.wait_dialog(project_dialog)
            ui.key("Return")
            ui.wait_dialog(review_dialog)
            ui.click(752, 524)  # Install here
            until(
                "tool installation", lambda: os.access(scratch / "bin/codex", os.X_OK)
            )
            # The install button disappears after the daemon rechecks the executable.
            until(
                "install action removed",
                lambda: ui.pixel(752, 518) == bytes.fromhex("281e18"),
            )
            ui.click(710, 588)  # Install project dependencies
            until("project dependencies", lambda: (project / "node_modules").is_dir())
            until(
                "dependency action removed",
                lambda: ui.pixel(710, 588) == bytes.fromhex("281e18"),
            )
            ui.click(922, 660)  # Open workspace
            until("agent launched", lambda: (project / "agent-started").exists())
            assert not (project / "server-started").exists()
            assert not (project / "test-runs").exists()
            ui.wait_dialog(review_dialog, visible=False)
            until(
                "active workspace",
                lambda: ui.pixel(1208, 67) == bytes.fromhex("b2e589"),
            )
            ui.shortcut("t")
            runs = project / "test-runs"
            until(
                "on-demand test", lambda: runs.exists() and runs.read_text() == "ran\n"
            )
            ui.close(client)

        # Reconnect to the daemon and run the saved assignment again.
        with running(
            binaries / "daedalus-ui", ["--no-auto-spawn"], project, env
        ) as client:
            ui = Workbench(screen, client)
            until(
                "workspace card", lambda: ui.pixel(1208, 67) == bytes.fromhex("b2e589")
            )
            ui.shortcut("t")
            until("restored test assignment", lambda: runs.read_text() == "ran\nran\n")
            listing = cli("workspace", "list")
            assert listing.returncode == 0 and listing.stdout.count("panes=") == 1, (
                listing
            )
            ui.close(client)

        # Close the workspace, then reopen the project from the recent list.
        ids = [
            line.split()[0]
            for line in cli("workspace", "list").stdout.splitlines()
            if line.strip()
        ]
        assert len(ids) == 1, ids
        assert cli("workspace", "close", ids[0]).returncode == 0
        assert "panes=" not in cli("workspace", "list").stdout
        assert "orbit" in cli("workspace", "recent").stdout
        (project / "agent-started").unlink()
        with running(
            binaries / "daedalus-ui", ["--no-auto-spawn"], project, env
        ) as client:
            ui = Workbench(screen, client)
            ui.wait_dialog(recent_dialog)
            ui.click(720, 349)  # The recent project
            until("agent relaunched", lambda: (project / "agent-started").exists())
            assert not (project / "server-started").exists()
            ui.wait_dialog(recent_dialog, visible=False)
            until(
                "reopened workspace",
                lambda: ui.pixel(1208, 67) == bytes.fromhex("b2e589"),
            )
            assert cli("workspace", "list").stdout.count("panes=") == 1
            ui.close(client)


def enable_accessibility():
    """Turn AT-SPI on so accesskit requests the tree, as a screen reader would."""

    def call(method, *args):
        return subprocess.run(
            [
                "dbus-send",
                "--session",
                "--print-reply",
                "--dest=org.a11y.Bus",
                "/org/a11y/bus",
                f"org.freedesktop.DBus.Properties.{method}",
                "string:org.a11y.Status",
                "string:IsEnabled",
                *args,
            ],
            check=True,
            capture_output=True,
            text=True,
            timeout=10,
        ).stdout

    call("Set", "variant:boolean:true")
    reply = call("Get")
    assert "boolean true" in reply, f"AT-SPI did not enable: {reply}"


if __name__ == "__main__":
    if "DBUS_SESSION_BUS_ADDRESS" not in os.environ:
        sys.exit("run under dbus-run-session so the accessibility tree is exercised")
    enable_accessibility()
    screen = display.Display()
    try:
        with TemporaryDirectory(prefix="daedalus-ui-") as folder:
            exercise(Path(sys.argv[1]).resolve(), Path(folder), screen)
    finally:
        screen.close()
