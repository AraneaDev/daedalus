mod docker;
mod local_ports;

pub use crate::process::inventory::{
    descendant_pids_including_roots as collect_descendant_pids, parse_ps_pid_comm_table,
};
pub use docker::{connect_docker_client, discover_docker_socket_candidates, poll_workspace_docker};
pub use local_ports::{
    discover_local_ports_for_roots, parse_lsof_listen_output, parse_proc_net_tcp,
    parse_windows_netstat_output,
};

use crate::process::monitor_root::MonitorRoot;
use crate::registry::Registry;
use crate::worktree;
use bollard::Docker;
use daedalus_proto::{ContainerHealth, GitWorkspaceStatus, PortMapping, WorkspaceId};
use std::collections::HashMap;
use std::path::PathBuf;
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

pub(crate) struct ActiveMonitorTarget {
    pub(crate) snapshot: WorkspaceMonitorTarget,
    pub(crate) monitor_roots: Vec<MonitorRoot>,
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

            let targets = registry.monitor_targets_with_roots().await;
            if targets.is_empty() {
                continue;
            }

            let local_probe_fut = probe_local_targets(&targets);

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

            if targets
                .iter()
                .any(|t| t.snapshot.compose_project_name.is_some())
                && docker_client.is_none()
            {
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

            for ActiveMonitorTarget {
                snapshot: target, ..
            } in targets
            {
                if *shutdown.borrow() {
                    break;
                }

                let observations = match (
                    docker_client.as_ref(),
                    target.compose_project_name.as_deref(),
                ) {
                    (Some(docker), Some(project)) => tokio::select! {
                        biased;
                        _ = wait_for_shutdown(&mut shutdown) => break,
                        result = poll_compose_observations(docker, project, &mut docker_unavailable_logged) => result,
                    },
                    _ => Default::default(),
                };
                let mut local = None;
                if let Some(((mut ports, status, branch), mut sampled)) =
                    local_by_ws.remove(&target.workspace_id)
                {
                    if sampled.iter().any(|root| root.live_pid().is_none()) {
                        // Docker polling can outlast a pane. Resample the surviving
                        // processes so their ports stay visible when this one exits.
                        (ports, sampled) = tokio::select! {
                            biased;
                            _ = wait_for_shutdown(&mut shutdown) => break,
                            result = tokio::task::spawn_blocking(move || sample_ports(sampled)) =>
                                result.unwrap_or_default(),
                        };
                    }
                    if sampled.iter().any(|root| root.live_pid().is_none()) {
                        ports.clear();
                    }
                    local = Some((ports, status, branch));
                }
                let (containers, bound_ports, git_status, git_branch) =
                    merge_runtime_observations(observations, local);

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

type LocalObservation = (Vec<PortMapping>, Option<GitWorkspaceStatus>, Option<String>);
type LocalSample = (LocalObservation, Vec<MonitorRoot>);
type ComposeObservation = (Vec<ContainerHealth>, Vec<PortMapping>);

fn probe_local_targets(
    targets: &[ActiveMonitorTarget],
) -> JoinHandle<HashMap<WorkspaceId, LocalSample>> {
    let targets: Vec<_> = targets
        .iter()
        .map(|t| {
            (
                t.snapshot.workspace_id,
                t.monitor_roots.clone(),
                t.snapshot.root_dir.clone(),
            )
        })
        .collect();
    tokio::task::spawn_blocking(move || {
        targets
            .into_iter()
            .map(|(id, roots, dir)| {
                let (status, branch) = match worktree::git_workspace_status_and_branch(&dir) {
                    Some((status, branch)) => (Some(status), branch),
                    None => (None, None),
                };
                let (ports, sampled) = sample_ports(roots);
                (id, ((ports, status, branch), sampled))
            })
            .collect()
    })
}

fn sample_ports(mut roots: Vec<MonitorRoot>) -> (Vec<PortMapping>, Vec<MonitorRoot>) {
    for _ in 0..2 {
        roots.retain(MonitorRoot::is_current);
        let pids: Vec<_> = roots.iter().filter_map(MonitorRoot::live_pid).collect();
        let ports = discover_local_ports_for_roots(&pids);
        if roots.iter().all(MonitorRoot::is_current) {
            return (ports, roots);
        }
    }
    // Process churn exceeded the bounded retry budget for this poll.
    (Vec::new(), Vec::new())
}

async fn poll_compose_observations(
    docker: &Docker,
    project: &str,
    unavailable_logged: &mut bool,
) -> ComposeObservation {
    match tokio::time::timeout(DOCKER_POLL_TIMEOUT, poll_workspace_docker(docker, project)).await {
        Ok(Ok(result)) => {
            *unavailable_logged = false;
            return result;
        }
        Ok(Err(error)) if !*unavailable_logged => {
            debug!(%error, %project, "docker unavailable; skipping container health polling")
        }
        Err(_) if !*unavailable_logged => {
            debug!(%project, "docker poll timed out; skipping container health polling")
        }
        _ => {}
    }
    *unavailable_logged = true;
    Default::default()
}

fn merge_runtime_observations(
    (containers, mut ports): ComposeObservation,
    local: Option<LocalObservation>,
) -> (
    Vec<ContainerHealth>,
    Vec<PortMapping>,
    Option<GitWorkspaceStatus>,
    Option<String>,
) {
    let (local_ports, status, branch) = local.unwrap_or_default();
    ports.extend(local_ports);
    (containers, ports, status, branch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use daedalus_proto::PortSource;

    #[tokio::test]
    async fn docker_poll_recovers_from_errors_and_uses_compose_project_filter() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let docker = Docker::connect_with_http(
            &format!("http://{}", listener.local_addr().unwrap()),
            5,
            bollard::API_DEFAULT_VERSION,
        )
        .unwrap();
        let responder = tokio::spawn(async move {
            for (status, body) in [
                ("500 Internal Server Error", r#"{"message":"offline"}"#),
                ("500 Internal Server Error", r#"{"message":"offline"}"#),
                (
                    "200 OK",
                    r#"[{"Id":"a","Ports":[{"PrivatePort":80,"PublicPort":8080,"Type":"tcp"}]},{"Id":"b","Labels":{"com.docker.compose.service":"web"},"Ports":[{"PrivatePort":80,"PublicPort":8080,"Type":"tcp"}],"Status":"Up (unhealthy)"}]"#,
                ),
            ] {
                let (stream, _) = listener.accept().await.unwrap();
                let mut stream = BufReader::new(stream);
                let mut request = String::new();
                stream.read_line(&mut request).await.unwrap();
                assert!(
                    request.contains("/containers/json?")
                        && request.contains("com.docker.compose.project")
                        && request.contains("demo"),
                    "{request}"
                );
                loop {
                    let mut line = String::new();
                    stream.read_line(&mut line).await.unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                }
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                stream
                    .get_mut()
                    .write_all(response.as_bytes())
                    .await
                    .unwrap();
            }
        });
        let mut unavailable = false;
        for _ in 0..2 {
            assert_eq!(
                poll_compose_observations(&docker, "demo", &mut unavailable).await,
                Default::default()
            );
            assert!(unavailable);
        }
        let (containers, ports) =
            poll_compose_observations(&docker, "demo", &mut unavailable).await;
        responder.await.unwrap();
        assert!(!unavailable);
        assert_eq!(containers.len(), 2);
        assert_eq!(containers[1].name, "b");
        assert_eq!(containers[1].health.as_deref(), Some("unhealthy"));
        assert_eq!(ports.len(), 1);
        assert_eq!(ports[0].service.as_deref(), Some("web"));
        assert_eq!(ports[0].host_port, 8080);
    }

    #[test]
    fn merge_preserves_compose_then_local_order_and_git_branch() {
        let port = |host_port, source| PortMapping {
            host_port,
            source,
            container_port: None,
            service: None,
        };
        let docker = port(8000, PortSource::Docker);
        let local = port(3000, PortSource::LocalProcess);
        let (_, ports, _, branch) = merge_runtime_observations(
            (vec![], vec![docker.clone()]),
            Some((vec![local.clone()], None, Some("dev".into()))),
        );
        assert_eq!(ports, [docker, local]);
        assert_eq!(branch.as_deref(), Some("dev"));
    }
}
