use super::*;
use crate::settings::{Control, SettingsAction, SettingsDialog};
use daedalus_proto::workbench::{NamedSource, WidgetSource, WidgetTemplate, WorkspaceDocument};

fn snapshot(revision: u64, ids: &[&str]) -> WorkbenchState {
    let mut document = WorkspaceDocument::new("Workbench".into());
    document.widgets = ids
        .iter()
        .map(|id| WidgetInstance {
            id: (*id).into(),
            name: (*id).into(),
            pinned: true,
            template: None,
            definition: Some(WidgetDefinition::default()),
        })
        .collect();
    WorkbenchState {
        revision,
        document,
        repo_baseline: None,
    }
}

#[test]
fn decompose_widget_remove_keeps_its_target_across_snapshot_changes() {
    let mut dialog = SettingsDialog::default();
    dialog.begin(vec![]);
    dialog.widget_workspace = Some(WorkspaceId::new());
    dialog.open_custom_page();
    dialog.custom.accept_snapshot(snapshot(1, &["a", "b"]));
    dialog.activate(Control::CustomNext);
    dialog.custom.accept_snapshot(snapshot(2, &["c", "a", "b"]));

    let SettingsAction::Workbench { request, .. } = dialog.activate(Control::CustomRemove) else {
        panic!("expected widget removal");
    };
    let ClientMessage::UpdateWorkbench {
        revision, document, ..
    } = *request
    else {
        panic!("expected workbench update");
    };
    assert_eq!(revision, 2);
    assert_eq!(
        document
            .widgets
            .iter()
            .map(|widget| widget.id.as_str())
            .collect::<Vec<_>>(),
        ["c", "a"]
    );

    dialog.custom.accept_snapshot(snapshot(3, &["a"]));
    assert!(dialog.custom.selected().is_none());
    assert!(matches!(
        dialog.activate(Control::CustomRemove),
        SettingsAction::None
    ));
    dialog.activate(Control::CustomNext);
    assert_eq!(dialog.custom.selected().unwrap().id, "a");
}

#[test]
fn decompose_widget_metadata_preserves_template_inheritance() {
    let mut state = snapshot(1, &["widget"]);
    let definition = WidgetDefinition {
        sources: vec![NamedSource {
            id: "main".into(),
            source: WidgetSource::Published {
                key: "before".into(),
            },
        }],
        ..Default::default()
    };
    state.document.templates.push(WidgetTemplate {
        id: "shared".into(),
        name: "Shared".into(),
        definition,
    });
    let widget = &mut state.document.widgets[0];
    widget.template = Some("shared".into());
    widget.definition = None;
    let mut editor = WidgetEditor::default();
    editor.accept_snapshot(state);
    editor.edit();
    let draft = editor.draft.as_mut().unwrap();
    draft.widget.name = "Renamed".into();
    draft.widget.pinned = false;

    let ClientMessage::UpdateWorkbench { mut document, .. } =
        editor.save(WorkspaceId::new()).unwrap()
    else {
        panic!("expected workbench update");
    };
    assert_eq!(document.widgets[0].name, "Renamed");
    assert!(!document.widgets[0].pinned);
    assert!(document.widgets[0].definition.is_none());
    let changed = &mut document.templates[0].definition;
    changed.sources[0].source = WidgetSource::Published {
        key: "changed".into(),
    };
    assert_eq!(
        document.widget_definition(&document.widgets[0]),
        Some(&document.templates[0].definition)
    );

    let draft = editor.draft.as_mut().unwrap();
    draft.publish_key = "override".into();
    draft.change_source = true;
    let ClientMessage::UpdateWorkbench { document, .. } = editor.save(WorkspaceId::new()).unwrap()
    else {
        panic!("expected workbench update");
    };
    let source = &document.widgets[0].definition.as_ref().unwrap().sources[0].source;
    assert!(matches!(source, WidgetSource::Published { key } if key == "override"));
}
