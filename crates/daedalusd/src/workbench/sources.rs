use super::runtime::SourceFields;
use crate::worktree::GitObservation;
use daedalus_proto::{PaneStatus, PaneSummary, RunnerInsight, StatusLevel, WorkspaceInfo};

pub(crate) fn port_fields(
    workspace: &WorkspaceInfo,
    port: Option<u16>,
    service: Option<&str>,
) -> SourceFields {
    let port = workspace.bound_ports.iter().find(|mapping| {
        port.is_none_or(|port| mapping.host_port == port)
            && service.is_none_or(|name| mapping.service.as_deref() == Some(name))
    });
    match port {
        Some(port) => SourceFields::from([
            ("label".into(), format!("Ready · :{}", port.host_port)),
            ("port".into(), port.host_port.to_string()),
            ("level".into(), "ok".into()),
        ]),
        None => SourceFields::from([("label".into(), "No listener".into())]),
    }
}

pub(crate) fn published_fields(workspace: &WorkspaceInfo, key: &str) -> SourceFields {
    workspace
        .external_statuses
        .iter()
        .find(|status| status.source == key)
        .map_or_else(
            || SourceFields::from([("label".into(), "No report".into())]),
            |status| {
                let mut fields = SourceFields::from([
                    ("label".into(), status.label.clone()),
                    ("level".into(), level_name(status.level).into()),
                ]);
                if let Some(detail) = &status.detail {
                    fields.insert("detail".into(), detail.clone());
                }
                fields
            },
        )
}

pub(crate) fn pane_fields(
    pane: Option<&PaneSummary>,
    insight: Option<RunnerInsight>,
    assignment: Option<&str>,
) -> SourceFields {
    let Some(pane) = pane else {
        return SourceFields::from([(
            "label".into(),
            assignment.map_or_else(
                || "No terminal".into(),
                |id| format!("Runner '{id}' unavailable"),
            ),
        )]);
    };
    let status = match pane.status {
        PaneStatus::Starting => "Starting",
        PaneStatus::Idle => "Idle",
        PaneStatus::Busy => "Running",
        PaneStatus::Exited { code: Some(0) } => "Passed",
        PaneStatus::Exited { .. } => "Exited",
    };
    let mut fields = SourceFields::from([
        ("label".into(), status.into()),
        ("phase".into(), status.into()),
    ]);
    let exit_code = match pane.status {
        PaneStatus::Exited { code: Some(code) } => Some(code),
        _ => pane.last_exit_code,
    };
    if let Some(code) = exit_code {
        fields.insert("exit_code".into(), code.to_string());
        fields.insert(
            "level".into(),
            if code == 0 { "ok" } else { "error" }.into(),
        );
    }
    if let Some(insight) = insight {
        fields.extend([
            ("phase".into(), insight.phase),
            ("passed".into(), insight.passed.to_string()),
            ("failed".into(), insight.failed.to_string()),
            ("warnings".into(), insight.warnings.to_string()),
            ("elapsed_secs".into(), insight.elapsed_secs.to_string()),
        ]);
        if let Some(diagnostic) = insight.diagnostic {
            fields.insert("diagnostic".into(), diagnostic);
        }
    }
    fields
}

pub(crate) fn git_fields(observation: Option<GitObservation>) -> SourceFields {
    let Some(git) = observation else {
        return SourceFields::from([("label".into(), "Git unavailable".into())]);
    };
    let mut fields = SourceFields::from([
        (
            "branch".into(),
            git.branch.unwrap_or_else(|| "detached".into()),
        ),
        ("changed_files".into(), git.changes.to_string()),
        (
            "changes".into(),
            if git.changes == 0 {
                "Clean".into()
            } else {
                format!("{} changed", git.changes)
            },
        ),
        ("dirty".into(), git.status.dirty.to_string()),
    ]);
    if git.upstream {
        fields.extend([
            ("ahead".into(), git.status.ahead.to_string()),
            ("behind".into(), git.status.behind.to_string()),
        ]);
        if git.status.ahead != 0 || git.status.behind != 0 {
            fields.insert(
                "upstream".into(),
                format!("↑{} ↓{}", git.status.ahead, git.status.behind),
            );
        }
    }
    fields
}

pub(crate) fn container_fields(workspace: &WorkspaceInfo, name: Option<&str>) -> SourceFields {
    let matching = workspace.containers.iter().filter(|container| {
        name.is_none_or(|name| container.name == name || container.service.as_deref() == Some(name))
    });
    let total = matching.clone().count();
    let healthy = matching
        .filter(|container| {
            container
                .health
                .as_deref()
                .map_or(container.state == "running", |health| health == "healthy")
        })
        .count();
    SourceFields::from([
        (
            "label".into(),
            if total == 0 {
                "No containers".into()
            } else {
                format!("{healthy}/{total} healthy")
            },
        ),
        ("healthy".into(), healthy.to_string()),
        ("total".into(), total.to_string()),
        (
            "level".into(),
            if total == 0 {
                "info"
            } else if total == healthy {
                "ok"
            } else {
                "warn"
            }
            .into(),
        ),
    ])
}

fn level_name(value: StatusLevel) -> &'static str {
    match value {
        StatusLevel::Info => "info",
        StatusLevel::Ok => "ok",
        StatusLevel::Warn => "warn",
        StatusLevel::Error => "error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbench::test_workspace;
    use daedalus_proto::{
        ContainerHealth, ExternalStatus, GitWorkspaceStatus, PaneId, PaneRole, PortMapping,
        PortSource, WorkspaceId,
    };

    #[test]
    fn port_uses_both_host_port_and_service_to_select_a_listener() {
        let mut workspace = test_workspace(Default::default());
        workspace.bound_ports = vec![
            PortMapping {
                host_port: 8080,
                container_port: Some(80),
                service: Some("web".into()),
                source: PortSource::Docker,
            },
            PortMapping {
                host_port: 8081,
                container_port: None,
                service: None,
                source: PortSource::LocalProcess,
            },
        ];

        let fields = port_fields(&workspace, None, Some("web"));
        assert_eq!(fields["label"], "Ready · :8080");
        assert_eq!(fields["port"], "8080");
        assert_eq!(fields["level"], "ok");
        assert_eq!(port_fields(&workspace, Some(8081), None)["port"], "8081");

        for (port, service) in [(Some(80), None), (Some(8081), Some("web"))] {
            let fields = port_fields(&workspace, port, service);
            assert_eq!(fields["label"], "No listener");
            assert!(!fields.contains_key("port"));
        }
    }

    #[test]
    fn container_source_uses_engine_health_and_supports_both_names() {
        let mut workspace = test_workspace(Default::default());
        workspace.containers = [
            ("project-web-1", "web", "running", Some("healthy")),
            ("project-web-2", "web", "running", Some("unhealthy")),
            ("project-worker-1", "worker", "running", None),
            ("project-cache-1", "cache", "exited", None),
        ]
        .into_iter()
        .map(|(name, service, state, health)| ContainerHealth {
            name: name.into(),
            service: Some(service.into()),
            state: state.into(),
            health: health.map(str::to_owned),
            ports: Vec::new(),
        })
        .collect();

        let all = container_fields(&workspace, None);
        assert_eq!(all["label"], "2/4 healthy");
        assert_eq!(all["level"], "warn");

        let web = container_fields(&workspace, Some("web"));
        assert_eq!(web["healthy"], "1");
        assert_eq!(web["total"], "2");
        assert_eq!(web["level"], "warn");

        let instance = container_fields(&workspace, Some("project-web-1"));
        assert_eq!(instance["label"], "1/1 healthy");
        assert_eq!(instance["level"], "ok");

        let worker = container_fields(&workspace, Some("worker"));
        assert_eq!(worker["level"], "ok");
        assert_eq!(container_fields(&workspace, Some("cache"))["level"], "warn");

        let absent = container_fields(&workspace, Some("not-running-here"));
        assert_eq!(absent["label"], "No containers");
        assert_eq!(absent["total"], "0");
        assert_eq!(absent["level"], "info");
    }

    #[test]
    fn published_status_is_selected_by_key_with_its_level_and_optional_detail() {
        let mut workspace = test_workspace(Default::default());
        let cases = [
            ("backup", StatusLevel::Info, "Scheduled", None, "info"),
            ("ci", StatusLevel::Ok, "Passed", Some("128 tests"), "ok"),
            ("queue", StatusLevel::Warn, "Busy", None, "warn"),
            (
                "deploy",
                StatusLevel::Error,
                "Failed",
                Some("Image unavailable"),
                "error",
            ),
        ];
        for (source, level, label, detail, _) in cases {
            workspace.external_statuses.push(ExternalStatus {
                source: source.into(),
                level,
                label: label.into(),
                detail: detail.map(str::to_owned),
                updated_at_ms: 50,
            });
        }
        for (source, _, label, detail, level) in cases {
            let fields = published_fields(&workspace, source);
            assert_eq!(fields["level"], level);
            assert_eq!(fields["label"], label);
            assert_eq!(fields.get("detail").map(String::as_str), detail);
        }
        assert_eq!(
            published_fields(&workspace, "missing")["label"],
            "No report"
        );
    }

    #[test]
    fn runner_fields_prefer_exit_status_and_include_diagnostic_and_counts() {
        let mut pane = PaneSummary {
            id: PaneId::new(),
            workspace_id: WorkspaceId::new(),
            role: PaneRole::Runner,
            status: PaneStatus::Starting,
            cols: 80,
            rows: 24,
            cwd: None,
            last_exit_code: None,
            has_failure_trace: false,
            launch_preset: None,
            agent_state: None,
        };
        for (status, label) in [
            (PaneStatus::Starting, "Starting"),
            (PaneStatus::Idle, "Idle"),
            (PaneStatus::Busy, "Running"),
            (PaneStatus::Exited { code: None }, "Exited"),
        ] {
            pane.status = status;
            let fields = pane_fields(Some(&pane), None, Some("integration"));
            assert_eq!(fields["label"], label);
            assert_eq!(fields["phase"], label);
            assert!(!fields.contains_key("exit_code"));
        }

        pane.last_exit_code = Some(2);
        pane.status = PaneStatus::Idle;
        let fields = pane_fields(Some(&pane), None, None);
        assert_eq!(fields["exit_code"], "2");
        assert_eq!(fields["level"], "error");

        pane.status = PaneStatus::Exited { code: Some(0) };
        let success = pane_fields(Some(&pane), None, Some("integration"));
        assert_eq!(success["label"], "Passed");
        assert_eq!(success["exit_code"], "0");
        assert_eq!(success["level"], "ok");

        pane.status = PaneStatus::Exited { code: Some(7) };
        let failed = pane_fields(
            Some(&pane),
            Some(RunnerInsight {
                phase: "Failed".into(),
                passed: 4,
                failed: 1,
                warnings: 2,
                elapsed_secs: 9,
                diagnostic: Some("tests::permissions".into()),
            }),
            Some("integration"),
        );
        assert_eq!(failed["exit_code"], "7");
        assert_eq!(failed["phase"], "Failed");
        assert_eq!(failed["passed"], "4");
        assert_eq!(failed["failed"], "1");
        assert_eq!(failed["warnings"], "2");
        assert_eq!(failed["elapsed_secs"], "9");
        assert_eq!(failed["diagnostic"], "tests::permissions");
        assert_eq!(pane_fields(None, None, None)["label"], "No terminal");
        assert_eq!(
            pane_fields(None, None, Some("integration"))["label"],
            "Runner 'integration' unavailable"
        );
    }

    #[test]
    fn git_fields_include_upstream_counts_only_when_tracking_a_branch() {
        let changed = git_fields(Some(GitObservation {
            status: GitWorkspaceStatus {
                is_worktree: true,
                dirty: true,
                ahead: 2,
                behind: 1,
            },
            branch: Some("feature/runtime".into()),
            changes: 3,
            upstream: true,
        }));
        assert_eq!(changed["branch"], "feature/runtime");
        assert_eq!(changed["changed_files"], "3");
        assert_eq!(changed["changes"], "3 changed");
        assert_eq!(changed["dirty"], "true");
        assert_eq!(changed["ahead"], "2");
        assert_eq!(changed["behind"], "1");
        assert_eq!(changed["upstream"], "↑2 ↓1");

        for tracking in [true, false] {
            let clean = git_fields(Some(GitObservation {
                status: GitWorkspaceStatus::default(),
                branch: None,
                changes: 0,
                upstream: tracking,
            }));
            assert_eq!(clean["branch"], "detached");
            assert_eq!(clean["changes"], "Clean");
            assert_eq!(clean.contains_key("ahead"), tracking);
            assert!(!clean.contains_key("upstream"));
        }

        assert_eq!(git_fields(None)["label"], "Git unavailable");
    }
}
