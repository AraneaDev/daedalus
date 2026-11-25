#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
use super::command::run_command_stdout_with_timeout;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
#[cfg(all(unix, any(test, not(any(target_os = "macos", target_os = "linux")))))]
use std::process::Command;
#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
use std::time::Duration;

/// Parses `ps -axo pid=,ppid=` output into `(pid, ppid)` pairs, ignoring headers and malformed lines.
pub fn parse_ps_ppid_table(output: &str) -> Vec<(u32, u32)> {
    let mut edges = Vec::new();
    for line in output.lines() {
        let mut cols = line.split_whitespace();
        let (Some(pid_str), Some(ppid_str), None) = (cols.next(), cols.next(), cols.next()) else {
            continue;
        };
        let (Ok(pid), Ok(ppid)) = (pid_str.parse::<u32>(), ppid_str.parse::<u32>()) else {
            continue;
        };
        if pid == 0 {
            continue;
        }
        edges.push((pid, ppid));
    }
    edges
}

/// Computes all descendant PIDs of `root` from `(pid, ppid)` edges.
pub fn descendants_from_ppid_edges(root: u32, edges: &[(u32, u32)]) -> Vec<u32> {
    if root == 0 {
        return Vec::new();
    }

    let mut children_by_ppid: HashMap<u32, Vec<u32>> = HashMap::new();
    for &(pid, ppid) in edges {
        if pid > 0 {
            children_by_ppid.entry(ppid).or_default().push(pid);
        }
    }
    for children in children_by_ppid.values_mut() {
        children.sort_unstable();
        children.dedup();
    }

    let mut visited = HashSet::from([root]);
    let mut queue = VecDeque::from([root]);
    let mut descendants = Vec::new();

    while let Some(pid) = queue.pop_front() {
        if let Some(children) = children_by_ppid.get(&pid) {
            for &child in children {
                if visited.insert(child) {
                    descendants.push(child);
                    queue.push_back(child);
                }
            }
        }
    }

    descendants
}

/// Collects all live descendant PIDs of `root_pid`.
#[cfg(target_os = "linux")]
pub fn collect_descendant_pids(root_pid: u32) -> Vec<u32> {
    if root_pid == 0 {
        return Vec::new();
    }
    let Ok(stats) = proc_stats() else {
        return Vec::new();
    };
    let edges: Vec<_> = stats
        .filter_map(|stat| parse_proc_stat_pid_ppid(&stat))
        .collect();
    descendants_from_ppid_edges(root_pid, &edges)
}

/// Collects descendants on Unix systems without a native port discovery backend.
#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
pub fn collect_descendant_pids(root_pid: u32) -> Vec<u32> {
    if root_pid == 0 {
        return Vec::new();
    }
    let mut cmd = Command::new("ps");
    cmd.args(["-axo", "pid=,ppid="]);
    let Some(output) = run_command_stdout_with_timeout(&mut cmd, Duration::from_secs(2), false)
    else {
        return Vec::new();
    };
    let edges = parse_ps_ppid_table(&output);
    descendants_from_ppid_edges(root_pid, &edges)
}

/// Uses the same native process inventory as workspace port discovery.
#[cfg(any(windows, target_os = "macos"))]
pub fn collect_descendant_pids(root_pid: u32) -> Vec<u32> {
    let mut pids = descendant_pids_including_roots(&[root_pid]);
    pids.remove(&root_pid);
    let mut pids: Vec<_> = pids.into_iter().collect();
    pids.sort_unstable();
    pids
}

#[cfg(target_os = "linux")]
pub(crate) fn parse_proc_stat(stat: &str) -> Option<(char, i32)> {
    let after_comm = stat.rfind(')')?;
    let mut parts = stat[after_comm + 1..].split_whitespace();
    let state = parts.next()?.chars().next()?;
    let _ppid = parts.next()?;
    let pgrp: i32 = parts.next()?.parse().ok()?;
    Some((state, pgrp))
}

#[cfg(target_os = "linux")]
pub(crate) fn pgid_has_non_zombie_proc(pgid: i32) -> bool {
    let Ok(mut stats) = proc_stats() else {
        return true;
    };
    stats.any(|stat| {
        parse_proc_stat(&stat)
            .is_some_and(|(state, pgrp)| pgrp == pgid && !matches!(state, 'Z' | 'X'))
    })
}

#[cfg(target_os = "linux")]
fn proc_stats() -> std::io::Result<impl Iterator<Item = String>> {
    Ok(std::fs::read_dir("/proc")?.flatten().filter_map(|entry| {
        let name = entry.file_name();
        if name.to_str()?.bytes().all(|b| b.is_ascii_digit()) {
            std::fs::read_to_string(entry.path().join("stat")).ok()
        } else {
            None
        }
    }))
}

#[cfg(any(target_os = "linux", test))]
fn parse_proc_stat_pid_ppid(stat: &str) -> Option<(u32, u32)> {
    let first_paren = stat.find('(')?;
    let last_paren = stat.rfind(')')?;
    if last_paren <= first_paren {
        return None;
    }
    let pid: u32 = stat[..first_paren].trim().parse().ok()?;
    let mut parts = stat[last_paren + 1..].split_whitespace();
    let _state = parts.next()?;
    let ppid: u32 = parts.next()?.parse().ok()?;
    Some((pid, ppid))
}

/// Parses `ps -axo pid=,ppid=,comm=` output into `(pid, ppid, comm_basename)` tuples.
pub fn parse_ps_pid_comm_table(output: &str) -> Vec<(u32, u32, String)> {
    let mut rows = Vec::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some(idx1) = trimmed.find(char::is_whitespace) else {
            continue;
        };
        let Ok(pid) = trimmed[..idx1].parse::<u32>() else {
            continue;
        };
        if pid == 0 {
            continue;
        }
        let rest = trimmed[idx1..].trim_start();
        let Some(idx2) = rest.find(char::is_whitespace) else {
            continue;
        };
        let Ok(ppid) = rest[..idx2].parse::<u32>() else {
            continue;
        };
        let raw_comm = rest[idx2..].trim();
        if raw_comm.is_empty() {
            continue;
        }
        let comm = Path::new(raw_comm)
            .file_name()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(raw_comm)
            .to_string();
        rows.push((pid, ppid, comm));
    }
    rows
}

/// Walks `/proc/<pid>/stat` to collect `roots` and all live descendant PIDs.
#[cfg(target_os = "linux")]
pub fn descendant_pids_including_roots(roots: &[u32]) -> HashSet<u32> {
    if roots.is_empty() {
        return HashSet::new();
    }

    let mut children_by_ppid: HashMap<u32, Vec<u32>> = HashMap::new();
    if let Ok(stats) = proc_stats() {
        for (pid, ppid) in stats.filter_map(|stat| parse_proc_stat_pid_ppid(&stat)) {
            children_by_ppid.entry(ppid).or_default().push(pid);
        }
    }

    let mut collected = HashSet::new();
    let mut queue = VecDeque::new();
    for &root in roots {
        if root > 0 && collected.insert(root) {
            queue.push_back(root);
        }
    }

    while let Some(pid) = queue.pop_front() {
        if let Some(children) = children_by_ppid.get(&pid) {
            for &child in children {
                if collected.insert(child) {
                    queue.push_back(child);
                }
            }
        }
    }

    collected
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
pub(crate) fn descendant_pids_with_comms(roots: &[u32]) -> (HashSet<u32>, HashMap<u32, String>) {
    if roots.is_empty() {
        return (HashSet::new(), HashMap::new());
    }

    let mut cmd = std::process::Command::new("ps");
    cmd.args(["-axo", "pid=,ppid=,comm="]);
    let Some(output) = run_command_stdout_with_timeout(&mut cmd, Duration::from_secs(2), false)
    else {
        return (HashSet::new(), HashMap::new());
    };

    let mut children_by_ppid: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut comm_by_pid: HashMap<u32, String> = HashMap::new();
    for (pid, ppid, comm) in parse_ps_pid_comm_table(&output) {
        children_by_ppid.entry(ppid).or_default().push(pid);
        comm_by_pid.insert(pid, comm);
    }

    let mut collected = HashSet::new();
    let mut queue = VecDeque::new();
    for &root in roots {
        if root > 0 && collected.insert(root) {
            queue.push_back(root);
        }
    }

    while let Some(pid) = queue.pop_front() {
        if let Some(children) = children_by_ppid.get(&pid) {
            for &child in children {
                if collected.insert(child) {
                    queue.push_back(child);
                }
            }
        }
    }

    (collected, comm_by_pid)
}

/// Walks `ps` output to collect `roots` and all live descendant PIDs.
#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
pub fn descendant_pids_including_roots(roots: &[u32]) -> HashSet<u32> {
    descendant_pids_with_comms(roots).0
}

#[cfg(target_os = "macos")]
pub fn descendant_pids_including_roots(roots: &[u32]) -> HashSet<u32> {
    use libproc::processes::{ProcFilter, pids_by_type};

    let mut pending = roots.to_vec();
    let mut seen = HashSet::new();
    while let Some(pid) = pending.pop() {
        if pid != 0
            && seen.insert(pid)
            && let Ok(children) = pids_by_type(ProcFilter::ByParentProcess { ppid: pid })
        {
            pending.extend(children);
        }
    }
    seen
}

#[cfg(windows)]
pub fn descendant_pids_including_roots(roots: &[u32]) -> HashSet<u32> {
    use std::mem::size_of;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    let mut seen: HashSet<_> = roots.iter().copied().filter(|&pid| pid != 0).collect();
    if seen.is_empty() {
        return seen;
    }
    // SAFETY: TH32CS_SNAPPROCESS requests a snapshot of the process list; the PID is ignored.
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if raw == INVALID_HANDLE_VALUE {
        return seen;
    }
    // SAFETY: the successful call transferred ownership of one snapshot handle.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut children = HashMap::<u32, Vec<u32>>::new();
    // SAFETY: entry has the required size and the snapshot stays open for the enumeration.
    let mut found = unsafe { Process32FirstW(snapshot.as_raw_handle(), &mut entry) };
    while found != 0 {
        if entry.th32ProcessID != 0 {
            children
                .entry(entry.th32ParentProcessID)
                .or_default()
                .push(entry.th32ProcessID);
        }
        // SAFETY: same snapshot and initialized destination as Process32FirstW.
        found = unsafe { Process32NextW(snapshot.as_raw_handle(), &mut entry) };
    }
    let mut pending: Vec<_> = seen.iter().copied().collect();
    while let Some(pid) = pending.pop() {
        for child in children.remove(&pid).unwrap_or_default() {
            if seen.insert(child) {
                pending.push(child);
            }
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::process::Stdio;

    #[cfg(target_os = "linux")]
    #[test]
    fn parse_proc_stat_reads_state_and_group_after_the_last_paren() {
        assert_eq!(
            parse_proc_stat("4242 (odd) name (x)) S 1 777 777 0 -1"),
            Some(('S', 777))
        );
        assert_eq!(parse_proc_stat("4242 (agent) Z 1 9 9"), Some(('Z', 9)));
        assert_eq!(parse_proc_stat("4242 (agent) S 1 group"), None);
        assert_eq!(parse_proc_stat("4242 (agent) S"), None);
        assert_eq!(parse_proc_stat("no paren"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pgid_scan_finds_this_test_group_and_not_an_unused_one() {
        let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
        let (_, own_group) = parse_proc_stat(&stat).unwrap();
        assert!(pgid_has_non_zombie_proc(own_group));
        // Linux caps PIDs at 2^22, so no process group can have this id.
        assert!(!pgid_has_non_zombie_proc(i32::MAX));
    }

    #[test]
    fn parse_ps_ppid_table_ignores_headers_and_malformed_lines() {
        let sample = "\
  PID  PPID
    0     0
    1     0
  100     1
  205   100
bad line
  300   abc
  400
  500   205   extra
  600   205
";
        assert_eq!(
            parse_ps_ppid_table(sample),
            vec![(1, 0), (100, 1), (205, 100), (600, 205)]
        );
    }

    #[test]
    fn descendants_from_ppid_edges_collects_subtree_and_handles_cycles() {
        let edges = vec![
            (10, 1),
            (20, 10),
            (30, 10),
            (40, 20),
            (50, 40),
            (60, 1),
            // Self-loop and cycle should not loop infinitely or re-include root
            (10, 20),
            (70, 70),
        ];
        assert_eq!(
            descendants_from_ppid_edges(10, &edges),
            vec![20, 30, 40, 50]
        );
        assert!(descendants_from_ppid_edges(999, &edges).is_empty());
        assert!(descendants_from_ppid_edges(0, &edges).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn collect_descendant_pids_finds_live_child_process() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "sleep 5"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn child");

        let child_pid = child.id();
        let descendants = collect_descendant_pids(std::process::id());
        assert!(
            descendants.contains(&child_pid),
            "expected {child_pid} in {descendants:?}"
        );

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn parse_proc_stat_handles_spaces_and_parens_in_comm() {
        let stat = "4242 (my (weird) proc name) S 1001 4242 1001 0 -1 4194304 10 0 0 0";
        assert_eq!(parse_proc_stat_pid_ppid(stat), Some((4242, 1001)));
        assert_eq!(parse_proc_stat_pid_ppid("bad"), None);
    }
}
