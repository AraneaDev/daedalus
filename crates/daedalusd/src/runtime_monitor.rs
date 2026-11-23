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
use daedalus_proto::{GitWorkspaceStatus, PortMapping, WorkspaceId};
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
