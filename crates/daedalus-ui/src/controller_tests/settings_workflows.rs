use super::*;
use daedalus_proto::workbench::{
    ConflictChoice, MergeConflict, PaneWidgets, WorkbenchState, WorkspaceImportPreview,
};
use daedalus_proto::{ClientMessage, ResponsePayload};
use daedalus_ui::settings::Control;
use daedalus_ui::workspace_widgets::{self, WorkspaceWidgets};

fn workbench_snapshot(revision: u64) -> daedalus_proto::workbench::WorkbenchState {
    daedalus_proto::workbench::WorkbenchState {
        revision,
        document: daedalus_proto::workbench::WorkspaceDocument::new("Workbench".into()),
        repo_baseline: None,
    }
}

fn modern_client(ws: WorkspaceInfo) -> Workbench {
    let mut w = Workbench::new();
    daemon_capabilities(&mut w, true);
    w.app.state.set_workspaces(vec![ws]);
    w.app
        .handle_daemon_event(UiEvent::WorkbenchCapability(true));
    w.app.open_workspace_settings();
    w
}

fn daemon_workbench_reply(
    w: &mut Workbench,
    id: WorkspaceId,
    operation: WorkbenchOperation,
    result: Result<ResponsePayload, String>,
) {
    w.app.handle_daemon_event(UiEvent::Workbench {
        workspace_id: id,
        operation,
        response: Box::new(result),
    });
}

fn snapshot_with_runner_label(revision: u64, label: &str) -> WorkbenchState {
    let mut state = workbench_snapshot(revision);
    let mut pane_widgets = PaneWidgets::default();
    pane_widgets.runner.label = Some(label.into());
    state.document.pane_widgets = Some(pane_widgets);
    state
}

fn apply_control(w: &mut Workbench, control: Control) {
    let action = w.app.state.settings.activate(control);
    w.app.handle_settings_action(action);
}

fn timed_widget(name: &str) -> daedalus_proto::workbench::WidgetInstance {
    use daedalus_proto::workbench::{
        CommandOutput, NamedSource, RefreshPolicy, WidgetDefinition, WidgetInstance, WidgetSource,
        WorkspaceCommand,
    };
    WidgetInstance {
        id: "timer".into(),
        name: name.into(),
        pinned: true,
        template: None,
        definition: Some(WidgetDefinition {
            sources: vec![NamedSource {
                id: "heartbeat".into(),
                source: WidgetSource::Command {
                    command: WorkspaceCommand {
                        argv: vec!["cargo".into(), "--argument with spaces".into()],
                        env: vec!["TEST_OPTIONS".into()],
                        timeout_seconds: Some(20),
                    },
                    refresh: RefreshPolicy {
                        interval_seconds: Some(30),
                        on_runner_completion: true,
                    },
                    output: CommandOutput::ExitStatus,
                },
            }],
            ..Default::default()
        }),
    }
}

#[test]
fn late_widget_test_cannot_populate_the_next_draft() {
    use daedalus_proto::{StatusLevel, workbench::WidgetResult};

    let ws = workspace("Preview isolation");
    let id = ws.id;
    let mut w = modern_client(ws);
    daemon_workbench_reply(
        &mut w,
        id,
        WorkbenchOperation::Read,
        Ok(ResponsePayload::Workbench(workbench_snapshot(1))),
    );
    w.commands.try_iter().for_each(drop);
    apply_control(&mut w, Control::CustomWidgetsPage);
    let start_test = |w: &mut Workbench, name| {
        apply_control(w, Control::CustomNew);
        w.app.state.settings.set_value(Control::CustomName, name);
        apply_control(w, Control::CustomPreview);
        let UiCommand::Workbench { operation, .. } = w.command() else {
            panic!("widget test request");
        };
        operation
    };
    let result = |label: &str| {
        Ok(ResponsePayload::WidgetResult(WidgetResult {
            label: label.into(),
            level: StatusLevel::Ok,
            details: Vec::new(),
        }))
    };
    let first = start_test(&mut w, "First");
    apply_control(&mut w, Control::CustomBack);
    let second = start_test(&mut w, "Second");
    daemon_workbench_reply(&mut w, id, first.clone(), result("First result"));
    assert!(w.app.state.settings.custom.preview.is_none());
    daemon_workbench_reply(&mut w, id, first, Err("Old error".into()));
    assert!(w.app.state.settings.error.is_none());
    daemon_workbench_reply(&mut w, id, second, result("Second result"));
    assert_eq!(
        w.app.state.settings.custom.preview.as_ref().unwrap().label,
        "Second result"
    );
    apply_control(&mut w, Control::CustomPreview);
    let UiCommand::Workbench { operation, .. } = w.command() else {
        panic!("expected repeat preview");
    };
    w.app
        .state
        .settings
        .set_value(Control::CustomExecutable, "cargo");
    daemon_workbench_reply(&mut w, id, operation, result("Old draft definition"));
    assert!(w.app.state.settings.custom.preview.is_none());
}

fn save_runner_label(w: &mut Workbench, label: &str) {
    apply_control(w, Control::WidgetsPage);
    w.app.state.settings.set_value(Control::RunnerLabel, label);
    apply_control(w, Control::Save);
}

fn next_pane_save(
    w: &Workbench,
) -> (
    WorkspaceId,
    WorkbenchOperation,
    u64,
    daedalus_proto::workbench::WorkspaceDocument,
) {
    match w.command() {
        UiCommand::Workbench {
            workspace_id,
            operation: operation @ WorkbenchOperation::PaneWidgets(_),
            request,
        } => {
            let ClientMessage::UpdateWorkbench {
                revision, document, ..
            } = *request
            else {
                panic!("expected a pane widget document update");
            };
            (workspace_id, operation, revision, document)
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn modern_pane_widget_changes_wait_for_the_daemon() {
    let ws = workspace("Modern widgets");
    let id = ws.id;
    let mut w = modern_client(ws);
    daemon_workbench_reply(
        &mut w,
        id,
        WorkbenchOperation::Read,
        Ok(ResponsePayload::Workbench(workbench_snapshot(1))),
    );
    w.commands.try_iter().for_each(drop);

    save_runner_label(&mut w, "New runner");
    let (target, operation, revision, document) = next_pane_save(&w);
    assert_eq!((target, revision), (id, 1));
    assert!(w.app.state.settings.is_saving());
    assert!(!w.app.state.settings.widgets_saved);
    assert!(!w.app.widget_directory.exists());
    assert!(!w.app.state.workspace_widgets.contains_key(&id));

    let accepted = WorkbenchState {
        revision: 2,
        document,
        repo_baseline: None,
    };
    daemon_workbench_reply(
        &mut w,
        id,
        WorkbenchOperation::Read,
        Ok(ResponsePayload::Workbench(accepted.clone())),
    );
    assert!(!w.app.state.settings.widgets_saved);
    assert!(w.app.state.settings.is_saving());
    daemon_workbench_reply(
        &mut w,
        id,
        operation,
        Ok(ResponsePayload::Workbench(accepted)),
    );
    assert!(w.app.state.settings.widgets_saved);
    assert!(!w.app.state.settings.is_saving());
    assert!(!w.app.widget_directory.exists());
    assert_eq!(
        w.app.state.workspace_widgets[&id].runner.label.as_deref(),
        Some("New runner")
    );
}

#[test]
fn stale_pane_save_uses_the_other_clients_confirmed_snapshot_and_can_retry() {
    let ws = workspace("Two clients");
    let id = ws.id;
    let mut first = modern_client(ws.clone());
    let mut second = modern_client(ws);
    for client in [&mut first, &mut second] {
        daemon_workbench_reply(
            client,
            id,
            WorkbenchOperation::Read,
            Ok(ResponsePayload::Workbench(snapshot_with_runner_label(
                1, "Original",
            ))),
        );
        client.commands.try_iter().for_each(drop);
    }
    save_runner_label(&mut first, "First");
    save_runner_label(&mut second, "Second");
    let (_, first_operation, revision, document) = next_pane_save(&first);
    assert_eq!(revision, 1);
    let (_, stale_operation, revision, _) = next_pane_save(&second);
    assert_eq!(revision, 1);

    let accepted = WorkbenchState {
        revision: 2,
        document,
        repo_baseline: None,
    };
    daemon_workbench_reply(
        &mut first,
        id,
        first_operation,
        Ok(ResponsePayload::Workbench(accepted.clone())),
    );
    daemon_workbench_reply(
        &mut second,
        id,
        stale_operation,
        Ok(ResponsePayload::WorkbenchConflict(accepted)),
    );
    assert!(!second.app.state.settings.widgets_saved);
    assert!(!second.app.state.settings.is_saving());
    assert!(
        second
            .app
            .state
            .settings
            .error
            .as_deref()
            .unwrap()
            .contains("changed")
    );
    assert_eq!(
        second.app.state.workspace_widgets[&id]
            .runner
            .label
            .as_deref(),
        Some("First")
    );
    assert_eq!(
        second.app.state.settings.value(Control::RunnerLabel),
        Some("Second")
    );
    assert!(!second.app.widget_directory.exists());

    // Ignore refresh requests emitted while reconciling the conflict.
    second.commands.try_iter().for_each(drop);
    apply_control(&mut second, Control::Save);
    let (_, retry, revision, document) = next_pane_save(&second);
    assert_eq!(revision, 2);
    let accepted = WorkbenchState {
        revision: 3,
        document,
        repo_baseline: None,
    };
    daemon_workbench_reply(
        &mut second,
        id,
        retry,
        Ok(ResponsePayload::Workbench(accepted.clone())),
    );
    daemon_workbench_reply(
        &mut first,
        id,
        WorkbenchOperation::Read,
        Ok(ResponsePayload::Workbench(accepted)),
    );
    assert!(second.app.state.settings.widgets_saved);
    for client in [&first, &second] {
        assert_eq!(
            client.app.state.workspace_widgets[&id]
                .runner
                .label
                .as_deref(),
            Some("Second")
        );
        assert_eq!(
            client.app.state.settings.value(Control::RunnerLabel),
            Some("Second")
        );
    }
}

#[test]
fn failed_pane_widget_requests_and_disconnection_keep_the_draft() {
    let ws = workspace("Offline widgets");
    let id = ws.id;
    let mut w = modern_client(ws);
    w.commands.try_iter().for_each(drop);
    save_runner_label(&mut w, "Draft");
    assert!(
        w.app
            .state
            .settings
            .error
            .as_deref()
            .unwrap()
            .contains("loading")
    );
    assert!(w.idle());

    daemon_workbench_reply(
        &mut w,
        id,
        WorkbenchOperation::Read,
        Ok(ResponsePayload::Workbench(workbench_snapshot(1))),
    );
    w.commands.try_iter().for_each(drop);
    save_runner_label(&mut w, "Draft");
    let (_, operation, _, _) = next_pane_save(&w);
    daemon_workbench_reply(&mut w, id, operation, Err("disk full".into()));
    assert_eq!(w.app.state.settings.error.as_deref(), Some("disk full"));
    assert_eq!(
        w.app.state.settings.value(Control::RunnerLabel),
        Some("Draft")
    );

    apply_control(&mut w, Control::Save);
    let (_, interrupted, _, document) = next_pane_save(&w);
    w.app.handle_daemon_event(UiEvent::Disconnected);
    assert!(!w.app.state.settings.is_saving());
    assert!(
        w.app
            .state
            .settings
            .error
            .as_deref()
            .unwrap()
            .contains("Connection lost")
    );
    let accepted = WorkbenchState {
        revision: 2,
        document,
        repo_baseline: None,
    };
    daemon_workbench_reply(
        &mut w,
        id,
        interrupted,
        Ok(ResponsePayload::Workbench(accepted)),
    );
    assert!(!w.app.state.settings.widgets_saved);
    assert!(!w.app.state.workspace_widgets.contains_key(&id));

    apply_control(&mut w, Control::Save);
    assert!(
        w.app
            .state
            .settings
            .error
            .as_deref()
            .unwrap()
            .contains("Reconnect")
    );
    assert_eq!(
        w.app.state.settings.value(Control::RunnerLabel),
        Some("Draft")
    );
    assert!(!w.app.widget_directory.exists());
    assert!(w.idle());
}

#[test]
fn pane_widgets_migrate_once_and_an_import_can_restore_inherited_defaults() {
    let ws = workspace("Widget migration");
    let id = ws.id;
    let mut w = modern_client(ws);
    let mut legacy = WorkspaceWidgets::default();
    legacy.runner.label = Some("Legacy".into());
    workspace_widgets::save(&w.app.widget_directory, id, &legacy).unwrap();
    w.app.state.workspace_widgets.insert(id, legacy.clone());
    w.app.open_workspace_settings();
    w.commands.try_iter().for_each(drop);

    daemon_workbench_reply(
        &mut w,
        id,
        WorkbenchOperation::Read,
        Ok(ResponsePayload::Workbench(workbench_snapshot(0))),
    );
    let UiCommand::Workbench {
        operation: WorkbenchOperation::Migration,
        request,
        ..
    } = w.command()
    else {
        panic!("expected the legacy widget seed");
    };
    let ClientMessage::SeedLegacyWorkbench { document, .. } = *request else {
        panic!("expected a legacy workbench document");
    };
    assert_eq!(document.pane_widgets, Some(legacy.clone().into()));
    daemon_workbench_reply(
        &mut w,
        id,
        WorkbenchOperation::Read,
        Ok(ResponsePayload::Workbench(workbench_snapshot(0))),
    );
    assert!(w.idle());

    let accepted = WorkbenchState {
        revision: 1,
        document,
        repo_baseline: None,
    };
    daemon_workbench_reply(
        &mut w,
        id,
        WorkbenchOperation::Migration,
        Ok(ResponsePayload::Workbench(accepted)),
    );
    apply_control(&mut w, Control::WidgetsPage);
    assert_eq!(
        w.app.state.settings.value(Control::RunnerLabel),
        Some("Legacy")
    );
    daemon_workbench_reply(
        &mut w,
        id,
        WorkbenchOperation::Import,
        Ok(ResponsePayload::Workbench(workbench_snapshot(2))),
    );
    assert!(!w.app.state.workspace_widgets.contains_key(&id));
    assert_eq!(w.app.state.settings.value(Control::RunnerLabel), Some(""));
    assert_eq!(
        workspace_widgets::load(&w.app.widget_directory, id).unwrap(),
        Some(legacy)
    );
}

#[test]
fn same_folder_workspaces_keep_separate_confirmed_runners() {
    let mut w = Workbench::new();
    daemon_capabilities(&mut w, true);
    let a = workspace("First");
    let mut b = workspace("Second");
    b.root_dir.clone_from(&a.root_dir);
    let (a_id, b_id) = (a.id, b.id);
    w.app.state.set_workspaces(vec![a, b]);
    w.app.workbench_capable = true;
    for (id, name, executable, revision) in [
        (a_id, "First runner", "first", 1),
        (b_id, "Second runner", "second", 1),
        (a_id, "New first", "first-updated", 2),
    ] {
        w.app.state.select_workspace(id);
        let mut state = workbench_snapshot(revision);
        let mut runner = runner_entry(name);
        runner.argv = vec![executable.into()];
        state.document.assignments = vec![runner];
        daemon_workbench_reply(
            &mut w,
            id,
            WorkbenchOperation::Read,
            Ok(ResponsePayload::Workbench(state)),
        );
    }
    w.commands.try_iter().for_each(drop);
    for (id, executable) in [(b_id, "second"), (a_id, "first-updated")] {
        w.app.state.select_workspace(id);
        w.app.handle_ui_action(UiAction::RunTests);
        assert!(
            matches!(w.command(), UiCommand::LaunchAssignment { workspace_id, assignment, .. }
                if workspace_id == id && assignment.argv == [executable])
        );
    }
}

#[test]
fn unrelated_workbench_save_preserves_the_widget_draft() {
    let mut w = Workbench::new();
    let ws = workspace("Draft");
    let id = ws.id;
    w.app.state.set_workspaces(vec![ws]);
    w.app.workbench_capable = true;
    w.app.open_workspace_settings();
    w.app.workbench_response(
        id,
        WorkbenchOperation::Read,
        Ok(ResponsePayload::Workbench(workbench_snapshot(1))),
    );
    let settings = &mut w.app.state.settings;
    settings.activate(Control::CustomWidgetsPage);
    settings.activate(Control::CustomNew);
    settings.set_value(Control::CustomName, "Uncommitted widget");
    settings.set_value(Control::CustomExecutable, "true");
    w.commands.try_iter().for_each(drop);
    w.app.handle_ui_action(UiAction::ToggleZoom);
    let (layout_save, _, layout_ack) = next_layout_save(&w, id);
    for (operation, snapshot) in [
        (layout_save, layout_ack),
        (WorkbenchOperation::Migration, workbench_snapshot(3)),
    ] {
        daemon_workbench_reply(
            &mut w,
            id,
            operation,
            Ok(ResponsePayload::Workbench(snapshot)),
        );
        assert_eq!(
            w.app.state.settings.value(Control::CustomName),
            Some("Uncommitted widget")
        );
    }
    w.commands.try_iter().for_each(drop);
    apply_control(&mut w, Control::CustomSave);
    let UiCommand::Workbench {
        operation: WorkbenchOperation::WidgetEdit,
        request,
        ..
    } = w.command()
    else {
        panic!("expected widget edit");
    };
    let ClientMessage::UpdateWorkbench {
        revision: 3,
        document,
        ..
    } = *request
    else {
        panic!("expected edit based on the latest unrelated save");
    };
    let accepted = WorkbenchState {
        revision: 4,
        document,
        repo_baseline: None,
    };
    daemon_workbench_reply(
        &mut w,
        id,
        WorkbenchOperation::WidgetEdit,
        Ok(ResponsePayload::Workbench(accepted)),
    );
    assert!(w.app.state.settings.custom.draft.is_none());
}

#[test]
fn sharing_conflicts_take_both_choices_and_export_paths_stay_with_the_request() {
    let ws = workspace("Share");
    let id = ws.id;
    let mut w = modern_client(ws);
    let state = workbench_snapshot(5);
    daemon_workbench_reply(
        &mut w,
        id,
        WorkbenchOperation::Read,
        Ok(ResponsePayload::Workbench(state.clone())),
    );
    w.commands.try_iter().for_each(drop);
    apply_control(&mut w, Control::CustomWidgetsPage);
    apply_control(&mut w, Control::CustomSharing);

    // Empty source path errors before asking a daemon, and a repo preview can proceed.
    apply_control(&mut w, Control::CustomImport);
    assert!(
        w.app
            .state
            .settings
            .error
            .as_deref()
            .unwrap()
            .contains("TOML")
    );
    assert!(w.idle());
    apply_control(&mut w, Control::CustomRepo);
    let UiCommand::Workbench {
        operation, request, ..
    } = w.command()
    else {
        panic!("expected repo review");
    };
    assert!(matches!(
        *request,
        ClientMessage::PreviewWorkspaceImport { linked: true, .. }
    ));
    let mut incoming = state.document.clone();
    incoming.name = "Incoming".into();
    incoming.layout.split_view = !incoming.layout.split_view;
    incoming.assignments.push(runner_entry("Imported task"));
    let preview = WorkspaceImportPreview {
        workspace_id: id,
        revision: 5,
        current: state.document.clone(),
        incoming: incoming.clone(),
        proposed: incoming,
        conflicts: vec![
            MergeConflict {
                path: "name".into(),
                base: None,
                local: Some("Local".into()),
                incoming: Some("Incoming".into()),
            },
            MergeConflict {
                path: "layout".into(),
                base: None,
                local: Some("Split".into()),
                incoming: None,
            },
        ],
        linked: true,
    };
    daemon_workbench_reply(
        &mut w,
        id,
        operation,
        Ok(ResponsePayload::WorkspaceImportPreview(preview.clone())),
    );
    assert!(
        w.app
            .state
            .settings
            .custom
            .preview
            .as_ref()
            .unwrap()
            .details
            .iter()
            .any(|(label, _)| label == "Layout")
    );
    assert!(!w.app.state.settings.enabled(Control::CustomApplyImport));
    assert!(
        w.app
            .state
            .settings
            .label(Control::CustomResolve)
            .contains("Local")
    );
    apply_control(&mut w, Control::CustomResolve);
    let UiCommand::Workbench {
        operation, request, ..
    } = w.command()
    else {
        panic!("expected updated review");
    };
    assert!(
        matches!(*request, ClientMessage::ResolveWorkspaceImport { revision: 5, resolutions, .. }
        if resolutions.get("name") == Some(&ConflictChoice::Local))
    );
    assert!(!w.app.state.settings.enabled(Control::CustomApplyImport));
    let mut review = preview.clone();
    review.conflicts.remove(0);
    daemon_workbench_reply(
        &mut w,
        id,
        operation,
        Ok(ResponsePayload::WorkspaceImportPreview(review.clone())),
    );
    assert!(
        w.app
            .state
            .settings
            .label(Control::CustomResolveIncoming)
            .contains("Remove")
    );
    apply_control(&mut w, Control::CustomResolveIncoming);
    let UiCommand::Workbench {
        operation, request, ..
    } = w.command()
    else {
        panic!("expected completed review");
    };
    assert!(
        matches!(*request, ClientMessage::ResolveWorkspaceImport { resolutions, .. }
        if resolutions.get("layout") == Some(&ConflictChoice::Incoming))
    );
    assert!(!w.app.state.settings.enabled(Control::CustomApplyImport));
    apply_control(&mut w, Control::CustomApplyImport);
    assert!(w.idle());
    review.conflicts.clear();
    review.proposed.assignments = (0..7)
        .map(|index| runner_entry(&format!("Assignment {index}")))
        .collect();
    review
        .proposed
        .widgets
        .push(timed_widget("Reviewed command"));
    daemon_workbench_reply(
        &mut w,
        id,
        operation,
        Ok(ResponsePayload::WorkspaceImportPreview(review)),
    );
    let mut saw_reviewed_command = false;
    while w.app.state.settings.enabled(Control::CustomDetailNext) {
        apply_control(&mut w, Control::CustomDetailNext);
        saw_reviewed_command |= w
            .app
            .state
            .settings
            .control_order()
            .filter(|control| matches!(control, Control::CustomDetail(_)))
            .any(|control| {
                w.app
                    .state
                    .settings
                    .label(control)
                    .contains("Reviewed command")
            });
    }
    assert!(saw_reviewed_command);
    apply_control(&mut w, Control::CustomApplyImport);
    assert!(
        matches!(w.command(), UiCommand::Workbench { operation: WorkbenchOperation::Import, request, .. }
        if matches!(request.as_ref(), ClientMessage::ApplyWorkspaceImport { resolutions, .. }
            if resolutions.get("name") == Some(&ConflictChoice::Local)
                && resolutions.get("layout") == Some(&ConflictChoice::Incoming)))
    );
    daemon_workbench_reply(
        &mut w,
        id,
        WorkbenchOperation::Import,
        Ok(ResponsePayload::Workbench(state)),
    );
    assert!(w.app.state.settings.custom.import.is_none());
    w.commands.try_iter().for_each(drop);
    let mut exports = Vec::new();
    for file in ["first.toml", "second.toml"] {
        let path = w.root.path().join(file);
        w.app
            .state
            .settings
            .set_value(Control::CustomPath, path.to_str().unwrap());
        apply_control(&mut w, Control::CustomExport);
        let UiCommand::Workbench {
            operation, request, ..
        } = w.command()
        else {
            panic!("expected export")
        };
        assert!(
            matches!(*request, ClientMessage::ExportWorkspace { workspace_id } if workspace_id == id)
        );
        assert_eq!(operation, WorkbenchOperation::Export(path.clone()));
        exports.push((operation, path, file));
    }
    for (operation, path, file) in exports.into_iter().rev() {
        daemon_workbench_reply(
            &mut w,
            id,
            operation,
            Ok(ResponsePayload::WorkbenchToml(file.into())),
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), file);
    }
}

#[test]
fn advanced_definition_validation_keeps_focus_and_preserves_edited_actions() {
    use daedalus_proto::workbench::{NamedAction, WidgetAction, WidgetDefinition};

    let ws = workspace("Advanced editor");
    let id = ws.id;
    let mut w = modern_client(ws);
    daemon_workbench_reply(
        &mut w,
        id,
        WorkbenchOperation::Read,
        Ok(ResponsePayload::Workbench(workbench_snapshot(3))),
    );
    w.commands.try_iter().for_each(drop);
    for control in [
        Control::CustomWidgetsPage,
        Control::CustomNew,
        Control::CustomAdvanced,
    ] {
        apply_control(&mut w, control);
    }
    assert_eq!(w.app.state.settings.focus, Control::CustomDefinition);
    assert!(
        w.app
            .state
            .settings
            .control_order()
            .any(|c| c == Control::CustomDefinition)
    );
    w.app
        .state
        .settings
        .set_value(Control::CustomDefinition, "{ invalid");
    apply_control(&mut w, Control::CustomApplyDefinition);
    assert!(w.app.state.settings.error.is_some());
    assert!(w.app.state.settings.custom.advanced);
    assert!(w.idle());

    let definition = WidgetDefinition {
        actions: vec![NamedAction {
            id: "logs".into(),
            label: "Logs".into(),
            action: WidgetAction::Logs {
                role: PaneRole::Service,
            },
        }],
        ..Default::default()
    };
    w.app.state.settings.set_value(
        Control::CustomDefinition,
        &serde_json::to_string(&definition).unwrap(),
    );
    apply_control(&mut w, Control::CustomApplyDefinition);
    assert!(!w.app.state.settings.custom.advanced);
    assert_eq!(w.app.state.settings.focus, Control::CustomName);
    w.app
        .state
        .settings
        .set_value(Control::CustomName, "Diagnostics");
    apply_control(&mut w, Control::CustomPreview);
    assert!(matches!(w.command(), UiCommand::Workbench { request, .. }
        if matches!(request.as_ref(), ClientMessage::PreviewWidget { definition: sent, .. } if sent == &definition)));
}

#[test]
fn widget_detail_pages_hold_action_output_until_explicit_refresh() {
    use daedalus_proto::StatusLevel;
    use daedalus_proto::workbench::{
        NamedAction, WidgetAction, WidgetActionResult, WidgetDefinition, WidgetInstance,
        WidgetResult, WidgetSnapshot,
    };

    let ws = workspace("Widget details");
    let id = ws.id;
    let agent = ws.panes[0].id;
    let mut w = modern_client(ws);
    let mut state = workbench_snapshot(1);
    state.document.widgets.push(WidgetInstance {
        id: "runner".into(),
        name: "Diagnostics".into(),
        pinned: true,
        template: None,
        definition: Some(WidgetDefinition {
            actions: vec![NamedAction {
                id: "terminal".into(),
                label: "Terminal".into(),
                action: WidgetAction::Terminal {
                    role: PaneRole::Agent,
                },
            }],
            ..Default::default()
        }),
    });
    daemon_workbench_reply(
        &mut w,
        id,
        WorkbenchOperation::Read,
        Ok(ResponsePayload::Workbench(state)),
    );
    let result = WidgetResult {
        label: "Ready".into(),
        level: StatusLevel::Ok,
        details: vec![(
            "Log".into(),
            (1..=10)
                .map(|n| format!("line {n}"))
                .collect::<Vec<_>>()
                .join("\n"),
        )],
    };
    let results = ResponsePayload::WidgetResults(vec![WidgetSnapshot {
        id: "runner".into(),
        result: result.clone(),
    }]);
    daemon_workbench_reply(&mut w, id, WorkbenchOperation::Read, Ok(results.clone()));
    w.app.open_widget_details(Some(0));
    w.commands.try_iter().for_each(drop);
    assert_eq!(w.app.state.settings.heading(), "Diagnostics");
    assert_eq!(
        w.app.state.settings.label(Control::CustomDetail(0)),
        "Log · line 1"
    );
    apply_control(&mut w, Control::CustomDetailNext);
    assert!(
        w.app
            .state
            .settings
            .control_order()
            .any(|c| c == Control::CustomDetail(8))
    );
    assert_eq!(
        w.app.state.settings.label(Control::CustomDetail(9)),
        "line 10"
    );
    apply_control(&mut w, Control::CustomDetailPrev);
    assert!(
        w.app
            .state
            .settings
            .control_order()
            .any(|c| c == Control::CustomDetail(0))
    );

    apply_control(&mut w, Control::CustomViewAction(0));
    assert!(matches!(w.command(), UiCommand::Workbench { request, .. }
        if matches!(request.as_ref(), ClientMessage::InvokeWidgetAction { widget, action, .. }
            if widget == "runner" && action == "terminal")));
    let action_output = WidgetResult {
        label: "Detailed output".into(),
        level: StatusLevel::Info,
        details: vec![],
    };
    daemon_workbench_reply(
        &mut w,
        id,
        WorkbenchOperation::Read,
        Ok(ResponsePayload::WidgetAction(WidgetActionResult::Output(
            action_output.clone(),
        ))),
    );
    daemon_workbench_reply(&mut w, id, WorkbenchOperation::Read, Ok(results.clone()));
    assert_eq!(w.app.state.settings.custom.preview, Some(action_output));
    apply_control(&mut w, Control::CustomRefresh);
    assert!(matches!(w.command(), UiCommand::Workbench { request, .. }
        if matches!(request.as_ref(), ClientMessage::GetWidgetResults { refresh: Some(widget), .. }
            if widget == "runner")));
    daemon_workbench_reply(&mut w, id, WorkbenchOperation::Read, Ok(results));
    assert_eq!(w.app.state.settings.custom.preview, Some(result));
    daemon_workbench_reply(
        &mut w,
        id,
        WorkbenchOperation::Read,
        Ok(ResponsePayload::WidgetAction(WidgetActionResult::Pane(
            agent,
        ))),
    );
    assert!(!w.app.state.settings.open);
    assert_eq!(w.app.state.focused_pane_id(), Some(agent));
}

#[test]
fn modern_folder_updates_reload_authoritative_setups_for_each_workspace() {
    let mut w = Workbench::new();
    let mut a = workspace("Portable");
    let mut b = workspace("Unconfigured");
    let mut c = workspace("Folder default");
    b.root_dir.clone_from(&a.root_dir);
    c.root_dir.clone_from(&a.root_dir);
    a.setup = Some(vec![runner_entry("Explicit")]);
    b.setup = None;
    c.setup = Some(vec![runner_entry("Old folder default")]);

    daemon_capabilities(&mut w, true);
    w.app
        .handle_daemon_event(UiEvent::WorkbenchCapability(true));
    w.app.handle_daemon_event(UiEvent::WorkspacesLoaded(vec![
        a.clone(),
        b.clone(),
        c.clone(),
    ]));
    w.commands.try_iter().for_each(drop);
    let new_default = vec![runner_entry("New folder default")];
    w.app.handle_daemon_event(UiEvent::ProjectSetupChanged {
        root_dir: a.root_dir.clone(),
        assignments: new_default.clone(),
    });
    assert!(matches!(
        w.command(),
        UiCommand::ReconcileWorkspaceMembership
    ));
    assert_eq!(
        w.app.project_setups.load(a.id).unwrap(),
        a.setup.clone().unwrap()
    );
    assert_eq!(
        w.app.project_setups.load(c.id).unwrap(),
        c.setup.clone().unwrap()
    );

    let (a_id, b_id, c_id) = (a.id, b.id, c.id);
    c.setup = Some(new_default.clone());
    w.app
        .handle_daemon_event(UiEvent::WorkspaceMembershipLoaded(vec![a, b, c]));
    assert_eq!(
        w.app.project_setups.load(a_id).unwrap(),
        vec![runner_entry("Explicit")]
    );
    assert!(w.app.project_setups.load(b_id).unwrap().is_empty());
    assert_eq!(w.app.project_setups.load(c_id).unwrap(), new_default);
}

#[test]
fn workspace_creation_preview_stays_separate_from_existing_workspace_import() {
    use daedalus_proto::workbench::{WorkspaceCreationPreview, WorkspaceImportSource};

    let ws = workspace("Original workspace");
    let id = ws.id;
    let mut w = modern_client(ws);
    apply_control(&mut w, Control::CustomWidgetsPage);
    apply_control(&mut w, Control::CustomSharing);
    w.commands.try_iter().for_each(drop);

    let source = w.root.path().join("shared.toml");
    let root = w.root.path().join("selected");
    let content = "name = 'Shared workspace'";
    std::fs::write(&source, content).unwrap();
    w.app
        .state
        .settings
        .set_value(Control::CustomPath, source.to_str().unwrap());
    w.app
        .state
        .settings
        .set_value(Control::CustomRoot, root.to_str().unwrap());
    apply_control(&mut w, Control::CustomImport);
    let UiCommand::Workbench {
        operation, request, ..
    } = w.command()
    else {
        panic!("expected new workspace review");
    };
    assert!(
        matches!(request.as_ref(), ClientMessage::PreviewWorkspaceCreation {
            root_dir, source: WorkspaceImportSource::Text(text), linked: false,
        } if root_dir == &root && text == content)
    );

    let mut incoming = workbench_snapshot(0).document;
    incoming.name = "Shared workspace".into();
    incoming.widgets.push(timed_widget("Timer"));
    let preview = WorkspaceCreationPreview {
        root_dir: root,
        incoming,
        linked: false,
    };
    daemon_workbench_reply(
        &mut w,
        id,
        operation,
        Ok(ResponsePayload::WorkspaceCreationPreview(preview.clone())),
    );
    assert!(w.app.state.settings.custom.import.is_none());
    let rows = &w
        .app
        .state
        .settings
        .custom
        .preview
        .as_ref()
        .unwrap()
        .details;
    assert!(
        rows.iter().any(|(label, value)| label.contains("Timer")
            && value.contains(r#""--argument with spaces""#))
    );
    assert!(rows.iter().any(|(_, value)| value.contains("TEST_OPTIONS")));
    assert!(
        rows.iter()
            .any(|(_, value)| value.contains("30s") && value.contains("runner completion"))
    );
    let mut controls = crate::accessibility::TerminalControls::default();
    let tree = crate::accessibility::workbench_tree(&w.app.state, 650., 720., 1., &mut controls);
    assert!(tree.nodes.iter().any(|(_, node)| {
        node.role() == accesskit::Role::Label
            && node.label().is_some_and(|label| label.contains("Layout"))
            && !node.is_disabled()
    }));
    let replacement = w.root.path().join("different");
    w.app
        .state
        .settings
        .set_value(Control::CustomRoot, replacement.to_str().unwrap());
    assert!(!w.app.state.settings.enabled(Control::CustomApplyImport));
    assert!(w.app.state.settings.custom.creation.is_none());
    apply_control(&mut w, Control::CustomImport);
    let UiCommand::Workbench {
        operation, request, ..
    } = w.command()
    else {
        panic!("expected new destination review");
    };
    assert!(
        matches!(request.as_ref(), ClientMessage::PreviewWorkspaceCreation { root_dir, .. } if root_dir == &replacement)
    );
    let preview = WorkspaceCreationPreview {
        root_dir: replacement,
        ..preview
    };
    daemon_workbench_reply(
        &mut w,
        id,
        operation,
        Ok(ResponsePayload::WorkspaceCreationPreview(preview.clone())),
    );
    apply_control(&mut w, Control::CustomApplyImport);
    assert!(
        matches!(w.command(), UiCommand::Workbench { operation: WorkbenchOperation::Import, request, .. }
        if matches!(request.as_ref(), ClientMessage::ApplyWorkspaceCreation { preview: sent } if sent == &preview))
    );

    let mut created = workspace("Shared workspace");
    created.setup = Some(preview.incoming.assignments.clone());
    let created_id = created.id;
    daemon_workbench_reply(
        &mut w,
        id,
        WorkbenchOperation::Import,
        Ok(ResponsePayload::WorkbenchCreated(created)),
    );
    assert_eq!(w.app.state.active_workspace_id, Some(created_id));
    assert!(
        w.app
            .state
            .workspaces
            .iter()
            .any(|workspace| workspace.id == id)
    );
    assert_eq!(
        w.app.project_setups.load(created_id).unwrap(),
        preview.incoming.assignments
    );
    assert!(!w.app.state.settings.open);
}

#[test]
fn changing_the_edited_widget_does_not_rebase_its_draft() {
    use daedalus_proto::workbench::{WidgetDefinition, WidgetInstance};

    let ws = workspace("Existing edit");
    let id = ws.id;
    let mut w = modern_client(ws);
    let mut initial = workbench_snapshot(3);
    initial.document.widgets.push(WidgetInstance {
        id: "existing".into(),
        name: "Original widget".into(),
        pinned: true,
        template: None,
        definition: Some(WidgetDefinition::default()),
    });
    daemon_workbench_reply(
        &mut w,
        id,
        WorkbenchOperation::Read,
        Ok(ResponsePayload::Workbench(initial.clone())),
    );
    w.app.open_widget_details(Some(0));
    apply_control(&mut w, Control::CustomEdit);
    assert_eq!(
        w.app.state.settings.label(Control::CustomSave),
        "Save widget"
    );
    w.app
        .state
        .settings
        .set_value(Control::CustomName, "Keep local draft");
    w.commands.try_iter().for_each(drop);

    let mut concurrent = initial;
    concurrent.revision = 4;
    concurrent.document.widgets.first_mut().unwrap().name = "Changed elsewhere".into();
    daemon_workbench_reply(
        &mut w,
        id,
        WorkbenchOperation::Read,
        Ok(ResponsePayload::Workbench(concurrent)),
    );
    w.commands.try_iter().for_each(drop);
    apply_control(&mut w, Control::CustomSave);
    assert!(
        w.app
            .state
            .settings
            .error
            .as_deref()
            .unwrap()
            .contains("changed while editing")
    );
    assert_eq!(
        w.app.state.settings.value(Control::CustomName),
        Some("Keep local draft")
    );
    assert!(w.idle());
}

#[test]
fn duplicate_widget_test_keeps_the_in_flight_preview() {
    use daedalus_proto::{StatusLevel, workbench::WidgetResult};

    let ws = workspace("Preview admission");
    let id = ws.id;
    let mut w = modern_client(ws);
    daemon_workbench_reply(
        &mut w,
        id,
        WorkbenchOperation::Read,
        Ok(ResponsePayload::Workbench(workbench_snapshot(1))),
    );
    w.commands.try_iter().for_each(drop);
    apply_control(&mut w, Control::CustomWidgetsPage);
    apply_control(&mut w, Control::CustomNew);
    w.app.state.settings.set_value(Control::CustomName, "Draft");
    apply_control(&mut w, Control::CustomPreview);
    let UiCommand::Workbench { operation, .. } = w.command() else {
        panic!("preview");
    };

    apply_control(&mut w, Control::CustomPreview);
    assert!(w.idle(), "the same draft already has a preview in flight");
    daemon_workbench_reply(
        &mut w,
        id,
        operation,
        Ok(ResponsePayload::WidgetResult(WidgetResult {
            label: "Ready".into(),
            level: StatusLevel::Ok,
            details: Vec::new(),
        })),
    );
    assert_eq!(
        w.app
            .state
            .settings
            .custom
            .preview
            .as_ref()
            .map(|result| result.label.as_str()),
        Some("Ready")
    );
}

#[test]
fn busy_widget_preview_rejection_settles_only_its_request() {
    use daedalus_proto::{StatusLevel, workbench::WidgetResult};

    let ws = workspace("Preview admission");
    let id = ws.id;
    let mut w = modern_client(ws);
    daemon_workbench_reply(
        &mut w,
        id,
        WorkbenchOperation::Read,
        Ok(ResponsePayload::Workbench(workbench_snapshot(1))),
    );
    w.commands.try_iter().for_each(drop);
    apply_control(&mut w, Control::CustomWidgetsPage);
    apply_control(&mut w, Control::CustomNew);
    w.app
        .state
        .settings
        .set_value(Control::CustomName, "Original");
    apply_control(&mut w, Control::CustomPreview);
    let UiCommand::Workbench {
        operation: original,
        ..
    } = w.command()
    else {
        panic!("preview");
    };

    w.app
        .state
        .settings
        .set_value(Control::CustomName, "Edited");
    apply_control(&mut w, Control::CustomPreview);
    let UiCommand::Workbench {
        operation: edited, ..
    } = w.command()
    else {
        panic!("edited preview");
    };
    assert_ne!(edited, original);

    daemon_workbench_reply(
        &mut w,
        id,
        edited,
        Err("A widget operation is already running.".into()),
    );
    assert_eq!(
        w.app.state.settings.error.as_deref(),
        Some("A widget operation is already running.")
    );
    daemon_workbench_reply(
        &mut w,
        id,
        original,
        Ok(ResponsePayload::WidgetResult(WidgetResult {
            label: "Original".into(),
            level: StatusLevel::Ok,
            details: Vec::new(),
        })),
    );
    assert!(w.app.state.settings.custom.preview.is_none());

    apply_control(&mut w, Control::CustomPreview);
    let UiCommand::Workbench { operation, .. } = w.command() else {
        panic!("retry");
    };
    daemon_workbench_reply(
        &mut w,
        id,
        operation,
        Ok(ResponsePayload::WidgetResult(WidgetResult {
            label: "Edited".into(),
            level: StatusLevel::Ok,
            details: Vec::new(),
        })),
    );
    assert_eq!(
        w.app
            .state
            .settings
            .custom
            .preview
            .as_ref()
            .map(|result| result.label.as_str()),
        Some("Edited")
    );
}

fn layout_client(split_view: bool) -> (WorkspaceId, Workbench, WorkbenchState) {
    let ws = workspace("Layout");
    let id = ws.id;
    let mut w = modern_client(ws);
    w.app.state.settings.open = false;
    let mut initial = workbench_snapshot(1);
    initial.document.layout = daedalus_ui::layout::PaneSplits::default().portable(split_view);
    daemon_workbench_reply(
        &mut w,
        id,
        WorkbenchOperation::Read,
        Ok(ResponsePayload::Workbench(initial.clone())),
    );
    w.commands.try_iter().for_each(drop);
    (id, w, initial)
}

fn next_layout_save(w: &Workbench, id: WorkspaceId) -> (WorkbenchOperation, u64, WorkbenchState) {
    w.commands
        .try_iter()
        .find_map(|command| {
            let UiCommand::Workbench {
                operation, request, ..
            } = command
            else {
                return None;
            };
            let ClientMessage::UpdateWorkbench {
                workspace_id,
                revision,
                document,
            } = *request
            else {
                return None;
            };
            assert_eq!(workspace_id, id);
            Some((
                operation,
                revision,
                WorkbenchState {
                    revision: revision + 1,
                    document,
                    repo_baseline: None,
                },
            ))
        })
        .expect("expected layout save")
}

#[test]
fn reversing_a_layout_before_acknowledgment_preserves_the_latest_choice() {
    let (id, mut w, original) = layout_client(false);
    w.app.handle_ui_action(UiAction::ToggleZoom);
    let (first, revision, split) = next_layout_save(&w, id);
    assert_eq!(revision, 1);
    assert!(split.document.layout.split_view);

    w.app.handle_ui_action(UiAction::ToggleZoom);
    assert!(w.app.state.zoomed);
    assert!(w.idle(), "only one layout save should be in flight");
    daemon_workbench_reply(&mut w, id, first, Ok(ResponsePayload::Workbench(split)));

    assert!(
        w.app.state.zoomed,
        "the first reply undid the latest choice"
    );
    let (second, revision, focused) = next_layout_save(&w, id);
    assert_eq!(revision, 2);
    assert_eq!(focused.document.layout, original.document.layout);
    daemon_workbench_reply(&mut w, id, second, Ok(ResponsePayload::Workbench(focused)));
    assert!(w.app.state.zoomed);
    assert!(w.commands.try_iter().all(|command| {
        !matches!(command, UiCommand::Workbench { request, .. } if matches!(*request, ClientMessage::UpdateWorkbench { .. }))
    }));
}

#[test]
fn layout_conflict_rebases_the_latest_divider_without_overwriting_widget_changes() {
    use daedalus_ui::layout::Divider;

    let (id, mut w, _) = layout_client(true);
    w.app.pointer.splits.move_divider(Divider::Column, 0.64);
    w.app.save_workbench_layout();
    let (first, revision, first_ack) = next_layout_save(&w, id);
    assert_eq!(revision, 1);
    assert_eq!(first_ack.document.layout.splits["column"], 640);

    w.app.pointer.splits.move_divider(Divider::Column, 0.71);
    w.app.save_workbench_layout();
    assert!(w.idle());

    let mut other_client = snapshot_with_runner_label(3, "Updated runner");
    other_client
        .document
        .layout
        .splits
        .insert("future_split".into(), 475);
    daemon_workbench_reply(
        &mut w,
        id,
        WorkbenchOperation::Read,
        Ok(ResponsePayload::Workbench(other_client.clone())),
    );
    let outdated_conflict = snapshot_with_runner_label(2, "Earlier runner");
    daemon_workbench_reply(
        &mut w,
        id,
        first.clone(),
        Ok(ResponsePayload::WorkbenchConflict(outdated_conflict)),
    );

    let (second, revision, rebased) = next_layout_save(&w, id);
    assert_ne!(first, second);
    assert_eq!(revision, 3);
    assert_eq!(
        rebased.document.pane_widgets,
        other_client.document.pane_widgets
    );
    assert_eq!(rebased.document.layout.splits["column"], 710);
    assert_eq!(rebased.document.layout.splits["future_split"], 475);
    assert!(!w.app.state.zoomed);

    let mut stale_ack = first_ack;
    stale_ack.revision = 99;
    daemon_workbench_reply(&mut w, id, first, Ok(ResponsePayload::Workbench(stale_ack)));
    assert!(
        w.idle(),
        "the stale first token must not retire the second save"
    );
    assert_eq!(w.app.state.workbench.as_ref().unwrap().1.revision, 3);

    daemon_workbench_reply(&mut w, id, second, Ok(ResponsePayload::Workbench(rebased)));
    assert_eq!(w.app.state.workbench.as_ref().unwrap().1.revision, 4);
}

#[test]
fn reconnect_reloads_a_layout_base_and_workspace_removal_discards_the_pending_save() {
    let (id, mut w, initial) = layout_client(false);
    w.app.handle_ui_action(UiAction::ToggleZoom);
    let (before_disconnect, _, old_ack) = next_layout_save(&w, id);
    w.app.handle_daemon_event(UiEvent::Disconnected);
    w.app.handle_ui_action(UiAction::ToggleZoom);
    assert!(w.app.state.zoomed);
    daemon_workbench_reply(
        &mut w,
        id,
        before_disconnect,
        Ok(ResponsePayload::Workbench(old_ack)),
    );
    assert!(w.app.state.zoomed);
    assert!(w.idle());

    w.app.handle_daemon_event(UiEvent::Connected);
    w.app.poll_workbench();
    assert!(w.commands.try_iter().any(|command| matches!(
        command, UiCommand::Workbench { request, .. } if matches!(*request, ClientMessage::WatchWorkbench { workspace_id } if workspace_id == id)
    )));
    let mut fresh = initial;
    fresh.revision = 6;
    fresh.document.layout.split_view = true;
    daemon_workbench_reply(
        &mut w,
        id,
        WorkbenchOperation::Read,
        Ok(ResponsePayload::Workbench(fresh)),
    );
    assert!(w.app.state.zoomed);
    let (after_reconnect, revision, ack) = next_layout_save(&w, id);
    assert_eq!(revision, 6);
    assert!(!ack.document.layout.split_view);

    w.app
        .handle_daemon_event(UiEvent::WorkspaceMembershipLoaded(Vec::new()));
    w.commands.try_iter().for_each(drop);
    assert!(!w.app.layout_saves.contains_key(&id));
    daemon_workbench_reply(
        &mut w,
        id,
        after_reconnect,
        Ok(ResponsePayload::Workbench(ack)),
    );
    assert!(w.idle());
}
