use bollard::Docker;
use bollard::models::ContainerSummary;
use bollard::query_parameters::ListContainersOptions;
use daedalus_proto::{ContainerHealth, PortMapping, PortSource};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

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

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::{ContainerSummaryStateEnum, PortSummary, PortSummaryTypeEnum};

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
}
