pub mod agent_detector;
pub mod external_hook;
pub mod isolation;
mod journal;
pub mod manifest;
pub mod pane_actor;
pub mod persistence;
mod preview;
pub mod registry;
pub mod runtime_monitor;
pub mod server;
mod setup;
pub mod shell_integration;
pub mod supervisor;
pub mod workbench;
pub mod worktree;

/// Background probes run from a detached daemon, which has no console to inherit.
fn background_command(program: &str) -> std::process::Command {
    let command = std::process::Command::new(program);
    #[cfg(windows)]
    let command = {
        use std::os::windows::process::CommandExt;
        let mut command = command;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
        command
    };
    command
}
