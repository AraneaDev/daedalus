use daedalus_proto::workbench::{
    CommandOutput, NamedAction, NamedSource, RefreshPolicy, WidgetAction, WidgetDefinition,
    WidgetField, WidgetInstance, WidgetSource, WorkbenchState, WorkspaceCommand, WorkspaceDocument,
};
use daedalus_proto::{ClientMessage, WorkspaceId};
use daedalus_ui::widget_editor::WidgetEditor;

fn editor(widget: WidgetInstance) -> WidgetEditor {
    let mut document = WorkspaceDocument::new("Editor".into());
    document.widgets.push(widget);
    let mut editor = WidgetEditor {
        state: Some(WorkbenchState {
            revision: 5,
            document,
            repo_baseline: None,
        }),
        ..Default::default()
    };
    editor.edit();
    editor
}

fn widget(definition: WidgetDefinition) -> WidgetInstance {
    WidgetInstance {
        id: "check".into(),
        name: "Check".into(),
        pinned: true,
        template: None,
        definition: Some(definition),
    }
}

#[test]
fn command_form_preserves_advanced_fields_and_rejects_invalid_input() {
    let command = WorkspaceCommand {
        argv: vec!["cargo".into(), "check".into()],
        env: vec!["CARGO_HOME".into()],
        timeout_seconds: Some(12),
    };
    let definition = WidgetDefinition {
        sources: vec![NamedSource {
            id: "primary".into(),
            source: WidgetSource::Command {
                command,
                refresh: RefreshPolicy {
                    interval_seconds: Some(2),
                    on_runner_completion: true,
                },
                output: CommandOutput::Json,
            },
        }],
        ..Default::default()
    };
    let mut editor = editor(widget(definition));
    let draft = editor.draft.as_mut().unwrap();
    assert_eq!(draft.executable, "cargo");
    assert_eq!(draft.arguments, r#"["check"]"#);
    assert_eq!(draft.interval, "2");
    assert_eq!(draft.output, CommandOutput::Json);
    draft.change_source = true;
    draft.arguments = "check".into();
    assert!(draft.build().unwrap_err().contains("JSON array"));
    draft.arguments = r#"["test", "--locked"]"#.into();
    draft.interval = "0".into();
    assert!(draft.build().unwrap_err().contains("positive"));
    draft.interval.clear();
    draft.widget.name = "  ".into();
    assert!(draft.build().unwrap_err().contains("Enter a name"));
    draft.widget.name = "  Workspace tests  ".into();
    draft.output = CommandOutput::WidgetResult;
    let edited = draft.build().unwrap();
    assert_eq!(edited.name, "Workspace tests");
    let definition = edited.definition.unwrap();
    assert_eq!(definition.sources[0].id, "primary");
    assert!(
        matches!(&definition.sources[0].source, WidgetSource::Command { command, refresh, output }
        if command.argv == ["cargo", "test", "--locked"]
            && command.env == ["CARGO_HOME"] && command.timeout_seconds == Some(12)
            && refresh.on_runner_completion && refresh.interval_seconds.is_none()
            && *output == CommandOutput::WidgetResult)
    );
}

#[test]
fn published_source_fields_and_url_edits_preserve_other_actions() {
    let definition = WidgetDefinition {
        sources: vec![NamedSource {
            id: "primary".into(),
            source: WidgetSource::Published { key: "old".into() },
        }],
        actions: vec![
            NamedAction {
                id: "diff".into(),
                label: "Diff".into(),
                action: WidgetAction::Diff,
            },
            NamedAction {
                id: "link".into(),
                label: "Link".into(),
                action: WidgetAction::OpenUrl {
                    url: "https://example.invalid/old".into(),
                },
            },
        ],
        ..Default::default()
    };
    let mut editor = editor(widget(definition));
    let draft = editor.draft.as_mut().unwrap();
    assert_eq!(draft.publish_key, "old");
    assert_eq!(draft.url, "https://example.invalid/old");
    draft.publish_key = " checks ".into();
    draft.change_source = true;
    draft.summary = "primary".into();
    assert!(draft.build().unwrap_err().contains("source:field"));
    draft.summary = "primary:label, primary:level".into();
    draft.details = "primary:detail".into();
    draft.url = "https://example.invalid/new".into();
    let edited = draft.build().unwrap().definition.unwrap();
    assert_eq!(
        edited.sources[0].source,
        WidgetSource::Published {
            key: "checks".into()
        }
    );
    assert_eq!(
        edited.display.summary[0],
        WidgetField {
            source: "primary".into(),
            field: "label".into(),
            label: None
        }
    );
    assert_eq!(edited.display.details[0].field, "detail");
    assert_eq!(edited.actions[1].id, "link");
    assert_eq!(
        edited.actions[1].action,
        WidgetAction::OpenUrl {
            url: "https://example.invalid/new".into()
        }
    );
    draft.url.clear();
    let ClientMessage::UpdateWorkbench {
        revision, document, ..
    } = editor.save(WorkspaceId::new()).unwrap()
    else {
        panic!("Expected a saved workspace")
    };
    assert_eq!(revision, 5);
    let saved = document.widgets[0].definition.as_ref().unwrap();
    assert_eq!(saved.actions.len(), 1);
    assert_eq!(saved.actions[0].action, WidgetAction::Diff);
}
