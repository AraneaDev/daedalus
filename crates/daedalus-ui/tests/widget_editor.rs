use daedalus_proto::workbench::{
    CommandOutput, NamedAction, NamedSource, RefreshPolicy, WidgetAction, WidgetDefinition,
    WidgetElement, WidgetField, WidgetInstance, WidgetResult, WidgetSource, WidgetStyle,
    WorkbenchState, WorkspaceCommand, WorkspaceDocument,
};
use daedalus_proto::{ClientMessage, StatusLevel, WorkspaceId};
use daedalus_ui::settings::{Control::*, SettingsAction, SettingsDialog};
use daedalus_ui::widget_editor::WidgetEditor;

fn editor(widget: WidgetInstance) -> WidgetEditor {
    let mut document = WorkspaceDocument::new("Editor".into());
    document.widgets.push(widget);
    let mut editor = WidgetEditor::default();
    editor.state = Some(WorkbenchState {
        revision: 5,
        document,
        repo_baseline: None,
    });
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

#[test]
fn settings_saves_ordered_elements_through_advanced_and_template_edits() {
    let mut dialog = SettingsDialog::default();
    dialog.custom_supported = true;
    dialog.widget_workspace = Some(WorkspaceId::new());
    dialog.custom.state = Some(WorkbenchState {
        revision: 1,
        document: WorkspaceDocument::new("Editor".into()),
        repo_baseline: None,
    });
    dialog.open_custom_page();
    dialog.activate(CustomNew);
    dialog.set_value(CustomPublish, "checks");
    dialog.set_value(CustomUrl, "https://example.invalid");
    assert!(
        dialog
            .control_order()
            .any(|control| control == CustomElements)
    );
    dialog.activate(CustomElements);
    dialog.activate(CustomElementAddStatus);
    dialog.activate(CustomElementAddLabel);
    dialog.set_value(CustomElementText, "Build checks");
    dialog.activate(CustomElementEarlier);
    dialog.activate(CustomElementAddCounter);
    dialog.set_value(CustomElementField, "passed");
    dialog.activate(CustomElementAddProgress);
    dialog.set_value(CustomElementField, "passed");
    dialog.set_value(CustomElementCaption, "Coverage");
    for invalid_maximum in ["0", "9223372036854775808"] {
        dialog.set_value(CustomElementMax, invalid_maximum);
        dialog.activate(CustomElementsBack);
        dialog.set_value(CustomName, "Build");
        assert!(matches!(dialog.activate(CustomSave), SettingsAction::None));
        assert!(dialog.error.as_deref().unwrap().contains("maximum"));
        dialog.activate(CustomElements);
        dialog.activate(CustomElementNext);
        dialog.activate(CustomElementNext);
        dialog.activate(CustomElementNext);
    }
    dialog.set_value(CustomElementMax, "20");
    dialog.activate(CustomElementAddButton);
    dialog.activate(CustomElementAddLabel);
    dialog.activate(CustomElementRemove);
    dialog.activate(CustomElementsBack);
    dialog.set_value(CustomName, "");
    dialog.activate(CustomAdvanced);
    dialog.activate(CustomApplyDefinition);
    dialog.set_value(CustomName, "Build");

    let SettingsAction::Workbench { request, .. } = dialog.activate(CustomSaveTemplate) else {
        panic!("expected the template library update")
    };
    let ClientMessage::UpdateWidgetTemplates { templates, .. } = *request else {
        panic!("expected widget templates")
    };
    let definition = &templates[0].definition;
    let field = WidgetField {
        source: "main".into(),
        field: "passed".into(),
        label: None,
    };
    assert_eq!(
        definition.display.elements,
        [
            WidgetElement::Label {
                text: "Build checks".into()
            },
            WidgetElement::Status,
            WidgetElement::Counter {
                field: field.clone()
            },
            WidgetElement::Progress {
                field: WidgetField {
                    label: Some("Coverage".into()),
                    ..field
                },
                max: 20,
            },
            WidgetElement::Button {
                action: definition.actions[0].id.clone()
            },
        ]
    );
    assert_eq!(
        definition.sources[0].source,
        WidgetSource::Published {
            key: "checks".into()
        }
    );
    let SettingsAction::Workbench { request, .. } = dialog.activate(CustomSave) else {
        panic!("expected the workspace update")
    };
    let ClientMessage::UpdateWorkbench { document, .. } = *request else {
        panic!("expected a saved workspace")
    };
    assert_eq!(document.widgets[0].definition.as_ref(), Some(definition));

    dialog.custom.library.templates = templates;
    dialog.activate(CustomBack);
    dialog.activate(CustomNew);
    dialog.activate(CustomTemplate);
    let SettingsAction::Workbench { request, .. } = dialog.activate(CustomSave) else {
        panic!("expected the workspace update")
    };
    let ClientMessage::UpdateWorkbench { document, .. } = *request else {
        panic!("expected a saved workspace")
    };
    assert!(document.widgets[0].definition.is_none());
    assert_eq!(
        document.widget_definition(&document.widgets[0]),
        Some(&dialog.custom.library.templates[0].definition)
    );
}

#[test]
fn appearance_form_validates_and_preserves_widget_edits() {
    fn control(dialog: &SettingsDialog, label: &str) -> daedalus_ui::settings::Control {
        dialog
            .control_order()
            .find(|&control| dialog.label(control).trim_end() == label)
            .unwrap_or_else(|| panic!("missing {label} in {}", dialog.heading()))
    }

    let mut dialog = SettingsDialog::default();
    dialog.custom_supported = true;
    dialog.widget_workspace = Some(WorkspaceId::new());
    dialog.custom.state = Some(WorkbenchState {
        revision: 1,
        document: WorkspaceDocument::new("Editor".into()),
        repo_baseline: None,
    });
    dialog.open_custom_page();
    dialog.activate(CustomNew);
    dialog.set_value(CustomName, "Build");
    let appearance = control(&dialog, "Appearance");
    dialog.activate(appearance);

    let accent = control(&dialog, "Accent (#RRGGBB)");
    let width = control(&dialog, "Width (px)");
    let icon = control(&dialog, "Icon (optional)");
    let style = control(&dialog, "Style · Badge ↻");
    let density = control(&dialog, "Density · Standard ↻");
    let surface = control(&dialog, "Surface · Automatic ↻");
    let status = control(&dialog, "Status · Light + text ↻");
    let back = control(&dialog, "Back to form");
    dialog.set_value(accent, "purple");
    dialog.activate(back);
    assert!(matches!(dialog.activate(CustomSave), SettingsAction::None));
    assert_eq!(dialog.heading(), "Widget appearance");
    assert_eq!(dialog.focus, accent);
    assert_eq!(dialog.invalid_field, Some(accent));
    assert!(dialog.error.as_deref().unwrap().contains("#RRGGBB"));

    dialog.set_value(accent, "#3e62ff");
    for invalid in ["109", "1025", "wide"] {
        dialog.set_value(width, invalid);
        assert!(matches!(dialog.activate(CustomSave), SettingsAction::None));
        assert_eq!(dialog.focus, width);
        assert!(dialog.error.as_deref().unwrap().contains("110–1024"));
    }

    dialog.set_value(width, "232");
    dialog.set_value(icon, "◆");
    dialog.activate(style);
    dialog.activate(density);
    dialog.activate(surface);
    dialog.activate(status);
    dialog.activate(back);
    dialog.activate(CustomAdvanced);
    dialog.activate(CustomApplyDefinition);

    let SettingsAction::Workbench { request, .. } = dialog.activate(CustomPreview) else {
        panic!("expected widget preview")
    };
    let ClientMessage::PreviewWidget { definition, .. } = *request else {
        panic!("expected a widget definition")
    };
    assert_eq!(definition.display.icon.as_deref(), Some("◆"));
    assert_eq!(definition.display.style, WidgetStyle::Button);
    assert_eq!(
        serde_json::to_value(&definition.display.appearance).unwrap(),
        serde_json::json!({
            "accent": "#3e62ff",
            "width": 232,
            "density": "spacious",
            "surface": "flat",
            "status": "light"
        })
    );
    let SettingsAction::Workbench { request, .. } = dialog.activate(CustomSaveTemplate) else {
        panic!("expected a template library update")
    };
    let ClientMessage::UpdateWidgetTemplates { templates, .. } = *request else {
        panic!("expected widget templates")
    };
    assert_eq!(templates[0].definition, definition);

    dialog.custom.library.templates = templates;
    dialog.activate(CustomBack);
    dialog.activate(CustomNew);
    dialog.activate(CustomTemplate);
    dialog.activate(appearance);
    assert_eq!(dialog.value(accent), Some("#3e62ff"));
    assert_eq!(dialog.value(width), Some("232"));
    assert_eq!(dialog.value(icon), Some("◆"));
    dialog.set_value(accent, "");
    dialog.set_value(width, "");
    dialog.set_value(icon, "");
    let SettingsAction::Workbench { request, .. } = dialog.activate(CustomSave) else {
        panic!("expected the workspace update")
    };
    let ClientMessage::UpdateWorkbench { document, .. } = *request else {
        panic!("expected a saved workspace")
    };
    let display = &document.widgets[0].definition.as_ref().unwrap().display;
    assert_eq!(display.icon, None);
    assert_eq!(display.appearance.accent, None);
    assert_eq!(display.appearance.width, None);
    assert_eq!(
        display.appearance.density,
        definition.display.appearance.density
    );
}

#[test]
fn widget_details_show_composed_values_and_referenced_actions() {
    let mut definition = WidgetDefinition {
        actions: (0..14)
            .map(|index| NamedAction {
                id: format!("action-{index}"),
                label: format!("Action {index}"),
                action: WidgetAction::Diff,
            })
            .collect(),
        ..Default::default()
    };
    let field = WidgetField {
        source: "checks".into(),
        field: "passed".into(),
        label: Some("Tests".into()),
    };
    definition.display.elements = vec![
        WidgetElement::Label {
            text: "Build checks".into(),
        },
        WidgetElement::Status,
        WidgetElement::Counter {
            field: field.clone(),
        },
        WidgetElement::Progress { field, max: 20 },
        WidgetElement::Button {
            action: "action-5".into(),
        },
    ];
    definition
        .display
        .elements
        .extend((6..14).map(|index| WidgetElement::Button {
            action: format!("action-{index}"),
        }));
    let mut dialog = SettingsDialog::default();
    dialog.custom_supported = true;
    dialog.widget_workspace = Some(WorkspaceId::new());
    dialog.custom = editor(widget(definition));
    dialog.open_custom_page();
    assert!(dialog.custom.open_details(0));
    dialog.custom.preview = Some(WidgetResult {
        label: "Ready".into(),
        level: StatusLevel::Ok,
        details: vec![("Log".into(), "Done".into())],
        element_values: [("checks".into(), [("passed".into(), "7".into())].into())].into(),
    });
    let controls: Vec<_> = dialog.control_order().collect();
    let lines: Vec<_> = controls
        .iter()
        .filter(|control| matches!(control, CustomDetail(_)))
        .map(|control| dialog.label(*control))
        .collect();
    assert_eq!(
        lines,
        [
            "Build checks",
            "Status · Ready (OK)",
            "Tests · 7",
            "Tests · 7 / 20",
            "Log · Done"
        ]
    );
    assert!(controls.contains(&CustomViewAction(5)));
    assert!(!controls.contains(&CustomViewAction(4)));
    let SettingsAction::Workbench { request, .. } = dialog.activate(CustomViewAction(5)) else {
        panic!("expected a widget action")
    };
    assert!(
        matches!(*request, ClientMessage::InvokeWidgetAction { action, .. } if action == "action-5")
    );
    assert!(controls.contains(&CustomActionNext));
    dialog.activate(CustomActionNext);
    assert!(
        dialog
            .control_order()
            .any(|control| control == CustomViewAction(13))
    );
    assert!(
        dialog
            .control_order()
            .any(|control| control == CustomViewAction(0))
    );
    assert!(!dialog.enabled(CustomActionNext));
}

#[test]
fn widget_details_return_to_first_page_when_composition_shrinks() {
    let mut definition = WidgetDefinition::default();
    definition.display.elements = (1..=9)
        .map(|index| WidgetElement::Label {
            text: format!("Element {index}"),
        })
        .collect();
    let mut dialog = SettingsDialog::default();
    dialog.custom_supported = true;
    dialog.widget_workspace = Some(WorkspaceId::new());
    dialog.custom = editor(widget(definition));
    dialog.open_custom_page();
    assert!(dialog.custom.open_details(0));
    dialog.activate(CustomDetailNext);
    assert!(
        dialog
            .control_order()
            .any(|control| control == CustomDetail(8))
    );

    let mut snapshot = dialog.custom.state.clone().unwrap();
    snapshot.revision += 1;
    dialog.custom.accept_snapshot(snapshot.clone());
    assert!(
        dialog
            .control_order()
            .any(|control| control == CustomDetail(8))
    );

    snapshot.revision += 1;
    snapshot.document.widgets[0]
        .definition
        .as_mut()
        .unwrap()
        .display
        .elements
        .truncate(1);
    dialog.custom.accept_snapshot(snapshot);
    assert!(
        dialog
            .control_order()
            .any(|control| control == CustomDetail(0))
    );
    assert_eq!(dialog.label(CustomDetail(0)), "Element 1");
}

#[test]
fn widget_details_keep_a_visible_page_when_cached_results_shrink() {
    let mut dialog = SettingsDialog::default();
    dialog.custom_supported = true;
    dialog.widget_workspace = Some(WorkspaceId::new());
    dialog.custom = editor(widget(WidgetDefinition::default()));
    dialog.open_custom_page();
    assert!(dialog.custom.open_details(0));

    let result = |line_count| WidgetResult {
        label: "Ready".into(),
        level: StatusLevel::Ok,
        details: vec![(
            "Log".into(),
            (1..=line_count)
                .map(|index| format!("Line {index}"))
                .collect::<Vec<_>>()
                .join("\n"),
        )],
        element_values: Default::default(),
    };
    dialog.update_widget_detail_result(result(17));
    dialog.activate(CustomDetailNext);
    dialog.activate(CustomDetailNext);
    assert!(
        dialog
            .control_order()
            .any(|control| control == CustomDetail(16))
    );

    dialog.update_widget_detail_result(result(10));
    assert!(
        dialog
            .control_order()
            .any(|control| control == CustomDetail(8))
    );
    assert_eq!(dialog.label(CustomDetail(8)), "Line 9");
    assert!(!dialog.enabled(CustomDetailNext));

    dialog.update_widget_detail_result(result(1));
    assert!(
        dialog
            .control_order()
            .any(|control| control == CustomDetail(0))
    );
    assert_eq!(dialog.label(CustomDetail(0)), "Log · Line 1");
    assert!(!dialog.enabled(CustomDetailPrev));
}
