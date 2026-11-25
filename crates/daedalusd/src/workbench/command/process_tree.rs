use std::io;
use tokio::process::{Child, Command};

/// Own descendant cleanup while a captured command is running. Tokio separately
/// owns the direct child's exit status and reaping.
pub(super) struct ProcessTree(Option<platform::Tree>);

impl ProcessTree {
    pub(super) async fn spawn(command: &mut Command) -> io::Result<(Child, Self)> {
        platform::prepare(command);
        let mut child = command.spawn()?;
        match platform::Tree::attach(&child) {
            Ok(tree) => Ok((child, Self(Some(tree)))),
            Err(error) => {
                let _ = child.kill().await;
                Err(error)
            }
        }
    }

    pub(super) fn complete(mut self) {
        self.0.take();
    }
}

impl Drop for ProcessTree {
    fn drop(&mut self) {
        if let Some(tree) = &self.0 {
            tree.terminate();
        }
    }
}

#[cfg(unix)]
mod platform {
    use super::*;
    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid;

    pub(super) fn prepare(command: &mut Command) {
        command.process_group(0);
    }

    pub(super) struct Tree(Pid);

    impl Tree {
        pub(super) fn attach(child: &Child) -> io::Result<Self> {
            let pid = child
                .id()
                .ok_or_else(|| io::Error::other("Command did not start"))?;
            Ok(Self(Pid::from_raw(pid as i32)))
        }

        pub(super) fn terminate(&self) {
            let _ = killpg(self.0, Signal::SIGKILL);
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::ptr::null;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        CREATE_NO_WINDOW, CREATE_SUSPENDED, OpenThread, ResumeThread, THREAD_SUSPEND_RESUME,
    };

    pub(super) fn prepare(command: &mut Command) {
        // Attach before the child's first instruction so it cannot start
        // descendants outside the job.
        command.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
    }

    pub(super) struct Tree(OwnedHandle);

    impl Tree {
        pub(super) fn attach(child: &Child) -> io::Result<Self> {
            let pid = child
                .id()
                .ok_or_else(|| io::Error::other("Command did not start"))?;
            let process = child
                .raw_handle()
                .ok_or_else(|| io::Error::other("Command process handle is unavailable"))?;
            // SAFETY: Null arguments request an unnamed job with default security.
            let job = unsafe { CreateJobObjectW(null(), null()) };
            if job.is_null() {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: CreateJobObjectW returned a new valid owned handle.
            let job = unsafe { OwnedHandle::from_raw_handle(job) };
            // SAFETY: Both handles remain valid through the call.
            if unsafe { AssignProcessToJobObject(job.as_raw_handle(), process) } == 0 {
                return Err(io::Error::last_os_error());
            }
            resume_initial_thread(pid)?;
            Ok(Self(job))
        }

        pub(super) fn terminate(&self) {
            // SAFETY: The owned job handle remains valid through the call.
            unsafe { TerminateJobObject(self.0.as_raw_handle(), 1) };
        }
    }

    fn resume_initial_thread(pid: u32) -> io::Result<()> {
        // SAFETY: The thread snapshot flag ignores the second argument.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: CreateToolhelp32Snapshot returned a new owned handle.
        let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
        let mut entry = THREADENTRY32 {
            dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
            ..Default::default()
        };
        // SAFETY: The snapshot and sized entry are valid for this enumeration.
        let mut found = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) };
        while found != 0 {
            if entry.th32OwnerProcessID == pid {
                // SAFETY: The ID comes from the snapshot and the requested access is
                // limited to resuming the suspended thread.
                let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if thread.is_null() {
                    return Err(io::Error::last_os_error());
                }
                // SAFETY: OpenThread returned a valid owned handle.
                let thread = unsafe { OwnedHandle::from_raw_handle(thread) };
                // SAFETY: The thread handle remains valid through the call.
                if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
                    return Err(io::Error::last_os_error());
                }
                return Ok(());
            }
            // SAFETY: The snapshot and entry remain valid for this enumeration.
            found = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) };
        }
        Err(io::Error::other(
            "Command's suspended thread is unavailable",
        ))
    }
}
