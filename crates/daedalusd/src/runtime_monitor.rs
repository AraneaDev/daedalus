use crate::registry::Registry;
use crate::worktree;
use bollard::Docker;
use bollard::models::ContainerSummary;
use bollard::query_parameters::ListContainersOptions;
use daedalus_proto::{ContainerHealth, GitWorkspaceStatus, PortMapping, PortSource, WorkspaceId};
#[cfg(unix)]
use std::collections::VecDeque;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::debug;

const RUNTIME_POLL_INTERVAL: Duration = Duration::from_secs(3);
const DOCKER_POLL_TIMEOUT: Duration = Duration::from_secs(2);

/// Snapshot of a workspace's runtime monitoring inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceMonitorTarget {
    pub workspace_id: WorkspaceId,
    pub root_dir: PathBuf,
    pub compose_project_name: Option<String>,
    pub root_pids: Vec<u32>,
}

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

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn parse_proc_stat_pid_ppid(stat: &str) -> Option<(u32, u32)> {
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

/// Walks `/proc/<pid>/stat` to collect `roots` and all live descendant PIDs.
#[cfg(target_os = "linux")]
pub fn collect_descendant_pids(roots: &[u32]) -> HashSet<u32> {
    if roots.is_empty() {
        return HashSet::new();
    }

    let mut children_by_ppid: HashMap<u32, Vec<u32>> = HashMap::new();
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name_str) = name.to_str() else {
                continue;
            };
            if !name_str.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
                continue;
            };
            if let Some((pid, ppid)) = parse_proc_stat_pid_ppid(&stat) {
                children_by_ppid.entry(ppid).or_default().push(pid);
            }
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

#[cfg(all(unix, not(target_os = "linux")))]
fn descendant_pids_with_comms(roots: &[u32]) -> (HashSet<u32>, HashMap<u32, String>) {
    if roots.is_empty() {
        return (HashSet::new(), HashMap::new());
    }

    let mut cmd = std::process::Command::new("ps");
    cmd.args(["-axo", "pid=,ppid=,comm="]);
    let Some(output) =
        crate::supervisor::run_command_stdout_with_timeout(&mut cmd, Duration::from_secs(2), false)
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
#[cfg(all(unix, not(target_os = "linux")))]
pub fn collect_descendant_pids(roots: &[u32]) -> HashSet<u32> {
    descendant_pids_with_comms(roots).0
}

/// Returns non-zero root PIDs for `netstat`-based port matching on Windows.
#[cfg(windows)]
pub fn collect_descendant_pids(roots: &[u32]) -> HashSet<u32> {
    roots.iter().copied().filter(|&pid| pid > 0).collect()
}

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

    let mut pids: Vec<u32> = collect_descendant_pids(root_pids).into_iter().collect();
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
#[cfg(all(unix, not(target_os = "linux")))]
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
    let Some(output) =
        crate::supervisor::run_command_stdout_with_timeout(&mut cmd, Duration::from_secs(2), true)
    else {
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

/// Discovers local TCP listening ports bound by `root_pids` via `netstat -ano -p TCP`.
#[cfg(windows)]
pub fn discover_local_ports_for_roots(root_pids: &[u32]) -> Vec<PortMapping> {
    let pid_filter = collect_descendant_pids(root_pids);
    if pid_filter.is_empty() {
        return Vec::new();
    }

    let mut cmd = std::process::Command::new("netstat");
    cmd.args(["-ano", "-p", "TCP"]);
    let Some(output) =
        crate::supervisor::run_command_stdout_with_timeout(&mut cmd, Duration::from_secs(2), false)
    else {
        return Vec::new();
    };
    parse_windows_netstat_output(&output, &pid_filter)
}

/// Returns ordered candidate Docker Unix socket paths when `DOCKER_HOST` is unset.
pub fn discover_docker_socket_candidates(
    docker_host: Option<&str>,
    home: Option<&Path>,
) -> Vec<PathBuf> {
    if docker_host.is_some_and(|h| !h.is_empty()) {
        return Vec::new();
    }

    let mut candidates = Vec::with_capacity(5);
    if let Some(home_dir) = home.filter(|p| !p.as_os_str().is_empty()) {
        candidates.push(home_dir.join(".orbstack/run/docker.sock"));
        candidates.push(home_dir.join(".docker/run/docker.sock"));
        candidates.push(home_dir.join(".colima/default/docker.sock"));
        candidates.push(home_dir.join(".config/colima/default/docker.sock"));
    }
    candidates.push(PathBuf::from("/var/run/docker.sock"));
    candidates
}

/// Connects to Docker respecting `DOCKER_HOST` first, then probing OrbStack, Docker Desktop,
/// Colima, and `/var/run/docker.sock`, falling back to `Docker::connect_with_local_defaults()`.
pub fn connect_docker_client() -> Option<Docker> {
    #[cfg(unix)]
    {
        let docker_host = std::env::var("DOCKER_HOST").ok();
        if docker_host.as_deref().is_some_and(|h| !h.is_empty()) {
            return Docker::connect_with_local_defaults().ok();
        }

        let home = std::env::var_os("HOME").map(PathBuf::from);
        for path in discover_docker_socket_candidates(docker_host.as_deref(), home.as_deref()) {
            if path.exists()
                && std::os::unix::net::UnixStream::connect(&path).is_ok()
                && let Some(path_str) = path.to_str()
                && let Ok(client) =
                    Docker::connect_with_unix(path_str, 120, bollard::API_DEFAULT_VERSION)
            {
                return Some(client);
            }
        }
    }

    Docker::connect_with_local_defaults().ok()
}

fn extract_container_health(status: Option<&str>) -> Option<String> {
    let status = status?;
    if status.contains("(healthy)") {
        Some("healthy".to_string())
    } else if status.contains("(unhealthy)") {
        Some("unhealthy".to_string())
    } else if status.contains("(health: starting)") {
        Some("starting".to_string())
    } else {
        None
    }
}

fn map_container_summaries(
    summaries: Vec<ContainerSummary>,
) -> (Vec<ContainerHealth>, Vec<PortMapping>) {
    let mut containers = Vec::with_capacity(summaries.len());
    let mut port_map: BTreeMap<(u16, u16), Option<String>> = BTreeMap::new();

    for summary in summaries {
        let name = summary
            .names
            .as_deref()
            .and_then(|names| names.first())
            .map(|n| n.strip_prefix('/').unwrap_or(n).to_string())
            .or_else(|| summary.id.clone())
            .unwrap_or_else(|| "unknown".to_string());

        let service = summary
            .labels
            .as_ref()
            .and_then(|labels| labels.get("com.docker.compose.service"))
            .cloned();

        let state = summary
            .state
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".to_string());
        let health = extract_container_health(summary.status.as_deref());

        let mut container_ports = BTreeSet::new();
        if let Some(ports) = summary.ports {
            for port in ports {
                if let Some(host_port) = port.public_port {
                    container_ports.insert(host_port);
                    let key = (host_port, port.private_port);
                    match port_map.get_mut(&key) {
                        Some(existing) if existing.is_none() && service.is_some() => {
                            *existing = service.clone();
                        }
                        Some(_) => {}
                        None => {
                            port_map.insert(key, service.clone());
                        }
                    }
                }
            }
        }

        containers.push(ContainerHealth {
            name,
            service,
            state,
            health,
            ports: container_ports.into_iter().collect(),
        });
    }

    containers.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.service.cmp(&b.service)));

    let port_mappings = port_map
        .into_iter()
        .map(|((host_port, private_port), service)| PortMapping {
            host_port,
            container_port: Some(private_port),
            service,
            source: PortSource::Docker,
        })
        .collect();

    (containers, port_mappings)
}

/// Polls Docker for all containers belonging to `compose_project` and returns their health
/// summaries and published host port mappings.
pub async fn poll_workspace_docker(
    docker: &Docker,
    compose_project: &str,
) -> Result<(Vec<ContainerHealth>, Vec<PortMapping>), bollard::errors::Error> {
    let mut filters = HashMap::new();
    filters.insert(
        "label".to_string(),
        vec![format!("com.docker.compose.project={compose_project}")],
    );
    let options = ListContainersOptions {
        all: true,
        filters: Some(filters),
        ..Default::default()
    };
    let summaries = docker.list_containers(Some(options)).await?;
    Ok(map_container_summaries(summaries))
}

async fn wait_for_shutdown(shutdown: &mut watch::Receiver<bool>) {
    while !*shutdown.borrow() {
        if shutdown.changed().await.is_err() {
            break;
        }
    }
}

/// Spawns the 3-second background runtime monitor that discovers local bound ports
/// (via `spawn_blocking`) and Docker Compose container health, broadcasting changes
/// through [`Registry::update_workspace_runtime`].
pub fn spawn_runtime_monitor(
    registry: Arc<Registry>,
    mut shutdown: watch::Receiver<bool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(RUNTIME_POLL_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let mut docker_client: Option<Docker> = None;
        let mut docker_unavailable_logged = false;

        loop {
            tokio::select! {
                biased;
                _ = wait_for_shutdown(&mut shutdown) => break,
                _ = interval.tick() => {}
            }

            if *shutdown.borrow() {
                break;
            }

            let targets = registry.monitor_targets().await;
            if targets.is_empty() {
                continue;
            }

            let local_targets: Vec<(WorkspaceId, Vec<u32>, PathBuf)> = targets
                .iter()
                .map(|t| (t.workspace_id, t.root_pids.clone(), t.root_dir.clone()))
                .collect();

            let local_probe_fut = tokio::task::spawn_blocking(move || {
                local_targets
                    .into_iter()
                    .map(|(ws_id, roots, root_dir)| {
                        let ports = discover_local_ports_for_roots(&roots);
                        let (git_status, git_branch) =
                            match worktree::git_workspace_status_and_branch(&root_dir) {
                                Some((status, branch)) => (Some(status), branch),
                                None => (None, None),
                            };
                        (ws_id, (ports, git_status, git_branch))
                    })
                    .collect::<HashMap<
                        WorkspaceId,
                        (Vec<PortMapping>, Option<GitWorkspaceStatus>, Option<String>),
                    >>()
            });

            let mut local_by_ws = tokio::select! {
                biased;
                _ = wait_for_shutdown(&mut shutdown) => break,
                res = local_probe_fut => match res {
                    Ok(map) => map,
                    Err(e) => {
                        debug!(error = %e, "local port and git discovery task failed");
                        HashMap::new()
                    }
                }
            };

            if targets.iter().any(|t| t.compose_project_name.is_some()) && docker_client.is_none() {
                match connect_docker_client() {
                    Some(client) => docker_client = Some(client),
                    None => {
                        if !docker_unavailable_logged {
                            debug!("docker unavailable; skipping container health polling");
                            docker_unavailable_logged = true;
                        }
                    }
                }
            }

            for target in targets {
                if *shutdown.borrow() {
                    break;
                }

                let mut bound_ports = Vec::new();
                let mut containers = Vec::new();

                if let (Some(docker), Some(project)) = (
                    docker_client.as_ref(),
                    target.compose_project_name.as_deref(),
                ) {
                    let docker_res = tokio::select! {
                        biased;
                        _ = wait_for_shutdown(&mut shutdown) => break,
                        res = tokio::time::timeout(
                            DOCKER_POLL_TIMEOUT,
                            poll_workspace_docker(docker, project),
                        ) => res,
                    };

                    match docker_res {
                        Ok(Ok((ws_containers, ws_docker_ports))) => {
                            docker_unavailable_logged = false;
                            containers = ws_containers;
                            bound_ports.extend(ws_docker_ports);
                        }
                        Ok(Err(e)) => {
                            if !docker_unavailable_logged {
                                debug!(
                                    error = %e,
                                    project = %project,
                                    "docker unavailable; skipping container health polling"
                                );
                                docker_unavailable_logged = true;
                            }
                        }
                        Err(_) => {
                            if !docker_unavailable_logged {
                                debug!(
                                    project = %project,
                                    "docker poll timed out; skipping container health polling"
                                );
                                docker_unavailable_logged = true;
                            }
                        }
                    }
                }

                let (git_status, git_branch) = if let Some((local_ports, git_status, git_branch)) =
                    local_by_ws.remove(&target.workspace_id)
                {
                    bound_ports.extend(local_ports);
                    (git_status, git_branch)
                } else {
                    (None, None)
                };

                registry
                    .update_workspace_runtime(
                        target.workspace_id,
                        bound_ports,
                        containers,
                        git_status,
                        git_branch,
                    )
                    .await;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::{ContainerSummaryStateEnum, PortSummary, PortSummaryTypeEnum};
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
    fn parse_proc_stat_handles_spaces_and_parens_in_comm() {
        let stat = "4242 (my (weird) proc name) S 1001 4242 1001 0 -1 4194304 10 0 0 0";
        assert_eq!(parse_proc_stat_pid_ppid(stat), Some((4242, 1001)));
        assert_eq!(parse_proc_stat_pid_ppid("bad"), None);
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
        let pids = collect_descendant_pids(&[self_pid]);
        assert!(pids.contains(&self_pid));
        assert!(pids.contains(&child_pid));

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn map_container_summaries_extracts_health_and_dedups_ports() {
        let summaries = vec![
            ContainerSummary {
                id: Some("id-web".to_string()),
                names: Some(vec!["/proj-wt1-web-1".to_string()]),
                labels: Some(HashMap::from([(
                    "com.docker.compose.service".to_string(),
                    "web".to_string(),
                )])),
                state: Some(ContainerSummaryStateEnum::RUNNING),
                status: Some("Up 2 minutes (healthy)".to_string()),
                ports: Some(vec![
                    PortSummary {
                        ip: Some("0.0.0.0".to_string()),
                        private_port: 3000,
                        public_port: Some(3100),
                        typ: Some(PortSummaryTypeEnum::TCP),
                    },
                    PortSummary {
                        ip: Some("::".to_string()),
                        private_port: 3000,
                        public_port: Some(3100),
                        typ: Some(PortSummaryTypeEnum::TCP),
                    },
                    PortSummary {
                        ip: None,
                        private_port: 9229,
                        public_port: None,
                        typ: Some(PortSummaryTypeEnum::TCP),
                    },
                ]),
                ..Default::default()
            },
            ContainerSummary {
                id: Some("id-db".to_string()),
                names: Some(vec!["/proj-wt1-db-1".to_string()]),
                labels: Some(HashMap::from([(
                    "com.docker.compose.service".to_string(),
                    "db".to_string(),
                )])),
                state: Some(ContainerSummaryStateEnum::EXITED),
                status: Some("Exited (0) 5 seconds ago".to_string()),
                ports: Some(vec![]),
                ..Default::default()
            },
            ContainerSummary {
                id: Some("id-worker".to_string()),
                names: Some(vec!["/proj-wt1-worker-1".to_string()]),
                labels: Some(HashMap::from([(
                    "com.docker.compose.service".to_string(),
                    "worker".to_string(),
                )])),
                state: Some(ContainerSummaryStateEnum::RUNNING),
                status: Some("Up 3 seconds (health: starting)".to_string()),
                ports: None,
                ..Default::default()
            },
        ];

        let (containers, ports) = map_container_summaries(summaries);
        assert_eq!(containers.len(), 3);
        assert_eq!(containers[0].name, "proj-wt1-db-1");
        assert_eq!(containers[0].service.as_deref(), Some("db"));
        assert_eq!(containers[0].state, "exited");
        assert_eq!(containers[0].health, None);
        assert!(containers[0].ports.is_empty());

        assert_eq!(containers[1].name, "proj-wt1-web-1");
        assert_eq!(containers[1].health.as_deref(), Some("healthy"));
        assert_eq!(containers[1].ports, vec![3100]);

        assert_eq!(containers[2].name, "proj-wt1-worker-1");
        assert_eq!(containers[2].health.as_deref(), Some("starting"));

        assert_eq!(
            ports,
            vec![PortMapping {
                host_port: 3100,
                container_port: Some(3000),
                service: Some("web".to_string()),
                source: PortSource::Docker,
            }]
        );
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
    fn discover_docker_socket_candidates_respects_docker_host_and_socket_order() {
        let home = Path::new("/Users/alice");
        assert!(
            discover_docker_socket_candidates(Some("unix:///custom.sock"), Some(home)).is_empty()
        );

        assert_eq!(
            discover_docker_socket_candidates(None, Some(home)),
            vec![
                PathBuf::from("/Users/alice/.orbstack/run/docker.sock"),
                PathBuf::from("/Users/alice/.docker/run/docker.sock"),
                PathBuf::from("/Users/alice/.colima/default/docker.sock"),
                PathBuf::from("/Users/alice/.config/colima/default/docker.sock"),
                PathBuf::from("/var/run/docker.sock"),
            ]
        );

        assert_eq!(
            discover_docker_socket_candidates(Some(""), Some(home)),
            vec![
                PathBuf::from("/Users/alice/.orbstack/run/docker.sock"),
                PathBuf::from("/Users/alice/.docker/run/docker.sock"),
                PathBuf::from("/Users/alice/.colima/default/docker.sock"),
                PathBuf::from("/Users/alice/.config/colima/default/docker.sock"),
                PathBuf::from("/var/run/docker.sock"),
            ]
        );

        assert_eq!(
            discover_docker_socket_candidates(None, None),
            vec![PathBuf::from("/var/run/docker.sock")]
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
