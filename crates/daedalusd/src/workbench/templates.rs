use daedalus_proto::PaneRole;
use daedalus_proto::workbench::{
    NamedAction, NamedSource, WidgetAction, WidgetDefinition, WidgetDisplay, WidgetField,
    WidgetSource, WidgetStyle, WidgetTemplate, WorkspaceCommand,
};

pub(super) fn builtins() -> Vec<WidgetTemplate> {
    let runner = |id, name, fields: &[&str]| {
        template(
            id,
            name,
            WidgetSource::Runner { assignment: None },
            fields,
            WidgetAction::Terminal {
                role: PaneRole::Runner,
            },
        )
    };
    let cargo = |id, name, subcommand: &str, fields: &[&str]| {
        let mut template = runner(id, name, fields);
        template.definition.display.style = WidgetStyle::Button;
        template.definition.display.primary_action = Some("run".into());
        template.definition.actions.insert(
            0,
            NamedAction {
                id: "run".into(),
                label: format!("Run {name}"),
                action: WidgetAction::Command {
                    command: WorkspaceCommand {
                        argv: vec!["cargo".into(), subcommand.into(), "--workspace".into()],
                        env: Vec::new(),
                        timeout_seconds: None,
                    },
                    run_in_runner: true,
                },
            },
        );
        template
    };
    vec![
        template(
            "git-worktree",
            "Git Worktree",
            WidgetSource::Git,
            &["branch", "changes", "upstream"],
            WidgetAction::Diff,
        ),
        cargo(
            "cargo-test",
            "Cargo Test",
            "test",
            &["phase", "passed", "failed"],
        ),
        cargo(
            "cargo-check",
            "Cargo Check",
            "check",
            &["phase", "warnings"],
        ),
        runner("clippy", "Clippy", &["phase", "warnings"]),
        template(
            "dev-server",
            "Dev Server",
            WidgetSource::Port {
                port: None,
                service: None,
            },
            &["label"],
            WidgetAction::Logs {
                role: PaneRole::Service,
            },
        ),
        template(
            "docker-compose",
            "Docker Compose",
            WidgetSource::Container { name: None },
            &["label"],
            WidgetAction::Logs {
                role: PaneRole::Service,
            },
        ),
        template(
            "custom-status",
            "Custom Status",
            WidgetSource::Published {
                key: "custom".into(),
            },
            &["label"],
            WidgetAction::Terminal {
                role: PaneRole::Agent,
            },
        ),
    ]
}

pub(super) fn refresh_cargo_templates(templates: &mut [WidgetTemplate]) {
    for latest in builtins()
        .into_iter()
        .filter(|template| matches!(template.id.as_str(), "cargo-test" | "cargo-check"))
    {
        if let Some(saved) = templates
            .iter_mut()
            .find(|template| template.id == latest.id)
        {
            let mut legacy = latest.clone();
            legacy.definition.display.primary_action = None;
            legacy.definition.display.style = WidgetStyle::Badge;
            legacy
                .definition
                .actions
                .retain(|action| action.id != "run");
            if *saved == legacy {
                *saved = latest;
            }
        }
    }
}

fn template(
    id: &str,
    name: &str,
    source: WidgetSource,
    summary: &[&str],
    action: WidgetAction,
) -> WidgetTemplate {
    let fields = summary
        .iter()
        .map(|field| WidgetField {
            source: "main".into(),
            field: (*field).into(),
            label: match *field {
                "passed" | "failed" | "warnings" => Some((*field).into()),
                _ => None,
            },
        })
        .collect::<Vec<_>>();
    WidgetTemplate {
        id: id.into(),
        name: name.into(),
        definition: WidgetDefinition {
            sources: vec![NamedSource {
                id: "main".into(),
                source,
            }],
            display: WidgetDisplay {
                summary: fields.clone(),
                details: fields,
                ..WidgetDisplay::default()
            },
            actions: vec![NamedAction {
                id: "open".into(),
                label: if matches!(action, WidgetAction::Diff) {
                    "View diff".into()
                } else {
                    "Open terminal".into()
                },
                action,
            }],
            ..WidgetDefinition::default()
        },
    }
}
