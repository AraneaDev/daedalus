use super::Registry;
use crate::workbench::command;
use crate::workbench::documents::validate_definition;
use crate::workbench::runtime::{SourceFields, WidgetInputs, evaluate_widget};
use crate::workbench::scheduler::Trigger;
use daedalus_proto::workbench::{
    CommandOutput, WidgetAction, WidgetActionResult, WidgetDefinition, WidgetResult,
    WidgetSnapshot, WidgetSource, WorkspaceCommand,
};
use daedalus_proto::{PaneRole, PaneStatus, ProtoError, StatusLevel, WorkspaceId, WorkspaceInfo};

impl Registry {
    pub(crate) async fn invoke_widget_action(
        &self,
        id: WorkspaceId,
        widget_id: &str,
        action_id: &str,
    ) -> Result<WidgetActionResult, ProtoError> {
        let state = self.workbench(id).await?;
        let definition = state
            .document
            .widgets
            .iter()
            .find(|widget| widget.id == widget_id)
            .and_then(|widget| state.document.widget_definition(widget))
            .ok_or_else(|| {
                ProtoError::InvalidRequest(format!("No widget with ID '{widget_id}'"))
            })?;
        let action = definition
            .actions
            .iter()
            .find(|action| action.id == action_id)
            .ok_or_else(|| {
                ProtoError::InvalidRequest(format!("No action with ID '{action_id}'"))
            })?;
        let workspace = self.widget_workspace(id).await?;
        let spec = match &action.action {
            WidgetAction::Terminal { role } | WidgetAction::Logs { role } => {
                return workspace
                    .panes
                    .iter()
                    .rev()
                    .find(|pane| pane.role == *role)
                    .map(|pane| WidgetActionResult::Pane(pane.id))
                    .ok_or_else(|| ProtoError::InvalidRequest(format!("No {role} terminal")));
            }
            WidgetAction::OpenUrl { url } => return Ok(WidgetActionResult::OpenUrl(url.clone())),
            WidgetAction::Diff => WorkspaceCommand {
                argv: vec!["git".into(), "diff".into(), "HEAD".into(), "--".into()],
                env: Vec::new(),
                timeout_seconds: None,
            },
            WidgetAction::Command { command } => command.clone(),
        };
        let fields = command::run(&workspace, &spec, &CommandOutput::Text { contains: None }).await;
        let inputs = WidgetInputs::from([("command".into(), fields)]);
        Ok(WidgetActionResult::Output(evaluate_widget(
            &WidgetDefinition::default(),
            &inputs,
            &action.label,
        )))
    }
    pub(crate) async fn preview_widget(
        &self,
        id: WorkspaceId,
        name: String,
        definition: WidgetDefinition,
    ) -> Result<WidgetResult, ProtoError> {
        validate_definition(&definition, "widget preview")
            .map_err(|error| ProtoError::InvalidRequest(error.to_string()))?;
        let workspace = self.widget_workspace(id).await?;
        Ok(self
            .widget_result(&workspace, &definition, &name, None)
            .await)
    }

    pub(crate) async fn widget_results(
        &self,
        id: WorkspaceId,
        refresh: Option<String>,
    ) -> Result<Vec<WidgetSnapshot>, ProtoError> {
        let state = self.workbench(id).await?;
        let workspace = self.widget_workspace(id).await?;
        if let Some(id) = &refresh
            && !state.document.widgets.iter().any(|widget| &widget.id == id)
        {
            return Err(ProtoError::InvalidRequest(format!(
                "No widget with ID '{id}'"
            )));
        }
        let mut results = Vec::new();
        for widget in &state.document.widgets {
            let Some(definition) = state.document.widget_definition(widget) else {
                continue;
            };
            let result = self
                .widget_result(
                    &workspace,
                    definition,
                    &widget.name,
                    Some((
                        &widget.id,
                        if refresh.as_ref() == Some(&widget.id) {
                            Trigger::Manual
                        } else {
                            Trigger::View
                        },
                    )),
                )
                .await;
            results.push(WidgetSnapshot {
                id: widget.id.clone(),
                result,
            });
        }
        Ok(results)
    }

    pub(crate) async fn refresh_scheduled_widgets(
        self: &std::sync::Arc<Self>,
        runner: Option<WorkspaceId>,
    ) {
        let trigger = if runner.is_some() {
            Trigger::Runner
        } else {
            Trigger::Timer
        };
        let workspaces = self
            .workspaces
            .read()
            .await
            .values()
            .filter(|workspace| runner.is_none_or(|id| workspace.id == id))
            .map(|workspace| workspace.to_info(Vec::new()))
            .collect::<Vec<_>>();
        let mut jobs = tokio::task::JoinSet::new();
        for workspace in workspaces {
            let Ok(state) = self.workbench(workspace.id).await else {
                continue;
            };
            if state.revision == 0 {
                continue;
            }
            let workspace = std::sync::Arc::new(workspace);
            for widget in &state.document.widgets {
                let Some(definition) = state.document.widget_definition(widget) else {
                    continue;
                };
                for source in &definition.sources {
                    if !matches!(source.source, WidgetSource::Command { .. }) {
                        continue;
                    }
                    let registry = self.clone();
                    let workspace = workspace.clone();
                    let id = widget.id.clone();
                    let source = source.clone();
                    jobs.spawn(async move {
                        registry
                            .widget_commands
                            .fields(&workspace, &id, &source, trigger)
                            .await;
                    });
                }
            }
        }
        while jobs.join_next().await.is_some() {}
    }

    async fn widget_workspace(&self, id: WorkspaceId) -> Result<WorkspaceInfo, ProtoError> {
        let (info, handles) = {
            let workspaces = self.workspaces.read().await;
            let panes = self.panes.read().await;
            workspaces
                .get(&id)
                .ok_or(ProtoError::WorkspaceNotFound(id))?
                .snapshot_handles(&panes)
        };
        Ok(Self::with_pane_summaries(info, handles).await)
    }

    async fn widget_result(
        &self,
        workspace: &WorkspaceInfo,
        definition: &WidgetDefinition,
        name: &str,
        saved: Option<(&str, Trigger)>,
    ) -> WidgetResult {
        let mut inputs = WidgetInputs::new();
        for named in &definition.sources {
            let fields = match &named.source {
                WidgetSource::Git => git_fields(workspace).await,
                WidgetSource::Port { port, service } => {
                    let port = workspace.bound_ports.iter().find(|mapping| {
                        port.is_none_or(|port| mapping.host_port == port)
                            && service
                                .as_ref()
                                .is_none_or(|name| mapping.service.as_ref() == Some(name))
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
                WidgetSource::Container { name } => container_fields(workspace, name.as_deref()),
                WidgetSource::Published { key } => workspace
                    .external_statuses
                    .iter()
                    .find(|status| &status.source == key)
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
                    ),
                WidgetSource::Command {
                    command: spec,
                    output,
                    ..
                } => match saved {
                    Some((widget, trigger)) => {
                        self.widget_commands
                            .fields(workspace, widget, named, trigger)
                            .await
                    }
                    None => command::run(workspace, spec, output).await,
                },
                WidgetSource::Runner { assignment: _ } => {
                    self.pane_fields(workspace, PaneRole::Runner).await
                }
                WidgetSource::Pane { role, config: _ } => self.pane_fields(workspace, *role).await,
            };
            inputs.insert(named.id.clone(), fields);
        }
        evaluate_widget(definition, &inputs, name)
    }

    async fn pane_fields(&self, workspace: &WorkspaceInfo, role: PaneRole) -> SourceFields {
        let Some(pane) = workspace.panes.iter().rev().find(|pane| pane.role == role) else {
            return SourceFields::from([("label".into(), "No terminal".into())]);
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
        if let Some(code) = pane.last_exit_code {
            fields.insert("exit_code".into(), code.to_string());
            fields.insert(
                "level".into(),
                if code == 0 { "ok" } else { "error" }.into(),
            );
        }
        if role == PaneRole::Runner
            && let Ok(handle) = self.get_pane(pane.id).await
            && let Ok(snapshot) = handle.snapshot().await
            && let Some(insight) = snapshot.insight
        {
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
}

async fn git_fields(workspace: &WorkspaceInfo) -> SourceFields {
    let root = workspace.root_dir.clone();
    let observation = tokio::task::spawn_blocking(move || crate::worktree::git_observation(&root))
        .await
        .ok()
        .flatten();
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

fn container_fields(workspace: &WorkspaceInfo, name: Option<&str>) -> SourceFields {
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
