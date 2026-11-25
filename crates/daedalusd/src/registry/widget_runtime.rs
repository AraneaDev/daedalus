use super::Registry;
use crate::pane_actor::PaneHandle;
use crate::workbench::documents::validate_definition;
use crate::workbench::runtime::{SourceFields, WidgetInputs, evaluate_widget};
use crate::workbench::scheduler::Trigger;
use crate::workbench::{command, sources};
use daedalus_proto::workbench::{
    CommandOutput, WidgetAction, WidgetActionResult, WidgetDefinition, WidgetResult,
    WidgetSnapshot, WidgetSource, WorkspaceCommand,
};
use daedalus_proto::{PaneRole, ProtoError, WorkspaceId, WorkspaceInfo};

struct WidgetWorkspace {
    info: WorkspaceInfo,
    handles: Vec<PaneHandle>,
}

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
                    .info
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
        let fields = command::run(
            &workspace.info,
            &spec,
            &CommandOutput::Text { contains: None },
        )
        .await;
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
        &self,
        runner: Option<WorkspaceId>,
        jobs: &mut tokio::task::JoinSet<()>,
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
                    self.widget_commands.schedule(
                        workspace.clone(),
                        &widget.id,
                        source,
                        trigger,
                        jobs,
                    );
                }
            }
        }
    }

    async fn widget_workspace(&self, id: WorkspaceId) -> Result<WidgetWorkspace, ProtoError> {
        let (info, handles) = {
            let workspaces = self.workspaces.read().await;
            let panes = self.panes.read().await;
            workspaces
                .get(&id)
                .ok_or(ProtoError::WorkspaceNotFound(id))?
                .snapshot_handles(&panes)
        };
        let info = Self::with_pane_summaries(info, handles.clone()).await;
        Ok(WidgetWorkspace { info, handles })
    }

    async fn widget_result(
        &self,
        workspace: &WidgetWorkspace,
        definition: &WidgetDefinition,
        name: &str,
        saved: Option<(&str, Trigger)>,
    ) -> WidgetResult {
        let mut inputs = WidgetInputs::new();
        for named in &definition.sources {
            let fields = match &named.source {
                WidgetSource::Git => {
                    let root = workspace.info.root_dir.clone();
                    let observation = tokio::task::spawn_blocking(move || {
                        crate::worktree::git_observation(&root)
                    })
                    .await
                    .ok()
                    .flatten();
                    sources::git_fields(observation)
                }
                WidgetSource::Port { port, service } => {
                    sources::port_fields(&workspace.info, *port, service.as_deref())
                }
                WidgetSource::Container { name } => {
                    sources::container_fields(&workspace.info, name.as_deref())
                }
                WidgetSource::Published { key } => sources::published_fields(&workspace.info, key),
                WidgetSource::Command {
                    command: spec,
                    output,
                    ..
                } => match saved {
                    Some((widget, trigger)) => {
                        self.widget_commands
                            .fields(&workspace.info, widget, named, trigger)
                            .await
                    }
                    None => command::run(&workspace.info, spec, output).await,
                },
                WidgetSource::Runner { assignment } => {
                    workspace
                        .pane_fields(PaneRole::Runner, assignment.as_deref())
                        .await
                }
                WidgetSource::Pane { role, config: _ } => workspace.pane_fields(*role, None).await,
            };
            inputs.insert(named.id.clone(), fields);
        }
        evaluate_widget(definition, &inputs, name)
    }
}
impl WidgetWorkspace {
    async fn pane_fields(&self, role: PaneRole, assignment: Option<&str>) -> SourceFields {
        let (pane, handle) = self
            .info
            .panes
            .iter()
            .rev()
            .filter(|pane| pane.role == role)
            .find_map(|pane| {
                let handle = self.handles.iter().find(|handle| handle.id() == pane.id)?;
                assignment
                    .is_none_or(|assignment| handle.assignment_id() == Some(assignment))
                    .then_some((Some(pane), Some(handle)))
            })
            .unwrap_or_default();
        let insight = if role == PaneRole::Runner
            && let Some(handle) = handle
        {
            handle
                .snapshot()
                .await
                .ok()
                .and_then(|snapshot| snapshot.insight)
        } else {
            None
        };
        sources::pane_fields(pane, insight, assignment)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::shell_integration::ShellIntegration;
    use crate::supervisor::{IsolationMode, Supervisor};
    use daedalus_proto::workbench::{
        NamedAction, NamedSource, RefreshPolicy, WidgetDisplay, WidgetField, WidgetInstance,
    };
    use daedalus_proto::{Assignment, PaneStatus, StatusLevel};
    use std::path::Path;

    fn test_registry(path: &Path) -> Registry {
        let (events, _) = tokio::sync::broadcast::channel(32);
        Registry::new(
            Supervisor::with_mode(IsolationMode::PgidFallback),
            ShellIntegration::prepare(path).unwrap(),
            events,
        )
    }

    fn action(id: &str, action: WidgetAction) -> NamedAction {
        NamedAction {
            id: id.into(),
            label: id.into(),
            action,
        }
    }

    async fn install_actions(
        registry: &Registry,
        root: &Path,
        actions: Vec<NamedAction>,
    ) -> WorkspaceId {
        let workspace = registry
            .create_workspace("Action fixture".into(), root.into(), Default::default())
            .await
            .unwrap();
        let mut document = registry.workbench(workspace.id).await.unwrap().document;
        document.widgets.push(WidgetInstance {
            id: "report".into(),
            name: "Report".into(),
            pinned: true,
            template: None,
            definition: Some(WidgetDefinition {
                actions,
                ..WidgetDefinition::default()
            }),
        });
        registry
            .update_workbench(workspace.id, Some(0), document)
            .await
            .unwrap();
        workspace.id
    }

    fn command_action(argv: &[&str]) -> WidgetAction {
        WidgetAction::Command {
            command: WorkspaceCommand {
                argv: argv.iter().map(|arg| (*arg).into()).collect(),
                env: Vec::new(),
                timeout_seconds: Some(3),
            },
        }
    }

    #[tokio::test]
    async fn widget_command_and_diff_run_in_the_workspace_and_return_displayable_details() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let git = |args: &[&str]| {
            let result = std::process::Command::new("git")
                .current_dir(&repo)
                .args([
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "core.hooksPath=/dev/null",
                ])
                .args(args)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "git error: {}",
                String::from_utf8_lossy(&result.stderr)
            );
        };
        git(&["init", "-q"]);
        std::fs::write(
            repo.join("tracked.txt"),
            "before
",
        )
        .unwrap();
        git(&["add", "tracked.txt"]);
        git(&["commit", "-qm", "Initial fixture"]);
        std::fs::write(
            repo.join("tracked.txt"),
            "after
",
        )
        .unwrap();

        let registry = test_registry(dir.path());
        let workspace = install_actions(&registry, &repo, vec![
            action("command", command_action(&[
                "/bin/sh", "-c", "printf 'Local result: '; cat tracked.txt; printf 'check failed\n' >&2; exit 7",
            ])),
            action("diff", WidgetAction::Diff),
            action("missing-program", command_action(&["./missing-executable"])),
        ]).await;

        let WidgetActionResult::Output(result) = registry
            .invoke_widget_action(workspace, "report", "command")
            .await
            .unwrap()
        else {
            panic!("expected command output");
        };
        assert_eq!(result.label, "Local result: after");
        assert_eq!(result.level, StatusLevel::Error);
        assert_eq!(
            result.details,
            [
                ("Output".into(), "Local result: after".into()),
                (
                    "stderr".into(),
                    "check failed
"
                    .into()
                ),
            ]
        );

        let WidgetActionResult::Output(result) = registry
            .invoke_widget_action(workspace, "report", "diff")
            .await
            .unwrap()
        else {
            panic!("expected Git diff");
        };
        assert_eq!(result.label, "diff --git a/tracked.txt b/tracked.txt");
        assert_eq!(result.level, StatusLevel::Ok);
        let diff = &result.details[0];
        assert_eq!(diff.0, "Output");
        assert!(
            diff.1.contains(
                "-before
+after"
            ),
            "{diff:?}"
        );

        let WidgetActionResult::Output(result) = registry
            .invoke_widget_action(workspace, "report", "missing-program")
            .await
            .unwrap()
        else {
            panic!("expected launch error");
        };
        assert_eq!(result.label, "missing-program");
        assert_eq!(result.level, StatusLevel::Error);
        assert!(
            matches!(result.details.as_slice(), [(key, value)] if key == "command" && value.contains("missing-executable"))
        );
    }

    #[tokio::test]
    async fn widget_action_checks_owned_widget_action_and_terminal_ids() {
        let dir = tempfile::tempdir().unwrap();
        let registry = test_registry(dir.path());
        let workspace = install_actions(
            &registry,
            dir.path(),
            vec![
                action(
                    "logs",
                    WidgetAction::Logs {
                        role: PaneRole::Service,
                    },
                ),
                action(
                    "terminal",
                    WidgetAction::Terminal {
                        role: PaneRole::Agent,
                    },
                ),
                action(
                    "url",
                    WidgetAction::OpenUrl {
                        url: "https://example.invalid/report".into(),
                    },
                ),
            ],
        )
        .await;

        for (widget, action, expected) in [
            ("missing", "url", "No widget with ID 'missing'"),
            ("report", "missing", "No action with ID 'missing'"),
            ("report", "logs", "No service terminal"),
            ("report", "terminal", "No agent terminal"),
        ] {
            assert!(matches!(
                registry.invoke_widget_action(workspace, widget, action).await,
                Err(ProtoError::InvalidRequest(message)) if message == expected
            ));
        }
        assert!(matches!(
            registry.widget_results(workspace, Some("missing".into())).await,
            Err(ProtoError::InvalidRequest(message)) if message == "No widget with ID 'missing'"
        ));
        assert_eq!(
            registry
                .invoke_widget_action(workspace, "report", "url")
                .await
                .unwrap(),
            WidgetActionResult::OpenUrl("https://example.invalid/report".into())
        );

        let service = registry
            .spawn_pane(
                workspace,
                PaneRole::Service,
                Some(vec!["/bin/sh".into(), "-c".into(), "exec sleep 5".into()]),
                None,
                80,
                24,
                Default::default(),
            )
            .await
            .unwrap();
        assert_eq!(
            registry
                .invoke_widget_action(workspace, "report", "logs")
                .await
                .unwrap(),
            WidgetActionResult::Pane(service.id)
        );
        registry
            .close_workspace(workspace, false, false, false)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn preview_runs_the_draft_command_and_rejects_unknown_template_source() {
        let dir = tempfile::tempdir().unwrap();
        let registry = test_registry(dir.path());
        let workspace = install_actions(&registry, dir.path(), Vec::new()).await;
        let mut definition = WidgetDefinition {
            sources: vec![NamedSource {
                id: "check".into(),
                source: WidgetSource::Command {
                    command: WorkspaceCommand {
                        argv: vec!["/bin/sh".into(), "-c".into(), "echo Preview-only".into()],
                        env: Vec::new(),
                        timeout_seconds: Some(3),
                    },
                    refresh: RefreshPolicy::default(),
                    output: CommandOutput::Text { contains: None },
                },
            }],
            ..WidgetDefinition::default()
        };
        let result = registry
            .preview_widget(workspace, "Draft".into(), definition.clone())
            .await
            .unwrap();
        assert_eq!(result.label, "Preview-only");
        assert_eq!(result.level, StatusLevel::Ok);
        definition.display.summary.push(WidgetField {
            source: "not-configured".into(),
            field: "label".into(),
            label: None,
        });
        assert!(matches!(
            registry.preview_widget(workspace, "Draft".into(), definition).await,
            Err(ProtoError::InvalidRequest(message)) if message.contains("unknown source 'not-configured'")
        ));
        assert_eq!(
            registry
                .workbench(workspace)
                .await
                .unwrap()
                .document
                .widgets
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn runner_widget_tracks_only_its_saved_assignment() {
        let dir = tempfile::tempdir().unwrap();
        let registry = test_registry(dir.path());
        let workspace = registry
            .create_workspace(
                "Runner inputs".into(),
                dir.path().into(),
                Default::default(),
            )
            .await
            .unwrap();
        let mut document = registry.workbench(workspace.id).await.unwrap().document;
        document.assignments = [("unit", 0), ("integration", 7)]
            .into_iter()
            .map(|(id, code)| Assignment {
                id: id.into(),
                name: id.into(),
                role: PaneRole::Runner,
                argv: vec!["/bin/sh".into(), "-c".into(), format!("exit {code}")],
            })
            .collect();
        for (id, assignment) in [
            ("unit", Some("unit")),
            ("integration", Some("integration")),
            ("missing", Some("missing")),
            ("latest", None),
        ] {
            document.widgets.push(WidgetInstance {
                id: id.into(),
                name: id.into(),
                pinned: true,
                template: None,
                definition: Some(WidgetDefinition {
                    sources: vec![NamedSource {
                        id: "run".into(),
                        source: WidgetSource::Runner {
                            assignment: assignment.map(str::to_owned),
                        },
                    }],
                    display: WidgetDisplay {
                        summary: vec![WidgetField {
                            source: "run".into(),
                            field: "exit_code".into(),
                            label: None,
                        }],
                        ..WidgetDisplay::default()
                    },
                    ..WidgetDefinition::default()
                }),
            });
        }
        registry
            .update_workbench(workspace.id, Some(0), document)
            .await
            .unwrap();
        for (id, code) in [("unit", 0), ("integration", 7)] {
            let runner = registry
                .launch_runner_assignment(workspace.id, id, 80, 24)
                .await
                .unwrap();
            let handle = registry.get_pane(runner.id).await.unwrap();
            assert_eq!(handle.assignment_id(), Some(id));
            tokio::time::timeout(std::time::Duration::from_secs(4), async {
                loop {
                    if handle.summary().await.unwrap().status
                        == (PaneStatus::Exited { code: Some(code) })
                    {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
        }

        let widgets = registry.widget_results(workspace.id, None).await.unwrap();
        let summaries = widgets
            .iter()
            .map(|widget| (widget.id.as_str(), widget.result.label.as_str()))
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(summaries["unit"], "0");
        assert_eq!(summaries["integration"], "7");
        assert_eq!(summaries["missing"], "Runner 'missing' unavailable");
        assert_eq!(summaries["latest"], "7");
        registry
            .close_workspace(workspace.id, false, false, false)
            .await
            .unwrap();
    }
}
