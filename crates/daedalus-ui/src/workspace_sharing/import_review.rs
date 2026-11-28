use daedalus_proto::StatusLevel;
use daedalus_proto::workbench::{
    RefreshPolicy, WidgetAction, WidgetResult, WidgetSource, WorkspaceCommand, WorkspaceDocument,
};

pub(super) fn project(
    label: String,
    current: Option<&WorkspaceDocument>,
    proposed: &WorkspaceDocument,
) -> WidgetResult {
    let mut details = Vec::new();
    if current.is_none_or(|current| current.layout != proposed.layout) {
        let layout = if proposed.layout.split_view {
            "Split view"
        } else {
            "Focused agent"
        };
        details.push(("Layout".into(), layout.into()));
    }
    details.extend(
        proposed
            .assignments
            .iter()
            .map(|assignment| (assignment.name.clone(), argv_json(&assignment.argv))),
    );
    for widget in &proposed.widgets {
        let Some(definition) = proposed.widget_definition(widget) else {
            continue;
        };
        for source in &definition.sources {
            if let WidgetSource::Command {
                command, refresh, ..
            } = &source.source
            {
                append_command(
                    &mut details,
                    format!("{} · {}", widget.name, source.id),
                    command,
                    &schedule(refresh),
                );
            }
        }
        for action in &definition.actions {
            if let WidgetAction::Command {
                command,
                run_in_runner,
            } = &action.action
            {
                append_command(
                    &mut details,
                    format!("{} · {}", widget.name, action.label),
                    command,
                    if *run_in_runner {
                        "Manual runner launch"
                    } else {
                        "Manual action"
                    },
                );
            }
        }
    }
    WidgetResult {
        element_values: Default::default(),
        label,
        level: StatusLevel::Info,
        details,
    }
}

fn argv_json(argv: &[String]) -> String {
    serde_json::to_string(argv).expect("string arrays serialize to JSON")
}

fn append_command(
    details: &mut Vec<(String, String)>,
    label: String,
    command: &WorkspaceCommand,
    schedule: &str,
) {
    details.push((label, argv_json(&command.argv)));
    details.push(("Trigger".into(), schedule.into()));
    if !command.env.is_empty() {
        details.push(("Environment names".into(), argv_json(&command.env)));
    }
    if let Some(timeout) = command.timeout_seconds {
        details.push(("Timeout".into(), format!("{timeout}s")));
    }
}

fn schedule(refresh: &RefreshPolicy) -> String {
    match (refresh.interval_seconds, refresh.on_runner_completion) {
        (Some(seconds), true) => format!("Every {seconds}s and on runner completion"),
        (Some(seconds), false) => format!("Every {seconds}s"),
        (None, true) => "On runner completion".into(),
        (None, false) => "Manual refresh".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daedalus_proto::workbench::{
        NamedAction, NamedSource, WidgetDefinition, WidgetInstance, WidgetTemplate,
    };

    #[test]
    fn template_review_lists_all_trigger_modes_and_manual_actions() {
        let command = WorkspaceCommand {
            argv: vec!["cargo".into(), "check".into()],
            env: Vec::new(),
            timeout_seconds: None,
        };
        let definition = WidgetDefinition {
            sources: [(Some(30), false), (None, true), (None, false)]
                .into_iter()
                .enumerate()
                .map(
                    |(index, (interval_seconds, on_runner_completion))| NamedSource {
                        id: format!("source{index}"),
                        source: WidgetSource::Command {
                            command: command.clone(),
                            refresh: RefreshPolicy {
                                interval_seconds,
                                on_runner_completion,
                            },
                            output: Default::default(),
                        },
                    },
                )
                .collect(),
            actions: vec![NamedAction {
                id: "run".into(),
                label: "Run".into(),
                action: WidgetAction::Command {
                    command,
                    run_in_runner: false,
                },
            }],
            ..Default::default()
        };
        let mut document = WorkspaceDocument::new("Project".into());
        document.templates.push(WidgetTemplate {
            id: "command-template".into(),
            name: "Command template".into(),
            definition,
        });
        document.widgets.push(WidgetInstance {
            id: "instance".into(),
            name: "Widget".into(),
            pinned: true,
            template: Some("command-template".into()),
            definition: None,
        });
        let review = project("Review".into(), Some(&document), &document);
        let triggers = review
            .details
            .iter()
            .filter_map(|(key, value)| (key == "Trigger").then_some(value.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            triggers,
            [
                "Every 30s",
                "On runner completion",
                "Manual refresh",
                "Manual action"
            ]
        );
        assert!(
            review
                .details
                .iter()
                .any(|(key, value)| key == "Widget · Run" && value == r#"["cargo","check"]"#)
        );
    }
}
