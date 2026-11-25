use super::*;
use daedalus_proto::workbench::{
    WidgetDefinition, WidgetLibrary, WidgetTemplate, WorkbenchState, WorkspaceDocument,
};
use daedalus_proto::{ClientMessage, ResponsePayload};
use daedalus_ui::settings::Control;

fn activate(w: &mut Workbench, control: Control) {
    let action = w.app.state.settings.activate(control);
    w.app.handle_settings_action(action);
}

fn command(w: &Workbench) -> (WorkbenchOperation, ClientMessage) {
    match w.command() {
        UiCommand::Workbench {
            operation, request, ..
        } => (operation, *request),
        unexpected => panic!("Expected workbench command, got {unexpected:?}"),
    }
}

fn reply(
    w: &mut Workbench,
    workspace_id: WorkspaceId,
    operation: WorkbenchOperation,
    payload: ResponsePayload,
) {
    w.app.handle_daemon_event(UiEvent::Workbench {
        workspace_id,
        operation,
        response: Box::new(Ok(payload)),
    });
}

fn accept_edit(
    w: &mut Workbench,
    workspace_id: WorkspaceId,
    expected: WorkbenchOperation,
) -> WorkspaceDocument {
    let (
        operation,
        ClientMessage::UpdateWorkbench {
            revision, document, ..
        },
    ) = command(w)
    else {
        panic!("Expected widget update");
    };
    assert_eq!(operation, expected);
    reply(
        w,
        workspace_id,
        operation,
        ResponsePayload::Workbench(WorkbenchState {
            revision: revision + 1,
            document: document.clone(),
            repo_baseline: None,
        }),
    );
    w.commands.try_iter().for_each(drop);
    document
}

#[test]
fn templates_can_be_saved_applied_reordered_and_shared_from_the_editor() {
    let mut w = Workbench::new();
    daemon_capabilities(&mut w, true);
    let ws = workspace("Widgets");
    let id = ws.id;
    w.app.state.set_workspaces(vec![ws]);
    w.app
        .handle_daemon_event(UiEvent::WorkbenchCapability(true));
    w.app.open_workspace_settings();
    let (read, _) = command(&w);
    let (templates_request, _) = command(&w);
    let make_template = |id: &str, name: &str| WidgetTemplate {
        id: id.into(),
        name: name.into(),
        definition: WidgetDefinition::default(),
    };
    let templates = vec![
        make_template("first", "First"),
        make_template("second", "Second"),
    ];
    let library = WidgetLibrary {
        revision: 1,
        templates,
    };
    reply(
        &mut w,
        id,
        read,
        ResponsePayload::Workbench(WorkbenchState {
            revision: 2,
            document: WorkspaceDocument::new("Widgets".into()),
            repo_baseline: None,
        }),
    );
    reply(
        &mut w,
        id,
        templates_request,
        ResponsePayload::WidgetLibrary(library.clone()),
    );
    w.commands.try_iter().for_each(drop);

    activate(&mut w, Control::CustomWidgetsPage);
    activate(&mut w, Control::CustomNew);
    activate(&mut w, Control::CustomTemplate);
    assert!(
        w.app
            .state
            .settings
            .label(Control::CustomTemplate)
            .contains("First")
    );
    activate(&mut w, Control::CustomTemplate);
    assert!(
        w.app
            .state
            .settings
            .label(Control::CustomTemplate)
            .contains("Second")
    );
    w.app
        .state
        .settings
        .set_value(Control::CustomName, "Second variant");
    activate(&mut w, Control::CustomSaveTemplate);
    let (
        operation,
        ClientMessage::UpdateWidgetTemplates {
            revision,
            templates,
        },
    ) = command(&w)
    else {
        panic!("Expected template save");
    };
    assert_eq!(revision, library.revision);
    assert_eq!(templates.last().unwrap().name, "Second variant");
    reply(
        &mut w,
        id,
        operation.clone(),
        ResponsePayload::WidgetLibraryConflict(library.clone()),
    );
    assert!(
        w.app
            .state
            .settings
            .error
            .as_deref()
            .unwrap()
            .contains("library changed")
    );
    reply(
        &mut w,
        id,
        operation,
        ResponsePayload::WidgetLibrary(WidgetLibrary {
            revision: 2,
            templates,
        }),
    );
    activate(&mut w, Control::CustomPin);
    activate(&mut w, Control::CustomSave);
    let document = accept_edit(&mut w, id, WorkbenchOperation::WidgetEdit);
    assert_eq!(document.templates, [library.templates[1].clone()]);
    assert_eq!(document.widgets[0].name, "Second variant");
    assert!(!document.widgets[0].pinned);

    activate(&mut w, Control::CustomDuplicate);
    let duplicated = accept_edit(&mut w, id, WorkbenchOperation::Read);
    assert_eq!(duplicated.widgets.len(), 2);
    assert_ne!(duplicated.widgets[0].id, duplicated.widgets[1].id);
    activate(&mut w, Control::CustomNext);
    activate(&mut w, Control::CustomMove);
    let reordered = accept_edit(&mut w, id, WorkbenchOperation::Read);
    assert_eq!(reordered.widgets[0].id, duplicated.widgets[1].id);
    activate(&mut w, Control::CustomPrev);
    activate(&mut w, Control::CustomPin);
    let pinned = accept_edit(&mut w, id, WorkbenchOperation::Read);
    assert!(pinned.widgets[w.app.state.settings.custom.selected].pinned);
    activate(&mut w, Control::CustomRemove);
    let remaining = accept_edit(&mut w, id, WorkbenchOperation::Read);
    assert_eq!(remaining.widgets.len(), 1);

    activate(&mut w, Control::CustomSharing);
    let path = w.root.path().join("template.toml");
    w.app
        .state
        .settings
        .set_value(Control::CustomPath, path.to_str().unwrap());
    activate(&mut w, Control::CustomExportTemplate);
    let (export, ClientMessage::EncodeWidgetTemplate { template }) = command(&w) else {
        panic!("Expected template export");
    };
    assert_eq!(template.id, remaining.widgets[0].id);
    let text = "version = 1\n[template]\nname = 'Shared'\n";
    reply(
        &mut w,
        id,
        export,
        ResponsePayload::WorkbenchToml(text.into()),
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    activate(&mut w, Control::CustomImportTemplate);
    let (import, ClientMessage::DecodeWidgetTemplate { text: request }) = command(&w) else {
        panic!("Expected template import");
    };
    assert_eq!(request, text);
    reply(
        &mut w,
        id,
        import.clone(),
        ResponsePayload::WidgetTemplate(library.templates[0].clone()),
    );
    assert!(
        w.app
            .state
            .settings
            .error
            .as_deref()
            .unwrap()
            .contains("already exists")
    );
    let shared = make_template("shared", "Shared");
    reply(
        &mut w,
        id,
        import,
        ResponsePayload::WidgetTemplate(shared.clone()),
    );
    let (
        _,
        ClientMessage::UpdateWidgetTemplates {
            revision,
            templates,
        },
    ) = command(&w)
    else {
        panic!("Expected library update for imported file");
    };
    assert_eq!(revision, 2);
    assert_eq!(templates.last(), Some(&shared));
}
