#[cfg(any(target_os = "linux", all(test, unix)))]
use crate::process::inventory::descendant_pids_including_roots;
#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
use crate::process::inventory::descendant_pids_with_comms;
use daedalus_proto::{PortMapping, PortSource};
#[cfg(all(unix, not(target_os = "macos")))]
use std::collections::BTreeMap;
use std::collections::{BTreeSet, HashMap, HashSet};
#[cfg(target_os = "linux")]
use std::path::Path;
#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
use std::time::Duration;

/// Parses Linux `/proc/net/tcp` and `/proc/net/tcp6` contents and returns `(inode, listening_port)`
/// pairs for sockets in the `LISTEN` state (`st == "0A"`).
pub fn parse_proc_net_tcp(content: &str) -> Vec<(u64, u16)> {
    let mut out = Vec::new();
    for line in content.lines() {
        let mut cols = line.split_whitespace();
        let Some(_sl) = cols.next() else {
            continue;
        };
        let Some(local_addr) = cols.next() else {
            continue;
        };
        let Some(_rem_addr) = cols.next() else {
            continue;
        };
        let Some(state) = cols.next() else {
            continue;
        };
        if !state.eq_ignore_ascii_case("0A") {
            continue;
        }
        // Columns 4..=8: tx_queue:rx_queue, tr:tm->when, retrnsmt, uid, timeout
        let Some(inode_str) = cols.nth(5) else {
            continue;
        };
        let Some((_, port_hex)) = local_addr.rsplit_once(':') else {
            continue;
        };
        let Ok(port) = u16::from_str_radix(port_hex, 16) else {
            continue;
        };
        let Ok(inode) = inode_str.parse::<u64>() else {
            continue;
        };
        if inode == 0 {
            continue;
        }
        out.push((inode, port));
    }
    out
}

/// Parses `lsof -nP -a -p <pids> -iTCP -sTCP:LISTEN -F pcn` output into deduplicated
/// `(pid, port, cmd)` tuples.
pub fn parse_lsof_listen_output(output: &str) -> Vec<(u32, u16, Option<String>)> {
    let mut out: Vec<(u32, u16, Option<String>)> = Vec::new();
    let mut seen: HashMap<(u32, u16), usize> = HashMap::new();
    let mut current_pid: Option<u32> = None;
    let mut current_cmd: Option<String> = None;

    for raw_line in output.lines() {
        let line = raw_line.trim_end_matches('\r');
        let Some(prefix) = line.chars().next() else {
            continue;
        };
        let value = &line[prefix.len_utf8()..];
        match prefix {
            'p' => {
                current_pid = value.trim().parse::<u32>().ok().filter(|&p| p > 0);
                current_cmd = None;
            }
            'c' => {
                let cmd = value.trim();
                if !cmd.is_empty() {
                    current_cmd = Some(cmd.to_string());
                }
            }
            'n' => {
                let Some(pid) = current_pid else {
                    continue;
                };
                let addr = value.trim();
                let local = addr.split("->").next().unwrap_or(addr);
                let Some((_, port_str)) = local.rsplit_once(':') else {
                    continue;
                };
                let Ok(port) = port_str.parse::<u16>() else {
                    continue;
                };
                if port == 0 {
                    continue;
                }
                if let Some(&idx) = seen.get(&(pid, port)) {
                    if out[idx].2.is_none() && current_cmd.is_some() {
                        out[idx].2 = current_cmd.clone();
                    }
                } else {
                    let idx = out.len();
                    seen.insert((pid, port), idx);
                    out.push((pid, port, current_cmd.clone()));
                }
            }
            _ => {}
        }
    }

    out
}

/// Parses Windows `netstat -ano -p TCP` output and returns deduplicated [`PortMapping`] entries
/// for `LISTENING` sockets whose PID is present in `pid_filter`.
pub fn parse_windows_netstat_output(output: &str, pid_filter: &HashSet<u32>) -> Vec<PortMapping> {
    if pid_filter.is_empty() {
        return Vec::new();
    }

    let mut ports = BTreeSet::new();
    for raw_line in output.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let mut cols = line.split_whitespace();
        let (Some(proto), Some(local_addr), Some(_foreign_addr), Some(state), Some(pid_str), None) = (
            cols.next(),
            cols.next(),
            cols.next(),
            cols.next(),
            cols.next(),
            cols.next(),
        ) else {
            continue;
        };
        if !proto.eq_ignore_ascii_case("TCP") || !state.eq_ignore_ascii_case("LISTENING") {
            continue;
        }
        let Ok(pid) = pid_str.parse::<u32>() else {
            continue;
        };
        if pid == 0 || !pid_filter.contains(&pid) {
            continue;
        }
        let Some((_, port_str)) = local_addr.rsplit_once(':') else {
            continue;
        };
        let Ok(port) = port_str.parse::<u16>() else {
            continue;
        };
        if port > 0 {
            ports.insert(port);
        }
    }

    ports
        .into_iter()
        .map(|host_port| PortMapping {
            host_port,
            container_port: None,
            service: None,
            source: PortSource::LocalProcess,
        })
        .collect()
}

#[cfg(target_os = "macos")]
#[path = "macos_ports.rs"]
mod native;
#[cfg(windows)]
#[path = "windows_ports.rs"]
mod native;

#[cfg(any(target_os = "macos", windows))]
pub use native::discover_local_ports_for_roots;

#[cfg(target_os = "linux")]
fn parse_socket_inode(link_target: &Path) -> Option<u64> {
    let s = link_target.to_str()?;
    let inner = s.strip_prefix("socket:[")?.strip_suffix(']')?;
    inner.parse::<u64>().ok()
}

#[cfg(target_os = "linux")]
fn read_process_comm(pid: u32) -> Option<String> {
    let raw = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Discovers local TCP listening ports bound by processes in the subtrees rooted at `root_pids`.
#[cfg(target_os = "linux")]
pub fn discover_local_ports_for_roots(root_pids: &[u32]) -> Vec<PortMapping> {
    if root_pids.is_empty() {
        return Vec::new();
    }

    let mut inode_to_port = HashMap::new();
    for path in ["/proc/net/tcp", "/proc/net/tcp6"] {
        if let Ok(content) = std::fs::read_to_string(path) {
            for (inode, port) in parse_proc_net_tcp(&content) {
                inode_to_port.entry(inode).or_insert(port);
            }
        }
    }
    if inode_to_port.is_empty() {
        return Vec::new();
    }

    let mut pids: Vec<u32> = descendant_pids_including_roots(root_pids)
        .into_iter()
        .collect();
    pids.sort_unstable();

    let mut by_port: BTreeMap<u16, Option<String>> = BTreeMap::new();
    for pid in pids {
        let Ok(fd_entries) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
            continue;
        };
        let mut comm_cache: Option<Option<String>> = None;
        for entry in fd_entries.flatten() {
            let Ok(target) = std::fs::read_link(entry.path()) else {
                continue;
            };
            let Some(inode) = parse_socket_inode(&target) else {
                continue;
            };
            let Some(&host_port) = inode_to_port.get(&inode) else {
                continue;
            };
            let service = comm_cache
                .get_or_insert_with(|| read_process_comm(pid))
                .clone();
            match by_port.get_mut(&host_port) {
                Some(existing) if existing.is_none() && service.is_some() => {
                    *existing = service;
                }
                Some(_) => {}
                None => {
                    by_port.insert(host_port, service);
                }
            }
        }
    }

    by_port
        .into_iter()
        .map(|(host_port, service)| PortMapping {
            host_port,
            container_port: None,
            service,
            source: PortSource::LocalProcess,
        })
        .collect()
}

/// Discovers local TCP listening ports bound by processes in the subtrees rooted at `root_pids`
/// using `ps` and `lsof`.
#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
pub fn discover_local_ports_for_roots(root_pids: &[u32]) -> Vec<PortMapping> {
    if root_pids.is_empty() {
        return Vec::new();
    }

    let (pids_set, comm_by_pid) = descendant_pids_with_comms(root_pids);
    if pids_set.is_empty() {
        return Vec::new();
    }

    let mut pids: Vec<u32> = pids_set.into_iter().collect();
    pids.sort_unstable();
    let pid_csv = pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");

    let mut cmd = std::process::Command::new("lsof");
    cmd.args([
        "-nP",
        "-a",
        "-p",
        &pid_csv,
        "-iTCP",
        "-sTCP:LISTEN",
        "-F",
        "pcn",
    ]);
    let Some(output) = crate::process::command::run_command_stdout_with_timeout(
        &mut cmd,
        Duration::from_secs(2),
        true,
    ) else {
        return Vec::new();
    };

    let mut by_port: BTreeMap<u16, Option<String>> = BTreeMap::new();
    for (pid, host_port, lsof_cmd) in parse_lsof_listen_output(&output) {
        let service = comm_by_pid.get(&pid).cloned().or(lsof_cmd);
        match by_port.get_mut(&host_port) {
            Some(existing) if existing.is_none() && service.is_some() => {
                *existing = service;
            }
            Some(_) => {}
            None => {
                by_port.insert(host_port, service);
            }
        }
    }

    by_port
        .into_iter()
        .map(|(host_port, service)| PortMapping {
            host_port,
            container_port: None,
            service,
            source: PortSource::LocalProcess,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::inventory::parse_ps_pid_comm_table;
    use std::net::TcpListener;
    #[cfg(unix)]
    use std::process::{Command, Stdio};

    #[test]
    fn parse_proc_net_tcp_filters_listen_ipv4_and_ipv6() {
        let ipv4_sample = "\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:0BB8 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 12345 1 0000000000000000 100 0 0 10 0
   1: 00000000:1F90 00000000:0000 0a 00000000:00000000 00:00000000 00000000  1000        0 23456 1 0000000000000000 100 0 0 10 0
   2: 0100007F:0BB8 0100007F:D431 01 00000000:00000000 00:00000000 00000000  1000        0 34567 1 0000000000000000 100 0 0 10 0
   3: 0100007F:1538 0100007F:9999 06 00000000:00000000 00:00000000 00000000     0        0 0 1 0000000000000000
   4: 0100007F:0050 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 0 1 0000000000000000
   malformed line
";
        assert_eq!(
            parse_proc_net_tcp(ipv4_sample),
            vec![(12345, 3000), (23456, 8080)]
        );

        let ipv6_sample = "\
  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000000000000000000000000000:1388 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 55555 1 0000000000000000 100 0 0 10 0
   1: 00000000000000000000000001000000:AD39 00000000000000000000000000000000:0000 01 00000000:00000000 00:00000000 00000000  1000        0 66666 1 0000000000000000 100 0 0 10 0
";
        assert_eq!(parse_proc_net_tcp(ipv6_sample), vec![(55555, 5000)]);
    }

    #[test]
    fn discover_local_ports_detects_live_bound_tcp_listener() {
        assert!(discover_local_ports_for_roots(&[]).is_empty());

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let bound_port = listener.local_addr().unwrap().port();

        let ports = discover_local_ports_for_roots(&[std::process::id()]);
        let found = ports
            .iter()
            .find(|p| p.host_port == bound_port)
            .unwrap_or_else(|| panic!("expected port {bound_port} in {ports:?}"));
        assert_eq!(found.container_port, None);
        assert_eq!(found.source, PortSource::LocalProcess);
        #[cfg(unix)]
        assert!(found.service.is_some());

        drop(listener);
    }

    #[cfg(unix)]
    #[test]
    fn collect_descendant_pids_includes_root_and_live_children() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "sleep 5"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn child");

        let self_pid = std::process::id();
        let child_pid = child.id();
        let pids = descendant_pids_including_roots(&[self_pid]);
        assert!(pids.contains(&self_pid));
        assert!(pids.contains(&child_pid));

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn parse_ps_pid_comm_table_extracts_basenames_and_skips_headers() {
        let sample = "\
  PID  PPID COMM
    0     0 kernel_task
  100     1 /sbin/launchd
 1234   100 /opt/homebrew/bin/node
 1235   100 /Applications/Visual Studio Code.app/Contents/MacOS/Electron
 1236   100 python3
malformed
 1237   bad foo
 1238   100
";
        assert_eq!(
            parse_ps_pid_comm_table(sample),
            vec![
                (100, 1, "launchd".to_string()),
                (1234, 100, "node".to_string()),
                (1235, 100, "Electron".to_string()),
                (1236, 100, "python3".to_string()),
            ]
        );
    }

    #[test]
    fn parse_lsof_listen_output_handles_ipv4_wildcard_ipv6_multiple_pids_and_dedup() {
        let sample = "\
p1234
cnode
f12
n127.0.0.1:3000
f13
n[::1]:3000
f14
n*:8080
p5678
cpostgres
n[::]:5432
n0.0.0.0:5432
p9999
n/tmp/ignore.sock
n[::1]
n127.0.0.1:0
n127.0.0.1:9000
";
        assert_eq!(
            parse_lsof_listen_output(sample),
            vec![
                (1234, 3000, Some("node".to_string())),
                (1234, 8080, Some("node".to_string())),
                (5678, 5432, Some("postgres".to_string())),
                (9999, 9000, None),
            ]
        );
    }

    #[test]
    fn parse_windows_netstat_output_filters_listening_tcp_and_pids() {
        let sample = "\
Active Connections

  Proto  Local Address          Foreign Address        State           PID
  TCP    0.0.0.0:5432           0.0.0.0:0              LISTENING       1234
  TCP    127.0.0.1:3000         0.0.0.0:0              LISTENING       1234
  TCP    [::1]:8080             [::]:0                 LISTENING       5678
  TCP    [::]:8080              [::]:0                 LISTENING       5678
  TCP    127.0.0.1:3000         127.0.0.1:51234        ESTABLISHED     1234
  TCP    0.0.0.0:9999           0.0.0.0:0              LISTENING       9999
  UDP    0.0.0.0:5353           *:*                                    1234
  TCP    0.0.0.0:0              0.0.0.0:0              LISTENING       1234
  malformed line
";
        let pid_filter = HashSet::from([1234, 5678]);
        assert_eq!(
            parse_windows_netstat_output(sample, &pid_filter),
            vec![
                PortMapping {
                    host_port: 3000,
                    container_port: None,
                    service: None,
                    source: PortSource::LocalProcess,
                },
                PortMapping {
                    host_port: 5432,
                    container_port: None,
                    service: None,
                    source: PortSource::LocalProcess,
                },
                PortMapping {
                    host_port: 8080,
                    container_port: None,
                    service: None,
                    source: PortSource::LocalProcess,
                },
            ]
        );
        assert!(parse_windows_netstat_output(sample, &HashSet::new()).is_empty());
    }
}
