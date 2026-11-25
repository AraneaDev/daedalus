use daedalus_proto::PaneRole;
use daedalus_proto::workbench::{
    NamedAction, NamedSource, WidgetAction, WidgetDefinition, WidgetDisplay, WidgetField,
    WidgetSource, WidgetTemplate,
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
    vec![
        template(
            "git-worktree",
            "Git Worktree",
            WidgetSource::Git,
            &["branch", "changes", "upstream"],
            WidgetAction::Diff,
        ),
        runner("cargo-test", "Cargo Test", &["phase", "passed", "failed"]),
        runner("cargo-check", "Cargo Check", &["phase", "warnings"]),
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
