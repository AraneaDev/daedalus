mod docker;
mod local_ports;

pub use docker::{connect_docker_client, discover_docker_socket_candidates, poll_workspace_docker};
#[cfg(target_os = "linux")]
pub(crate) use local_ports::parse_proc_stat_pid_ppid;
pub use local_ports::{
    collect_descendant_pids, discover_local_ports_for_roots, parse_lsof_listen_output,
    parse_proc_net_tcp, parse_ps_pid_comm_table, parse_windows_netstat_output,
};

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
                let (containers, bound_ports, git_status, git_branch) = merge_runtime_observations(
                    observations,
                    local_by_ws.remove(&target.workspace_id),
                );

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
type ComposeObservation = (Vec<ContainerHealth>, Vec<PortMapping>);

fn probe_local_targets(
    targets: &[WorkspaceMonitorTarget],
) -> JoinHandle<HashMap<WorkspaceId, LocalObservation>> {
    let targets: Vec<_> = targets
        .iter()
        .map(|t| (t.workspace_id, t.root_pids.clone(), t.root_dir.clone()))
        .collect();
    tokio::task::spawn_blocking(move || {
        targets
            .into_iter()
            .map(|(id, roots, dir)| {
                let ports = discover_local_ports_for_roots(&roots);
                let (status, branch) = match worktree::git_workspace_status_and_branch(&dir) {
                    Some((status, branch)) => (Some(status), branch),
                    None => (None, None),
                };
                (id, (ports, status, branch))
            })
            .collect()
    })
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
