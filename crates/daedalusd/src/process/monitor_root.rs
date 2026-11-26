use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Debug)]
pub(crate) struct MonitorRoot {
    pid: u32,
    live: Arc<AtomicBool>,
    start_time: Option<u128>,
}

impl MonitorRoot {
    pub(crate) fn new(pid: u32) -> Self {
        Self {
            pid,
            live: Arc::new(AtomicBool::new(true)),
            start_time: process_start_time(pid),
        }
    }

    pub(crate) fn retire(&self) {
        self.live.store(false, Ordering::Release);
    }

    pub(crate) fn live_pid(&self) -> Option<u32> {
        self.live.load(Ordering::Acquire).then_some(self.pid)
    }

    /// Called on a blocking worker before and after sampling the process tree.
    pub(crate) fn is_current(&self) -> bool {
        if cfg!(any(target_os = "linux", target_os = "macos", windows)) {
            return self.live_pid().is_some()
                && self.start_time.is_some()
                && self.start_time == process_start_time(self.pid);
        }
        self.live_pid().is_some()
    }
}

#[cfg(target_os = "linux")]
fn process_start_time(pid: u32) -> Option<u128> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, fields) = stat.rsplit_once(')')?;
    // The state is field 3. The process start time is field 22.
    fields.split_whitespace().nth(19)?.parse().ok()
}

#[cfg(target_os = "macos")]
fn process_start_time(pid: u32) -> Option<u128> {
    use libproc::{bsd_info::BSDInfo, proc_pid::pidinfo};

    let info = pidinfo::<BSDInfo>(i32::try_from(pid).ok()?, 0).ok()?;
    Some((u128::from(info.pbi_start_tvsec) << 64) | u128::from(info.pbi_start_tvusec))
}

#[cfg(windows)]
fn process_start_time(pid: u32) -> Option<u128> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, FILETIME},
        System::Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    };

    // SAFETY: A non-null handle from OpenProcess is owned here and closed after the query.
    // GetProcessTimes receives four distinct, writable FILETIME values that live through the call.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        let read = GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user);
        CloseHandle(process);
        (read != 0).then_some(
            (u128::from(creation.dwHighDateTime) << 32) | u128::from(creation.dwLowDateTime),
        )
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn process_start_time(_: u32) -> Option<u128> {
    None
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos", windows)))]
mod tests {
    use super::*;

    #[test]
    fn rejects_a_changed_start_time_and_retires_all_clones() {
        let root = MonitorRoot::new(std::process::id());
        let mut stale = root.clone();
        stale.start_time = Some(root.start_time.unwrap() + 1);
        assert!(root.is_current());
        assert!(!stale.is_current());
        root.retire();
        assert!(stale.live_pid().is_none());
    }
}
