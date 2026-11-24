//! The Linux coverage job provides Xvfb, software Vulkan and Python Xlib.
#![cfg(target_os = "linux")]

use std::path::Path;
use std::process::Command;

#[test]
#[ignore = "run through scripts/check-test-quality.sh with the Linux desktop dependencies"]
fn first_launch_install_and_reconnect() {
    let ui = Path::new(env!("CARGO_BIN_EXE_daedalus-ui"));
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/tests/workbench.py");
    let python =
        std::env::var_os("DAEDALUS_TEST_PYTHON").unwrap_or_else(|| "/usr/bin/python3".into());
    let status = Command::new("xvfb-run")
        .args(["-a", "-s", "-screen 0 1440x900x24 -nolisten tcp"])
        .arg(python)
        .arg(script)
        .arg(ui.parent().unwrap())
        .status()
        .expect("launch the Linux desktop test under Xvfb");
    assert!(status.success(), "desktop test exited with {status}");
}
