use crate::registry::Registry;
use bollard::Docker;
use bollard::container::ListContainersOptions;
use bollard::models::ContainerSummary;
use daedalus_proto::{ContainerHealth, PortMapping, PortSource, WorkspaceId};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
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

/// Walks `/proc/<pid>/stat` to collect `roots` and all live descendant PIDs.
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

fn parse_socket_inode(link_target: &std::path::Path) -> Option<u64> {
    let s = link_target.to_str()?;
    let inner = s.strip_prefix("socket:[")?.strip_suffix(']')?;
    inner.parse::<u64>().ok()
}

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

        let state = summary.state.unwrap_or_else(|| "unknown".to_string());
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
        filters,
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

            let local_targets: Vec<(WorkspaceId, Vec<u32>)> = targets
                .iter()
                .map(|t| (t.workspace_id, t.root_pids.clone()))
                .collect();

            let local_ports_fut = tokio::task::spawn_blocking(move || {
                local_targets
                    .into_iter()
                    .map(|(ws_id, roots)| (ws_id, discover_local_ports_for_roots(&roots)))
                    .collect::<HashMap<WorkspaceId, Vec<PortMapping>>>()
            });

            let mut local_by_ws = tokio::select! {
                biased;
                _ = wait_for_shutdown(&mut shutdown) => break,
                res = local_ports_fut => match res {
                    Ok(map) => map,
                    Err(e) => {
                        debug!(error = %e, "local port discovery task failed");
                        HashMap::new()
                    }
                }
            };

            if targets.iter().any(|t| t.compose_project_name.is_some()) && docker_client.is_none() {
                match Docker::connect_with_local_defaults() {
                    Ok(client) => docker_client = Some(client),
                    Err(e) => {
                        if !docker_unavailable_logged {
                            debug!(error = %e, "docker unavailable; skipping container health polling");
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

                if let Some(local_ports) = local_by_ws.remove(&target.workspace_id) {
                    bound_ports.extend(local_ports);
                }

                registry
                    .update_workspace_runtime(target.workspace_id, bound_ports, containers)
                    .await;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::{Port, PortTypeEnum};
    use std::net::TcpListener;
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
        assert!(found.service.is_some());

        drop(listener);
    }

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
                state: Some("running".to_string()),
                status: Some("Up 2 minutes (healthy)".to_string()),
                ports: Some(vec![
                    Port {
                        ip: Some("0.0.0.0".to_string()),
                        private_port: 3000,
                        public_port: Some(3100),
                        typ: Some(PortTypeEnum::TCP),
                    },
                    Port {
                        ip: Some("::".to_string()),
                        private_port: 3000,
                        public_port: Some(3100),
                        typ: Some(PortTypeEnum::TCP),
                    },
                    Port {
                        ip: None,
                        private_port: 9229,
                        public_port: None,
                        typ: Some(PortTypeEnum::TCP),
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
                state: Some("exited".to_string()),
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
                state: Some("running".to_string()),
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
}
