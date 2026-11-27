use super::*;
use daedalus_proto::{PaneStatus, PaneSummary, SetupInspection, WorkspaceInfo};
use daedalus_ui::bridge::WorkbenchOperation;
use daedalus_ui::setup_review::SetupAction;
use daedalus_ui::workspace_form::WorkspaceFormAction;
use std::sync::mpsc::{Receiver, channel};
use tempfile::TempDir;
use winit::keyboard::{Key, NamedKey};

struct Workbench {
    app: DaedalusApp,
    commands: Receiver<UiCommand>,
    root: TempDir,
}

mod coverage_paths;
mod membership;
mod sessions;
mod settings_workflows;
mod widget_library;
mod widget_viewport;

#[test]
fn widget_editor_builds_previews_and_saves_a_daemon_definition() {
    use daedalus_proto::workbench::{
        WidgetAction, WidgetResult, WidgetSource, WorkbenchState, WorkspaceDocument,
    };
    use daedalus_proto::{ClientMessage, ResponsePayload, StatusLevel};
    use daedalus_ui::settings::Control;

    let mut workbench = Workbench::new();
    let workspace = workspace("Widget editor");
    let id = workspace.id;
    let agent = workspace.panes[0].id;
    workbench.app.state.set_workspaces(vec![workspace]);
    workbench.app.workbench_capable = true;
    workbench.app.open_workspace_settings();
    let state = WorkbenchState {
        revision: 2,
        document: WorkspaceDocument::new("Widget editor".into()),
        repo_baseline: None,
    };
    workbench.app.workbench_response(
        id,
        WorkbenchOperation::Read,
        Ok(ResponsePayload::Workbench(state)),
    );
    while workbench.commands.try_recv().is_ok() {}
    let settings = &mut workbench.app.state.settings;
    settings.activate(Control::CustomWidgetsPage);
    settings.activate(Control::CustomNew);
    settings.set_value(Control::CustomName, "API health");
    settings.set_value(Control::CustomExecutable, "printf");
    settings.set_value(Control::CustomArguments, r#"["ready"]"#);
    settings.set_value(Control::CustomInterval, "5");
    settings.activate(Control::CustomOutput);
    settings.set_value(Control::CustomSummary, "main:text");
    let url = "https://example.invalid/health";
    settings.set_value(Control::CustomUrl, url);
    let preview = settings.activate(Control::CustomPreview);
    workbench.app.handle_settings_action(preview);
    let UiCommand::Workbench {
        request, operation, ..
    } = workbench.command()
    else {
        panic!("expected a daemon preview")
    };
    let ClientMessage::PreviewWidget { definition, .. } = *request else {
        panic!("expected a widget definition")
    };
    assert!(
        matches!(&definition.sources[0].source, WidgetSource::Command { command, refresh, .. }
            if command.argv == ["printf", "ready"] && refresh.interval_seconds == Some(5))
    );
    assert_eq!(
        definition.actions[0].action,
        WidgetAction::OpenUrl { url: url.into() }
    );
    let duplicate = workbench
        .app
        .state
        .settings
        .activate(Control::CustomPreview);
    workbench.app.handle_settings_action(duplicate);
    assert!(workbench.commands.try_recv().is_err());
    let result = WidgetResult {
        label: "Ready".into(),
        level: StatusLevel::Ok,
        details: vec![],
    };
    workbench.app.workbench_response(
        id,
        operation,
        Ok(ResponsePayload::WidgetResult(result.clone())),
    );
    assert_eq!(workbench.app.state.settings.custom.preview, Some(result));
    let save = workbench.app.state.settings.activate(Control::CustomSave);
    workbench.app.handle_settings_action(save);
    let UiCommand::Workbench {
        request, operation, ..
    } = workbench.command()
    else {
        panic!("expected a daemon save")
    };
    let ClientMessage::UpdateWorkbench {
        revision: 2,
        document,
        ..
    } = *request
    else {
        panic!("expected a saved workspace")
    };
    assert_eq!(document.widgets.len(), 1);
    assert_eq!(document.widgets[0].name, "API health");
    assert_eq!(
        document.widgets[0].definition.as_ref().unwrap().actions,
        definition.actions
    );
    workbench.app.workbench_response(
        id,
        operation,
        Ok(ResponsePayload::Workbench(WorkbenchState {
            revision: 3,
            document,
            repo_baseline: None,
        })),
    );
    assert!(workbench.app.state.settings.custom.draft.is_none());
    assert_eq!(
        workbench.app.state.pane_id_for_role(PaneRole::Agent),
        Some(agent)
    );
}

#[test]
fn widget_lifecycle_creates_a_slot_and_removal_keeps_its_terminal() {
    use daedalus_ui::settings::Control;

    let mut workbench = Workbench::new();
    let mut workspace = workspace("Widget lifecycle");
    let service = workspace.panes.remove(1);
    let id = workspace.id;
    workbench.app.state.set_workspaces(vec![workspace]);
    workbench.app.open_workspace_settings();
    let settings = &mut workbench.app.state.settings;
    settings.activate(Control::WidgetsPage);
    let add = settings
        .control_order()
        .find(|&control| settings.label(control) == "+ Add service widget")
        .expect("missing service widget offers creation");
    settings.activate(add);
    settings.set_value(Control::ServiceLabel, "Health API");
    let action = settings.activate(Control::Save);
    workbench.app.handle_settings_action(action);
    assert_eq!(
        workbench
            .app
            .state
            .widget_label(PaneRole::Service)
            .as_deref(),
        Some("Health API · No terminal")
    );

    workbench.app.state.register_pane(service.clone(), false);
    let settings = &mut workbench.app.state.settings;
    let remove = settings
        .control_order()
        .find(|&control| settings.label(control) == "Remove service widget")
        .expect("configured widget offers removal");
    settings.activate(remove);
    let action = settings.activate(Control::Save);
    workbench.app.handle_settings_action(action);
    assert!(
        workbench
            .app
            .state
            .widget_label(PaneRole::Service)
            .is_none()
    );
    assert_eq!(
        workbench.app.state.pane_id_for_role(PaneRole::Service),
        Some(service.id)
    );

    workbench.app.state.workspace_widgets =
        daedalus_ui::workspace_widgets::load_all(&workbench.app.widget_directory);
    assert!(
        workbench
            .app
            .state
            .widget_label(PaneRole::Service)
            .is_none()
    );
    assert!(
        workbench.app.state.workspace_widgets[&id]
            .service
            .label
            .is_none()
    );
    assert!(workbench.idle());
}

#[test]
fn widget_page_updates_the_current_workspace_and_saves_per_profile() {
    use daedalus_ui::settings::Control;

    let mut workbench = Workbench::new();
    let workspace = workspace("Widgets");
    let id = workspace.id;
    workbench.app.state.set_workspaces(vec![workspace]);
    workbench.app.open_workspace_settings();

    let settings = &mut workbench.app.state.settings;
    settings.activate(Control::WidgetsPage);
    settings.set_value(Control::ServiceLabel, "  API on WSL  ");
    settings.activate(Control::ServicePreset);
    settings.activate(Control::RunnerWarnings);
    settings.activate(Control::RunnerWarnings);
    let action = settings.activate(Control::Save);
    workbench.app.handle_settings_action(action);

    assert!(workbench.app.state.settings.open);
    assert!(workbench.app.state.settings.widgets_saved);
    assert_eq!(workbench.app.state.settings.label(Control::Cancel), "Done");
    assert!(
        workbench
            .app
            .state
            .widget_label(PaneRole::Service)
            .unwrap()
            .starts_with("API on WSL · ")
    );
    let persisted = daedalus_ui::workspace_widgets::load_all(&workbench.app.widget_directory);
    assert_eq!(persisted[&id], workbench.app.state.workspace_widgets[&id]);
    assert_eq!(persisted[&id].runner.show_warnings, Some(false));
    assert!(workbench.idle());
}

#[test]
fn failed_widget_save_keeps_the_previous_configuration_and_the_draft() {
    use daedalus_ui::settings::Control;
    use daedalus_ui::workspace_widgets::WorkspaceWidgets;

    let mut workbench = Workbench::new();
    let workspace = workspace("Widgets");
    let id = workspace.id;
    workbench.app.state.set_workspaces(vec![workspace]);
    let mut previous = WorkspaceWidgets::default();
    previous.runner.label = Some("Original".into());
    workbench
        .app
        .state
        .workspace_widgets
        .insert(id, previous.clone());
    std::fs::write(&workbench.app.widget_directory, "not a directory").unwrap();
    workbench.app.open_workspace_settings();
    let settings = &mut workbench.app.state.settings;
    settings.activate(Control::WidgetsPage);
    settings.set_value(Control::RunnerLabel, "Edited");
    let action = settings.activate(Control::Save);
    workbench.app.handle_settings_action(action);

    let settings = &workbench.app.state.settings;
    assert!(settings.open && settings.error.is_some());
    assert_eq!(settings.value(Control::RunnerLabel), Some("Edited"));
    assert_eq!(workbench.app.state.workspace_widgets[&id], previous);
}

impl Workbench {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let config = BridgeConfig {
            bootstrap_explicit_dir: false,
            socket_path: root.path().join("socket"),
            auth_token: None,
            initial_dir: root.path().to_owned(),
            default_agent: AgentPreset::Codex,
            auto_spawn_daemon: false,
        };
        let mut app = DaedalusApp::new(
            config,
            UiConfig::default(),
            None,
            &root.path().join("config.toml"),
        );
        let (tx, commands) = channel();
        app.bridge = Some(Box::new(move |cmd| tx.send(cmd).is_ok()));
        app.handle_daemon_event(UiEvent::Connected);
        Self {
            app,
            commands,
            root,
        }
    }

    /// Nothing was sent apart from recent-project refreshes.
    fn idle(&self) -> bool {
        self.commands
            .try_iter()
            .all(|command| matches!(command, UiCommand::RefreshRecentProjects))
    }

    /// The next command, past the recent-project refresh every opened form asks for.
    fn command(&self) -> UiCommand {
        loop {
            match self
                .commands
                .try_recv()
                .expect("expected controller command")
            {
                UiCommand::RefreshRecentProjects => continue,
                command => return command,
            }
        }
    }

    fn begin_review(&mut self) {
        self.app
            .handle_daemon_event(UiEvent::WorkspacesLoaded(vec![]));
        assert!(self.app.state.workspace_form.open);
        let folder = self.root.path().to_owned();
        self.app.handle_workspace_form(WorkspaceFormAction::Save {
            name: "Workbench".into(),
            folder: folder.clone(),
        });
        let UiCommand::InspectSetup {
            session,
            root_dir,
            executables,
        } = self.command()
        else {
            panic!("expected inspection")
        };
        assert_eq!(root_dir, folder);
        assert!(executables.is_empty());
        self.app.handle_daemon_event(UiEvent::SetupInspected {
            session: session.wrapping_sub(1),
            result: Err("stale".into()),
        });
        assert!(!self.app.state.setup_review.open);
        self.app.handle_daemon_event(UiEvent::SetupInspected {
            session,
            result: Ok(inspection()),
        });
        assert_eq!(self.app.state.setup_review.host(), "daemon-host");
        assert!(self.app.state.setup_review.open);
    }
}

fn inspection() -> SetupInspection {
    SetupInspection {
        commands: vec![(PaneRole::Runner, vec!["cargo".into(), "test".into()])],
        available: vec!["cargo".into()],
        dependency_install: None,
        host: "daemon-host".into(),
        platform: "linux".into(),
    }
}

fn workspace(name: &str) -> WorkspaceInfo {
    let id = WorkspaceId::new();
    let panes = [PaneRole::Agent, PaneRole::Service, PaneRole::Runner]
        .into_iter()
        .map(|role| PaneSummary {
            id: PaneId::new(),
            workspace_id: id,
            role,
            status: PaneStatus::Busy,
            cols: 80,
            rows: 24,
            cwd: None,
            last_exit_code: None,
            has_failure_trace: false,
            launch_preset: None,
            agent_state: None,
        })
        .collect();
    WorkspaceInfo {
        id,
        name: name.into(),
        panes,
        root_dir: "/project".into(),
        slot_index: 0,
        project: Default::default(),
        setup: None,
        default_panes_pending: None,
        injected_env: Default::default(),
        port_offset: 0,
        compose_project_name: None,
        git_branch: None,
        git_status: None,
        bound_ports: vec![],
        containers: vec![],
        external_statuses: vec![],
        manifest: None,
    }
}

#[test]
fn first_workspace_installs_a_tool_then_applies_review_to_same_workspace() {
    let mut w = Workbench::new();
    daemon_capabilities(&mut w, true);
    w.begin_review();
    let install = vec!["installer".to_string(), "tool".to_string()];
    w.app
        .handle_setup_action(SetupAction::Install(install.clone()));
    let UiCommand::CreateManualWorkspace {
        session,
        name,
        root_dir,
    } = w.command()
    else {
        panic!("expected workspace creation")
    };
    assert_eq!(name, "Workbench");
    assert_eq!(root_dir, w.root.path());
    w.app
        .handle_setup_action(SetupAction::Install(install.clone()));
    assert!(w.idle());
    let mut ws = workspace("Workbench");
    ws.setup = Some(vec![stored_setup().remove(0)]);
    let expected_baseline = ws.setup.clone();
    let id = ws.id;
    let pane = ws.panes[2].id;
    w.app.handle_daemon_event(UiEvent::ManualWorkspaceCreated {
        session,
        workspace: ws.clone(),
    });
    assert!(
        matches!(w.command(), UiCommand::InstallSetupTool { workspace_id, argv }
            if workspace_id == id && argv == install)
    );
    w.app.handle_daemon_event(UiEvent::SetupInstallerStarted {
        workspace_id: id,
        pane_id: pane,
    });
    assert!(w.app.setup_installer_is_running());
    assert!(!w.app.state.setup_review.open);
    assert_eq!(w.app.state.session_names[&pane], "Setup installer");
    w.app.handle_ui_action(UiAction::NewWorkspace);
    assert!(!w.app.state.workspace_form.open);
    w.app.handle_daemon_event(UiEvent::Disconnected);
    w.app.handle_daemon_event(UiEvent::Connected);
    let mut updated = ws;
    updated.setup = Some(stored_setup());
    w.app
        .handle_daemon_event(UiEvent::WorkspacesLoaded(vec![updated]));
    assert!(w.app.setup_installer_is_running());
    w.app
        .handle_daemon_event(UiEvent::SetupInstallerFinished(pane, Some(7)));
    assert!(w.app.state.setup_review.open);
    let UiCommand::InspectSetup {
        session,
        executables,
        ..
    } = w.command()
    else {
        panic!("expected tool refresh")
    };
    assert!(executables.contains(&"cargo".into()));
    w.app.handle_daemon_event(UiEvent::SetupInspected {
        session,
        result: Ok(inspection()),
    });
    assert!(w.app.state.setup_review.feedback.contains("exited 7"));
    w.app.handle_setup_action(SetupAction::Open);
    assert!(
        matches!(w.command(), UiCommand::ApplySetup { workspace_id, assignments, expected, .. }
            if workspace_id == id
                && expected.assignments == expected_baseline
                && assignments.iter().any(|a| a.id == "setup-runner" && a.argv == ["cargo", "test"]))
    );
    w.app.handle_daemon_event(UiEvent::SetupApplied {
        workspace_id: id,
        error: Some("start failed".into()),
    });
    assert_eq!(w.app.state.setup_review.feedback, "start failed");
    w.app.handle_setup_action(SetupAction::Open);
    assert!(
        matches!(w.command(), UiCommand::ApplySetup { workspace_id, .. } if workspace_id == id)
    );
    w.app.handle_daemon_event(UiEvent::SetupApplied {
        workspace_id: id,
        error: None,
    });
    assert!(!w.app.state.setup_review.open);
    assert_eq!(w.app.state.active_workspace_id, Some(id));
}

#[test]
fn guided_setup_on_an_old_daemon_remembers_an_explicitly_empty_local_setup() {
    const CHILD: &str = "DAEDALUS_TEST_GUIDED_LOCAL_SETUP";
    if !run_in_isolated_profile(
        "controller_tests::guided_setup_on_an_old_daemon_remembers_an_explicitly_empty_local_setup",
        CHILD,
    ) {
        return;
    }
    let mut w = Workbench::new();
    daemon_capabilities(&mut w, false);
    w.begin_review();
    w.app
        .handle_setup_action(SetupAction::Install(vec!["installer".into()]));
    let UiCommand::CreateManualWorkspace { session, .. } = w.command() else {
        panic!("expected workspace creation");
    };
    let ws = workspace("Workbench");
    let id = ws.id;
    daedalus_ui::assignments::save(id, &[]).unwrap();
    w.app.handle_daemon_event(UiEvent::ManualWorkspaceCreated {
        session,
        workspace: ws,
    });
    assert!(matches!(w.command(), UiCommand::InstallSetupTool { .. }));
    w.app.handle_daemon_event(UiEvent::SetupInstallerFailed {
        workspace_id: id,
        message: "simulated installer failure".into(),
    });
    w.app.handle_setup_action(SetupAction::Open);
    let UiCommand::ApplySetup { expected, .. } = w.command() else {
        panic!("expected to apply the workspace setup");
    };
    assert_eq!(expected.assignments, Some(Vec::new()));
}

#[test]
fn review_revisit_preserves_tool_commands_and_disconnect_clears_pending_creation() {
    let mut w = Workbench::new();
    w.begin_review();
    w.app.handle_setup_action(SetupAction::Back);
    assert!(w.app.state.workspace_form.open);
    w.app.handle_workspace_form(WorkspaceFormAction::Save {
        name: "Renamed".into(),
        folder: w.root.path().to_owned(),
    });
    let UiCommand::InspectSetup {
        session,
        executables,
        ..
    } = w.command()
    else {
        panic!("expected folder inspection")
    };
    assert!(executables.contains(&"cargo".into()));
    w.app.handle_daemon_event(UiEvent::SetupInspected {
        session,
        result: Ok(inspection()),
    });
    assert_eq!(w.app.state.setup_review.name, "Renamed");
    w.app.handle_setup_action(SetupAction::Open);
    let UiCommand::CreateManualWorkspace { session, .. } = w.command() else {
        panic!("expected creation")
    };
    w.app.handle_daemon_event(UiEvent::ManualWorkspaceFailed {
        session,
        message: "occupied".into(),
    });
    assert_eq!(w.app.state.setup_review.feedback, "occupied");
    w.app.handle_setup_action(SetupAction::Open);
    assert!(matches!(
        w.command(),
        UiCommand::CreateManualWorkspace { .. }
    ));
    w.app.handle_daemon_event(UiEvent::Disconnected);
    assert!(!w.app.state.setup_review.open);
    assert_eq!(w.app.state.connection, ConnectionState::Reconnecting);
    w.app.bridge = None;
    w.app.handle_ui_action(UiAction::NewWorkspace);
    w.app.handle_workspace_form(WorkspaceFormAction::Save {
        name: "offline".into(),
        folder: w.root.path().into(),
    });
    assert!(
        w.app
            .state
            .workspace_form
            .error
            .as_deref()
            .unwrap()
            .contains("Reconnect")
    );
    w.app.workspace_form_key(&Key::Named(NamedKey::Escape));
    assert!(!w.app.state.workspace_form.open);
    w.app.handle_daemon_event(UiEvent::WorkspacesLoaded(vec![]));
    assert!(!w.app.state.workspace_form.open);
}

#[test]
fn search_rejects_stale_results_and_routes_input_and_scrollback_to_the_selected_session() {
    use daedalus_proto::{GridRow, GridSnapshot, SearchMatch};
    let mut w = Workbench::new();
    let ws = workspace("Search");
    let pane = ws.panes[1].id;
    w.app.handle_daemon_event(UiEvent::WorkspaceCreated(ws));
    w.app.activate_session(HitTarget::SelectSession(pane));
    assert_eq!(w.app.state.focused_role, PaneRole::Service);
    w.app.handle_daemon_event(UiEvent::Snapshot(GridSnapshot {
        terminal_modes: Default::default(),
        activity: None,
        insight: None,
        pane_id: pane,
        seq: 1,
        cols: 80,
        rows: 24,
        cursor: Default::default(),
        rows_data: (0..24)
            .map(|n| GridRow {
                links: Vec::new(),
                wraps: false,
                row_index: n,
                abs_row: 100 + u64::from(n),
                cells: vec![],
            })
            .collect(),
        scrollback_len: 100,
        history_generation: 0,
    }));
    w.app.handle_ui_action(UiAction::ToggleSearch);
    w.app.handle_ui_action(UiAction::SearchText("test!".into()));
    let UiCommand::SearchPane {
        pane_id,
        generation: old,
        query,
        ..
    } = w.command()
    else {
        panic!("expected search")
    };
    assert_eq!((pane_id, query.as_str()), (pane, "test!"));
    w.app.handle_ui_action(UiAction::SearchBackspace);
    assert!(matches!(w.command(), UiCommand::SearchPane { query, .. } if query == "test"));
    w.app.handle_ui_action(UiAction::SearchToggleCase);
    let UiCommand::SearchPane {
        generation,
        case_sensitive,
        ..
    } = w.command()
    else {
        panic!("expected search")
    };
    assert!(case_sensitive);
    let results = |generation| UiEvent::SearchResults {
        pane_id: pane,
        generation,
        total_scrollback: 100,
        matches: [101, 5]
            .into_iter()
            .map(|abs_row| SearchMatch {
                abs_row,
                start_col: 0,
                end_col: 4,
            })
            .collect(),
    };
    w.app.handle_daemon_event(results(old));
    assert!(w.app.state.search.as_ref().unwrap().matches.is_empty());
    w.app.handle_daemon_event(results(generation));
    assert_eq!(w.app.state.search.as_ref().unwrap().current_index, 0);
    w.app.handle_ui_action(UiAction::SearchNext);
    let UiCommand::RequestScrollback {
        pane_id,
        generation,
        offset_from_bottom,
        count,
    } = w.command()
    else {
        panic!("expected history request")
    };
    assert_eq!(pane_id, pane);
    assert!(offset_from_bottom > 0);
    assert_eq!(count, 24);
    w.app.handle_daemon_event(UiEvent::ScrollbackLoaded {
        pane_id: pane,
        generation,
        total_scrollback: 100,
        rows: vec![],
    });
    w.app.handle_ui_action(UiAction::SearchPrev);
    assert_eq!(w.app.state.search.as_ref().unwrap().current_index, 0);
    w.app.handle_ui_action(UiAction::SearchCancel);
    assert!(w.app.state.search.is_none());
    w.app.handle_ui_action(UiAction::ScrollPageUp);
    assert!(w.app.state.panes[&pane].requested_scroll_offset > 0);
    w.app.handle_ui_action(UiAction::ScrollPageDown);
    w.app.handle_ui_action(UiAction::ScrollToBottom);
    assert_eq!(w.app.state.panes[&pane].requested_scroll_offset, 0);
    w.app
        .handle_ui_action(UiAction::SendBytes(b"pwd\r".to_vec()));
    w.app.handle_ui_action(UiAction::CloseTerminal);
    let mut pending = w
        .commands
        .try_iter()
        .filter(|c| !matches!(c, UiCommand::RequestScrollback { .. }));
    assert!(
        matches!(pending.next(), Some(UiCommand::SendInput { pane_id, data}) if pane_id == pane && data == b"pwd\r")
    );
    assert!(matches!(pending.next(), Some(UiCommand::ClosePane(id)) if id == pane));
    w.app
        .handle_daemon_event(UiEvent::PaneClosed { pane_id: pane });
    assert!(!w.app.state.panes.contains_key(&pane));
}

#[test]
fn palette_presets_worktrees_and_confirmation_send_the_selected_workspace_ids() {
    use daedalus_proto::{CustomAction, WorkspaceManifest};
    let mut w = Workbench::new();
    let mut ws = workspace("Tools");
    let id = ws.id;
    let agent = ws.panes[0].id;
    ws.manifest = Some(WorkspaceManifest {
        actions: ["build", "lint"]
            .into_iter()
            .map(|id| CustomAction {
                id: id.into(),
                label: id.into(),
                command: id.into(),
                target: PaneRole::Runner,
                shortcut: None,
            })
            .collect(),
        ..Default::default()
    });
    w.app
        .handle_daemon_event(UiEvent::WorkspacesLoaded(vec![ws]));
    w.app.handle_ui_action(UiAction::ToggleActionPalette);
    w.app.handle_ui_action(UiAction::ActionPaletteUp);
    assert_eq!(w.app.state.action_palette.selected_index, 1);
    w.app.handle_ui_action(UiAction::ActionPaletteDown);
    w.app.handle_ui_action(UiAction::ActionPaletteConfirm);
    assert!(
        matches!(w.command(), UiCommand::RunWorkspaceAction { workspace_id, action_id}
            if workspace_id == id && action_id == "build")
    );
    w.app.handle_ui_action(UiAction::ToggleAgentPicker);
    w.app.handle_ui_action(UiAction::PickerUp);
    let chosen = w.app.state.picker.selected_preset();
    w.app.handle_ui_action(UiAction::PickerAdd);
    assert!(
        matches!(w.command(), UiCommand::AddAgent { workspace_id, preset, .. }
            if workspace_id == id && preset == chosen)
    );
    w.app.handle_ui_action(UiAction::ToggleAgentPicker);
    w.app.handle_ui_action(UiAction::PickerDown);
    let chosen = w.app.state.picker.selected_preset();
    w.app.handle_ui_action(UiAction::PickerConfirm);
    assert!(
        matches!(w.command(), UiCommand::ReplaceAgentPreset { workspace_id, preset, old_pane_id, .. }
            if workspace_id == id && preset == chosen && old_pane_id == Some(agent))
    );
    w.app.handle_ui_action(UiAction::ToggleWorktreeModal);
    w.app.handle_ui_action(UiAction::WorktreeModalConfirm);
    assert!(w.app.state.worktree_modal.missing_branch());
    w.app
        .handle_ui_action(UiAction::WorktreeModalText("feature!".into()));
    w.app.handle_ui_action(UiAction::WorktreeModalBackspace);
    w.app
        .handle_ui_action(UiAction::WorktreeModalSetText("topic".into()));
    w.app.handle_ui_action(UiAction::WorktreeModalConfirm);
    assert!(
        matches!(w.command(), UiCommand::CreateWorktreeWorkspace {repo_dir, branch, .. }
            if repo_dir == std::path::Path::new("/project") && branch == "topic")
    );
    assert!(!w.app.state.worktree_modal.open);
    w.app.handle_ui_action(UiAction::OpenCloseWorkspaceModal);
    w.app.handle_ui_action(UiAction::CloseModalToggleCompose);
    w.app.handle_ui_action(UiAction::CloseModalConfirm);
    assert!(
        matches!(w.command(), UiCommand::CloseWorkspace { id: closed, compose_down: true, .. } if closed == id)
    );
    w.app.handle_ui_action(UiAction::CloseModalCancel);
    assert!(w.app.state.close_modal.open);
    w.app
        .handle_daemon_event(UiEvent::WorkspaceCloseFinished(id));
    w.app.handle_ui_action(UiAction::CloseModalCancel);
    assert!(!w.app.state.close_modal.open);
}

#[test]
fn dormant_workspaces_spawn_once_and_failure_forwarding_revalidates_agent_choices() {
    let mut w = Workbench::new();
    let mut dormant = workspace("Dormant");
    dormant.panes.clear();
    dormant.default_panes_pending = Some(true);
    let mut failed = workspace("Failed");
    let agent = failed.panes[0].id;
    let runner = failed.panes[2].id;
    failed.panes[2].has_failure_trace = true;
    failed.panes[2].last_exit_code = Some(1);
    w.app.handle_daemon_event(UiEvent::WorkspacesLoaded(vec![
        dormant.clone(),
        failed.clone(),
    ]));
    assert!(
        matches!(w.command(), UiCommand::EnsureWorkspacePanes { workspace_id, .. } if workspace_id == dormant.id)
    );
    w.app.ensure_active_workspace_panes();
    assert!(w.idle());
    w.app.handle_daemon_event(UiEvent::WorkspacePanesEnsured {
        workspace_id: dormant.id,
        complete: true,
    });
    assert!(w.app.state.spawning_workspaces.is_empty());
    w.app.handle_ui_action(UiAction::OpenNextFailedRun);
    assert_eq!(w.app.state.focused_pane_id(), Some(runner));
    w.app.handle_ui_action(UiAction::ToggleZoom);
    w.app.handle_ui_action(UiAction::ForwardFailureToAgent);
    assert!(
        matches!(w.command(), UiCommand::ForwardRunnerFailureToAgent { runner_pane_id, agent_pane_id}
            if (runner_pane_id, agent_pane_id) == (runner, agent))
    );
    assert_eq!(w.app.state.focused_pane_id(), Some(agent));
    let mut second = failed.panes[0].clone();
    second.id = PaneId::new();
    second.launch_preset = Some("claude".into());
    w.app.handle_daemon_event(UiEvent::PaneSpawned {
        summary: second,
        select_tab: true,
    });
    w.app.handle_ui_action(UiAction::ForwardFailureToAgent);
    let choices = w.app.state.failure_recipients.take().unwrap();
    w.app.handle_ui_action(UiAction::SwitchWorkspacePrev);
    w.app.send_failure_to_recipient(choices);
    assert!(w.app.state.toast.as_ref().unwrap().0.contains("changed"));
    assert!(w.idle());
    w.app.handle_ui_action(UiAction::SwitchWorkspaceNext);
    assert_eq!(w.app.state.active_workspace_id, Some(failed.id));
}

#[test]
fn settings_persist_bindings_restore_names_and_launch_saved_tests_in_an_isolated_profile() {
    use daedalus_ui::{
        assignments::{self, Assignment},
        settings::SettingsAction,
    };
    const CHILD: &str = "DAEDALUS_TEST_SETTINGS_CONTROLLER";
    if std::env::var_os(CHILD).is_none() {
        let profile = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "controller_tests::settings_persist_bindings_restore_names_and_launch_saved_tests_in_an_isolated_profile",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("DAEDALUS_CONFIG", profile.path().join("config.toml"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let mut w = Workbench::new();
    let ws = workspace("Settings");
    let id = ws.id;
    let runner = ws.panes[2].id;
    let entry = Assignment {
        id: "setup-runner".into(),
        name: "Regression suite".into(),
        role: PaneRole::Runner,
        argv: vec!["cargo".into(), "test".into()],
    };
    w.app
        .handle_daemon_event(UiEvent::WorkspaceCreated(ws.clone()));
    w.app.handle_ui_action(UiAction::WorkspaceSettings);
    assert!(w.app.state.settings.open);
    w.app
        .handle_settings_action(SettingsAction::Save(vec![entry.clone(), entry.clone()]));
    assert!(
        w.app
            .state
            .settings
            .error
            .as_deref()
            .unwrap()
            .contains("unique")
    );
    w.app.handle_settings_action(SettingsAction::Launch(
        vec![entry.clone()],
        entry.id.clone(),
    ));
    assert!(!w.app.state.settings.open);
    assert!(
        matches!(w.command(), UiCommand::LaunchAssignment { workspace_id, assignment, cols, rows, ..}
            if workspace_id == id && assignment == entry && (cols, rows) == (FALLBACK_PANE_COLS, FALLBACK_PANE_ROWS))
    );
    w.app.handle_daemon_event(UiEvent::AssignmentStarted {
        workspace_id: id,
        pane_id: runner,
        assignment: entry.clone(),
    });
    assert_eq!(w.app.state.focused_pane_id(), Some(runner));
    assert_eq!(assignments::load_bindings(id).unwrap()[&runner], entry.id);
    w.app.state.session_names.clear();
    w.app
        .handle_daemon_event(UiEvent::WorkspacesLoaded(vec![ws.clone()]));
    assert_eq!(w.app.state.session_names[&runner], "Regression suite");
    w.app.handle_ui_action(UiAction::RunTests);
    assert!(
        matches!(w.command(), UiCommand::LaunchAssignment { workspace_id, assignment, .. }
            if workspace_id == id && assignment == entry)
    );
    w.app.bridge = None;
    w.app.handle_ui_action(UiAction::RunTests);
    assert!(w.app.state.toast.as_ref().unwrap().0.contains("Reconnect"));
    w.app.handle_ui_action(UiAction::WorkspaceSettings);
    w.app.handle_settings_action(SettingsAction::Launch(
        vec![entry.clone()],
        entry.id.clone(),
    ));
    assert!(w.app.state.settings.open);
    assert!(
        w.app
            .state
            .settings
            .error
            .as_deref()
            .unwrap()
            .contains("Reconnect")
    );
    w.app.settings_key(&Key::Named(NamedKey::Escape));
    assert!(!w.app.state.settings.open);
    assignments::save(id, &[]).unwrap();
    w.app.handle_ui_action(UiAction::RunTests);
    assert!(
        w.app
            .state
            .toast
            .as_ref()
            .unwrap()
            .0
            .contains("no longer configured")
    );
    assert!(!w.app.state.test_workspaces.contains(&id));
    let bad = daedalus_proto::default_config_file_path()
        .with_file_name("assignments")
        .join(format!("{id}.toml"));
    std::fs::write(bad, "[invalid").unwrap();
    w.app.handle_ui_action(UiAction::WorkspaceSettings);
    assert!(!w.app.state.settings.open);
    assert!(
        w.app
            .state
            .toast
            .as_ref()
            .unwrap()
            .0
            .contains("Invalid assignments")
    );
}

#[test]
fn disconnected_state_clears_pending_commands_and_keeps_actionable_feedback_visible() {
    let mut w = Workbench::new();
    w.app.handle_ui_action(UiAction::WorkspaceSettings);
    assert!(
        w.app
            .state
            .toast
            .as_ref()
            .unwrap()
            .0
            .contains("Create a workspace")
    );
    w.app.handle_ui_action(UiAction::OpenCloseWorkspaceModal);
    assert!(w.app.state.toast.as_ref().unwrap().1);
    w.app.handle_ui_action(UiAction::OpenNextFailedRun);
    assert!(w.app.state.toast.as_ref().unwrap().0.contains("No failed"));
    w.app.handle_ui_action(UiAction::RunTests);
    assert!(w.app.state.toast.as_ref().unwrap().0.contains("No test"));
    w.app.handle_ui_action(UiAction::ForwardFailureToAgent);
    assert!(
        w.app
            .state
            .toast
            .as_ref()
            .unwrap()
            .0
            .contains("not available")
    );
    w.app.handle_daemon_event(UiEvent::Toast {
        message: "attention".into(),
        is_error: false,
    });
    assert!(w.app.has_top_banner());
    let shown = w.app.state.toast.as_ref().unwrap().2;
    w.app.expire_toast_if_due(shown);
    assert!(w.app.state.toast.is_some());
    w.app.expire_toast_if_due(shown + TOAST_TTL);
    assert!(!w.app.has_top_banner());
    let ws = workspace("Reconnecting");
    let id = ws.id;
    w.app
        .handle_daemon_event(UiEvent::WorkspacesLoaded(vec![ws.clone()]));
    w.app.handle_ui_action(UiAction::OpenCloseWorkspaceModal);
    w.app.handle_daemon_event(UiEvent::Disconnected);
    w.app.handle_ui_action(UiAction::CloseModalConfirm);
    assert!(w.app.state.toast.as_ref().unwrap().0.contains("Reconnect"));
    assert!(w.idle());
    w.app
        .handle_daemon_event(UiEvent::WorkspaceCloseFailed(vec![ws]));
    assert_eq!(w.app.state.active_workspace_id, Some(id));
}

#[test]
fn clicked_links_follow_the_scrollback_view_and_remote_host_boundary() {
    use daedalus_proto::{Cell, GridRow};
    use daedalus_ui::links::DetectedLink;
    let mut w = Workbench::new();
    let mut ws = workspace("links");
    ws.root_dir = w.root.path().into();
    let id = ws.panes[0].id;
    let role = ws.panes[0].role;
    w.app
        .handle_daemon_event(UiEvent::WorkspacesLoaded(vec![ws]));
    let row = |text: &str| GridRow {
        row_index: 0,
        abs_row: 0,
        wraps: false,
        links: vec![],
        cells: text
            .chars()
            .map(|ch| Cell {
                text: ch.to_string(),
                ..Default::default()
            })
            .collect(),
    };
    let path = w.root.path().join("source.rs");
    std::fs::write(&path, "").unwrap();
    let pane = w.app.state.panes.get_mut(&id).unwrap();
    pane.viewport_rows = vec![row("source.rs:42")];
    assert!(matches!(
        w.app.clicked_link(role, 0, 0),
        Some(DetectedLink::File { line: Some(42), .. })
    ));
    w.app.state.remote_host = Some("build-host".into());
    assert!(w.app.clicked_link(role, 0, 0).is_none());
    let pane = w.app.state.panes.get_mut(&id).unwrap();
    pane.scroll_offset = 10;
    pane.scrolled_rows = Some(vec![row("https://scrollback.example")]);
    assert_eq!(
        w.app.clicked_link(role, 4, 0),
        Some(DetectedLink::Url("https://scrollback.example".into()))
    );
    assert!(w.app.clicked_link(role, 0, 10).is_none());
}

#[test]
fn pointer_routes_header_sidebar_and_overlay_clicks_using_the_rendered_layout() {
    use daedalus_ui::layout::{
        Rect,
        overlays::{
            close_modal_cancel_rect, close_workspace_modal_rect, picker_item_rect,
            preset_picker_layout, worktree_button_rects, worktree_modal_rect,
        },
        sidebar_workspace_close_rect,
    };
    use daedalus_ui::sidebar_content::SidebarRow;
    let metrics = ViewportMetrics {
        size: (1440, 900).into(),
        scale_factor: 1.,
        cell: (8., 16.),
    };
    let click = |app: &mut DaedalusApp, r: Rect| {
        app.handle_cursor_moved(r.x + r.width / 2., r.y + r.height / 2.);
        app.click_at(metrics);
    };
    let mut w = Workbench::new();
    let first = workspace("One");
    let mut second = workspace("Two");
    let runner = second.panes[2].id;
    second.panes[2].last_exit_code = Some(2);
    w.app.handle_daemon_event(UiEvent::WorkspacesLoaded(vec![
        first.clone(),
        second.clone(),
    ]));
    w.app.state.zoomed = false;
    let layout = w.app.layout_for(metrics);
    let list = layout.sidebar_content.as_ref().unwrap();
    let second_rect = list.rows[list.visible.clone()]
        .iter()
        .zip(&list.rects)
        .find_map(|(row, rect)| matches!(row, SidebarRow::Workspace(1)).then_some(*rect))
        .unwrap();
    click(&mut w.app, second_rect);
    assert_eq!(w.app.state.active_workspace_id, Some(second.id));
    click(&mut w.app, sidebar_workspace_close_rect(second_rect, 1.));
    assert_eq!(w.app.state.close_modal.workspace_id, Some(second.id));
    click(
        &mut w.app,
        close_modal_cancel_rect(close_workspace_modal_rect(1440., 900., 1.), 1.),
    );
    assert!(!w.app.state.close_modal.open);
    click(&mut w.app, layout.failed_run_buttons[0].1);
    assert_eq!(w.app.state.focused_pane_id(), Some(runner));
    click(&mut w.app, layout.pane(PaneRole::Service).unwrap().viewport);
    assert_eq!(w.app.state.focused_role, PaneRole::Service);
    click(
        &mut w.app,
        layout.pane(PaneRole::Agent).unwrap().action_pill.unwrap(),
    );
    assert!(w.app.state.picker.open);
    let (modal, rows) = preset_picker_layout(1440., 900., 1., w.app.state.picker.selected_index);
    click(&mut w.app, picker_item_rect(modal, 0, 1.));
    assert!(
        matches!(w.command(), UiCommand::ReplaceAgentPreset { workspace_id, preset, .. }
            if workspace_id == second.id && preset == AgentPreset::ALL[rows.start])
    );
    click(&mut w.app, list.new_worktree);
    assert!(w.app.state.worktree_modal.open);
    click(
        &mut w.app,
        worktree_button_rects(worktree_modal_rect(1440., 900., 1.), 1.).0,
    );
    assert!(!w.app.state.worktree_modal.open);
    click(&mut w.app, list.new_workspace);
    assert!(w.app.state.workspace_form.open);
    use daedalus_ui::workspace_form;
    let form = &w.app.state.workspace_form;
    let cancel = form
        .controls(form.modal_rect(1440., 900., 1.))
        .into_iter()
        .find(|(control, _)| *control == workspace_form::Control::Cancel)
        .unwrap()
        .1;
    click(&mut w.app, cancel);
    assert!(!w.app.state.workspace_form.open);
    w.app.handle_ui_action(UiAction::ToggleZoom);
    let zoom = w.app.layout_for(metrics);
    let tab = zoom
        .named_tabs
        .iter()
        .find(|tab| tab.kind.session() == Some(runner))
        .unwrap();
    click(&mut w.app, tab.rect);
    assert_eq!(w.app.state.focused_pane_id(), Some(runner));
    let (_, close) = w.app.layout_for(metrics).close_tab.unwrap();
    click(&mut w.app, close);
    assert!(matches!(w.command(), UiCommand::ClosePane(id) if id == runner));
}

#[test]
fn keyboard_and_ime_route_unicode_to_the_active_editor_and_suppress_modal_confirm_repeats() {
    use daedalus_ui::{settings, setup_review, workspace_form};
    use winit::keyboard::KeyCode;
    let mut w = Workbench::new();
    let ws = workspace("input");
    let pane = ws.panes[0].id;
    w.app
        .handle_daemon_event(UiEvent::WorkspacesLoaded(vec![ws]));
    w.app.commit_text("日本語".into());
    assert!(
        matches!(w.command(), UiCommand::SendInput { pane_id, data} if pane_id == pane && data == "日本語".as_bytes())
    );
    w.app.key_pressed(
        KeyCode::KeyA.into(),
        &Key::Character("a".into()),
        Some("a"),
        false,
    );
    assert!(
        matches!(w.command(), UiCommand::SendInput { pane_id, data} if pane_id == pane && data == b"a")
    );
    w.app.handle_ui_action(UiAction::ToggleSearch);
    w.app.commit_text("état".into());
    assert!(matches!(w.command(), UiCommand::SearchPane { query, .. } if query == "état"));
    w.app.handle_ui_action(UiAction::SearchCancel);
    w.app.handle_ui_action(UiAction::ToggleAgentPicker);
    w.app.commit_text("ignored".into());
    assert!(w.idle());
    w.app.handle_ui_action(UiAction::PickerCancel);
    w.app.handle_ui_action(UiAction::ToggleWorktreeModal);
    w.app.commit_text("étape".into());
    assert_eq!(w.app.state.worktree_modal.branch_input, "étape");
    w.app.handle_ui_action(UiAction::WorktreeModalCancel);
    w.app.handle_ui_action(UiAction::NewWorkspace);
    w.app.commit_text("/projets/été".into());
    assert_eq!(
        w.app
            .state
            .workspace_form
            .value(workspace_form::Control::Folder),
        Some("/projets/été")
    );
    w.app.key_pressed(
        KeyCode::Escape.into(),
        &Key::Named(NamedKey::Escape),
        None,
        false,
    );
    assert!(!w.app.state.workspace_form.open);
    w.app.handle_ui_action(UiAction::WorkspaceSettings);
    w.app.state.settings.set_focus(settings::Control::Name);
    w.app.commit_text("Vérifier".into());
    assert_eq!(
        w.app.state.settings.value(settings::Control::Name),
        Some("codexVérifier")
    );
    w.app.key_pressed(
        KeyCode::Escape.into(),
        &Key::Named(NamedKey::Escape),
        None,
        false,
    );
    assert!(!w.app.state.settings.open);
    w.app
        .state
        .setup_review
        .begin("review".into(), w.root.path().into(), inspection());
    w.app
        .state
        .setup_review
        .set_focus(setup_review::Control::Command(0));
    w.app.commit_text("agent".into());
    assert_eq!(w.app.state.setup_review.command(0), "agent");
    w.app
        .key_pressed(KeyCode::Tab.into(), &Key::Named(NamedKey::Tab), None, false);
    assert_ne!(
        w.app.state.setup_review.focus,
        setup_review::Control::Command(0)
    );
    w.app.handle_setup_action(SetupAction::Cancel);
    let mut failed = workspace("choices");
    failed.panes[2].has_failure_trace = true;
    let mut agent = failed.panes[0].clone();
    agent.id = PaneId::new();
    failed.panes.push(agent);
    let first_agent = failed.panes[0].id;
    w.app.handle_daemon_event(UiEvent::WorkspaceCreated(failed));
    w.app
        .activate_session(HitTarget::SelectSession(first_agent));
    w.app.handle_ui_action(UiAction::ForwardFailureToAgent);
    let old = w.app.state.failure_recipients.as_ref().unwrap().selected;
    w.app.key_pressed(
        KeyCode::ArrowDown.into(),
        &Key::Named(NamedKey::ArrowDown),
        None,
        false,
    );
    assert_ne!(
        old,
        w.app.state.failure_recipients.as_ref().unwrap().selected
    );
    w.app.key_pressed(
        KeyCode::ArrowUp.into(),
        &Key::Named(NamedKey::ArrowUp),
        None,
        false,
    );
    assert_eq!(
        old,
        w.app.state.failure_recipients.as_ref().unwrap().selected
    );
    w.app.commit_text("ignored".into());
    w.app.key_pressed(
        KeyCode::Enter.into(),
        &Key::Named(NamedKey::Enter),
        None,
        true,
    );
    assert!(w.idle());
    w.app.key_pressed(
        KeyCode::Enter.into(),
        &Key::Named(NamedKey::Enter),
        None,
        false,
    );
    assert!(matches!(
        w.command(),
        UiCommand::ForwardRunnerFailureToAgent { .. }
    ));
    assert!(w.app.state.failure_recipients.is_none());
}

#[test]
fn terminal_dimensions_retry_rejected_commands_and_drag_selection_uses_absolute_rows() {
    use daedalus_proto::{GridRow, GridSnapshot};
    use daedalus_ui::selection::{CellPos, Selection};
    let mut w = Workbench::new();
    let ws = workspace("sizing");
    let pane = ws.panes[0].id;
    w.app
        .handle_daemon_event(UiEvent::WorkspacesLoaded(vec![ws]));
    let metrics = ViewportMetrics {
        size: (1440, 900).into(),
        scale_factor: 1.,
        cell: (8., 16.),
    };
    let layout = w.app.layout_for(metrics);
    let agent = layout.pane(PaneRole::Agent).unwrap();
    let transport = w.app.bridge.take();
    w.app.resize_panes(&layout);
    assert!(w.app.state.last_sent_sizes.is_empty());
    w.app.bridge = transport;
    w.app.resize_panes(&layout);
    assert!(
        matches!(w.command(), UiCommand::ResizePane { pane_id, rows, cols}
            if pane_id == pane && (cols, rows) == (agent.cols, agent.rows))
    );
    w.app.resize_panes(&layout);
    assert!(w.idle());
    w.app.handle_daemon_event(UiEvent::Snapshot(GridSnapshot {
        terminal_modes: Default::default(),
        activity: None,
        insight: None,
        pane_id: pane,
        seq: 1,
        cols: agent.cols,
        rows: agent.rows,
        cursor: Default::default(),
        history_generation: 0,
        scrollback_len: 50,
        rows_data: (0..agent.rows)
            .map(|row| GridRow {
                links: Vec::new(),
                wraps: false,
                row_index: row,
                abs_row: 50 + u64::from(row),
                cells: vec![],
            })
            .collect(),
    }));
    w.app.pointer.position = (agent.viewport.x + 8.5, agent.viewport.y + 1.);
    w.app.click_at(metrics);
    assert!(w.app.state.panes[&pane].selection.is_none());
    w.app.pointer.position = (agent.viewport.right() + 100., agent.viewport.y + 32.5);
    w.app.drag_selection(metrics);
    let expected = Selection::new(CellPos::new(1, 50), CellPos::new(agent.cols - 1, 52));
    assert_eq!(w.app.state.panes[&pane].selection, Some(expected));
    w.app.pointer.end_drag();
    w.app.pointer.position = (0., 0.);
    w.app.drag_selection(metrics);
    assert_eq!(w.app.state.panes[&pane].selection, Some(expected));
    w.app.handle_ui_action(UiAction::SendBytes(vec![b'x']));
    assert!(w.app.state.panes[&pane].selection.is_none());
    w.app.handle_daemon_event(UiEvent::Disconnected);
    assert!(w.app.state.last_sent_sizes.is_empty());
}

#[test]
fn terminal_mouse_reports_live_clicks_and_shift_preserves_selection() {
    use daedalus_proto::{
        GridDelta, GridRow, GridSnapshot, MouseTracking, ServerEvent, TerminalModes,
    };
    use daedalus_ui::selection::{CellPos, Selection};
    use winit::keyboard::KeyCode;

    let mut w = Workbench::new();
    let ws = workspace("mouse");
    let pane = ws.panes[0].id;
    w.app
        .handle_daemon_event(UiEvent::WorkspacesLoaded(vec![ws]));
    let metrics = ViewportMetrics {
        size: (1440, 900).into(),
        scale_factor: 1.,
        cell: (8., 16.),
    };
    let layout = w.app.layout_for(metrics);
    let agent = layout.pane(PaneRole::Agent).unwrap();
    w.app.handle_daemon_event(UiEvent::Snapshot(GridSnapshot {
        terminal_modes: TerminalModes {
            mouse: MouseTracking::Button,
            sgr_mouse: true,
            ..TerminalModes::default()
        },
        activity: None,
        insight: None,
        pane_id: pane,
        seq: 1,
        cols: agent.cols,
        rows: agent.rows,
        cursor: Default::default(),
        history_generation: 0,
        scrollback_len: 0,
        rows_data: vec![GridRow {
            links: Vec::new(),
            wraps: false,
            row_index: 0,
            abs_row: 0,
            cells: vec![],
        }],
    }));
    w.app.pointer.position = (agent.viewport.x + 8.5, agent.viewport.y + 1.);
    w.app.click_at(metrics);
    assert!(matches!(w.command(), UiCommand::SendInput { pane_id, data }
        if pane_id == pane && data == b"\x1b[<0;2;1M"));
    w.app.handle_left_mouse_up();
    assert!(matches!(w.command(), UiCommand::SendInput { pane_id, data }
        if pane_id == pane && data == b"\x1b[<0;2;1m"));
    w.app
        .key_pressed(KeyCode::Tab.into(), &Key::Named(NamedKey::Tab), None, false);
    assert!(matches!(w.command(), UiCommand::SendInput { pane_id, data }
        if pane_id == pane && data == b"\t"));

    w.app.modifiers = winit::keyboard::ModifiersState::SHIFT;
    w.app.click_at(metrics);
    w.app.pointer.position = (agent.viewport.x + 24.5, agent.viewport.y + 1.);
    w.app.drag_selection(metrics);
    w.app.handle_left_mouse_up();
    assert_eq!(
        w.app.state.panes[&pane].selection,
        Some(Selection::new(CellPos::new(1, 0), CellPos::new(3, 0)))
    );
    w.app
        .handle_daemon_event(UiEvent::ServerEvent(ServerEvent::GridDelta(GridDelta {
            terminal_modes: TerminalModes::default(),
            activity: None,
            insight: None,
            pane_id: pane,
            seq: 2,
            cols: agent.cols,
            rows: agent.rows,
            cursor: Default::default(),
            history_generation: 0,
            scrollback_len: None,
            dirty_rows: vec![],
        })));
    w.app.modifiers = winit::keyboard::ModifiersState::empty();
    w.app.pointer.position = (agent.viewport.x + 40.5, agent.viewport.y + 1.);
    w.app.click_at(metrics);
    w.app.pointer.position = (agent.viewport.x + 56.5, agent.viewport.y + 1.);
    w.app.drag_selection(metrics);
    w.app.handle_left_mouse_up();
    assert_eq!(
        w.app.state.panes[&pane].selection,
        Some(Selection::new(CellPos::new(5, 0), CellPos::new(7, 0)))
    );
    assert!(w.idle());
}

#[test]
fn daemon_activity_updates_the_visible_workspace_and_invalidates_a_search_when_history_resets() {
    use daedalus_proto::{
        ActivityCursor, ActivityEntry, ActivityKind, ActivityPage, AgentState, ExternalStatus,
        GridDelta, GridRow, ServerEvent, StatusLevel,
    };
    let mut w = Workbench::new();
    let ws = workspace("Activity");
    let id = ws.id;
    let agent = ws.panes[0].id;
    let runner = ws.panes[2].id;
    w.app
        .handle_daemon_event(UiEvent::WorkspacesLoaded(vec![ws]));
    w.app.focused = true;
    w.app.state.recap.focus(true);
    w.app.handle_daemon_event(UiEvent::JournalCapability(true));
    w.app.handle_daemon_event(UiEvent::AgentStateChanged {
        workspace_id: id,
        pane_id: agent,
        state: AgentState::Thinking,
    });
    assert_eq!(
        w.app.state.panes[&agent].summary.agent_state,
        Some(AgentState::Thinking)
    );
    let UiCommand::ReadActivity { token, .. } = w.app.history.request(Instant::now()).unwrap()
    else {
        panic!("expected initial read")
    };
    let page = ActivityPage {
        workspace_id: id,
        cursor: ActivityCursor { epoch: id, seq: 1 },
        gap: true,
        entries: vec![ActivityEntry {
            seq: 1,
            at_ms: 7,
            kind: ActivityKind::Git,
        }],
    };
    w.app.handle_daemon_event(UiEvent::ActivityRead {
        token: token + 1,
        page: page.clone(),
    });
    assert!(
        !w.app
            .state
            .recap
            .lines(id)
            .iter()
            .any(|s| s.contains("partial history"))
    );
    w.app
        .handle_daemon_event(UiEvent::ActivityRead { token, page });
    assert!(
        w.app
            .state
            .recap
            .lines(id)
            .iter()
            .any(|s| s.contains("partial history"))
    );
    w.app.history.presented();
    w.app.handle_daemon_event(UiEvent::WorkspaceRuntimeUpdated {
        workspace_id: id,
        bound_ports: vec![],
        containers: vec![],
        git_status: None,
        git_branch: Some("feature".into()),
    });
    assert_eq!(
        w.app
            .state
            .active_workspace()
            .unwrap()
            .git_branch
            .as_deref(),
        Some("feature")
    );
    let time = Instant::now() + Duration::from_secs(2);
    let UiCommand::ReadActivity { token, cursor, .. } = w.app.history.request(time).unwrap() else {
        panic!("expected activity refresh")
    };
    assert_eq!(cursor.unwrap().seq, 1);
    w.app
        .handle_daemon_event(UiEvent::ActivityReadFailed(token));
    assert!(w.app.history.request(Instant::now()).is_none());
    let UiCommand::ReadActivity { token, .. } = w
        .app
        .history
        .request(time + Duration::from_secs(10))
        .unwrap()
    else {
        panic!("expected retry")
    };
    w.app.handle_daemon_event(UiEvent::ActivityRead {
        token,
        page: ActivityPage {
            workspace_id: id,
            cursor: ActivityCursor { epoch: id, seq: 2 },
            gap: false,
            entries: vec![ActivityEntry {
                seq: 2,
                at_ms: 8,
                kind: ActivityKind::Git,
            }],
        },
    });
    w.app.history.presented();
    w.app
        .handle_daemon_event(UiEvent::ServerEvent(ServerEvent::ExternalStatusUpdated {
            workspace_id: id,
            status: ExternalStatus {
                source: "CI".into(),
                level: StatusLevel::Warn,
                label: "Delayed".into(),
                detail: None,
                updated_at_ms: 8,
            },
        }));
    assert_eq!(
        w.app.state.active_workspace().unwrap().external_statuses[0].label,
        "Delayed"
    );
    w.app.handle_ui_action(UiAction::ToggleSearch);
    w.app.handle_ui_action(UiAction::SearchText("agent".into()));
    let UiCommand::SearchPane { generation, .. } = w.command() else {
        panic!("expected search")
    };
    w.app
        .handle_daemon_event(UiEvent::ServerEvent(ServerEvent::GridDelta(GridDelta {
            terminal_modes: Default::default(),
            activity: None,
            insight: None,
            pane_id: agent,
            seq: 1,
            cols: 80,
            rows: 24,
            cursor: Default::default(),
            dirty_rows: vec![GridRow {
                links: Vec::new(),
                wraps: false,
                row_index: 0,
                abs_row: 0,
                cells: vec![],
            }],
            history_generation: 1,
            scrollback_len: Some(0),
        })));
    assert_eq!(w.app.state.panes[&agent].last_seq, 1);
    assert!(
        matches!(w.command(), UiCommand::SearchPane { generation: next, .. } if next != generation)
    );
    w.app
        .handle_daemon_event(UiEvent::ServerEvent(ServerEvent::ResyncRequired {
            pane_id: agent,
        }));
    assert!(matches!(w.command(), UiCommand::RequestSnapshot { pane_id} if pane_id == agent));
    w.app
        .handle_daemon_event(UiEvent::ServerEvent(ServerEvent::CommandFinished {
            pane_id: runner,
            command: None,
            exit_code: 8,
            has_failure_trace: true,
        }));
    assert!(w.app.state.panes[&runner].summary.has_failure_trace);
    w.app
        .handle_daemon_event(UiEvent::ServerEvent(ServerEvent::PaneExited {
            pane_id: runner,
            exit_code: Some(8),
        }));
    assert_eq!(
        w.app.state.panes[&runner].summary.status,
        PaneStatus::Exited { code: Some(8) }
    );
}

#[test]
fn wheel_gestures_target_hovered_panel_and_modal_without_scrolling_the_terminal_behind_it() {
    let metrics = ViewportMetrics {
        size: (1440, 600).into(),
        scale_factor: 1.,
        cell: (8., 16.),
    };
    let mut w = Workbench::new();
    let workspaces: Vec<_> = (0..24)
        .map(|n| workspace(&format!("project-{n}")))
        .collect();
    let pane = workspaces[0].panes[0].id;
    w.app
        .handle_daemon_event(UiEvent::WorkspacesLoaded(workspaces));
    let layout = w.app.layout_for(metrics);
    let sidebar = layout.sidebar.unwrap();
    w.app.pointer.position = (sidebar.x + 12., sidebar.y + 20.);
    let wheel = |app: &mut DaedalusApp, lines| {
        app.scroll_at(Some(metrics), MouseScrollDelta::LineDelta(0., lines))
    };
    wheel(&mut w.app, -1.);
    assert!(w.app.state.sidebar_first > 0);
    assert!(w.idle());
    wheel(&mut w.app, 1.);
    assert_eq!(w.app.state.sidebar_first, 0);
    let viewport = layout.pane(PaneRole::Agent).unwrap().viewport;
    w.app.pointer.position = (viewport.x + 3., viewport.y + 3.);
    wheel(&mut w.app, 1.);
    assert!(
        matches!(w.command(), UiCommand::RequestScrollback { pane_id, offset_from_bottom, .. }
            if pane_id == pane && offset_from_bottom>0)
    );
    let offset = w.app.state.panes[&pane].requested_scroll_offset;
    w.app.handle_ui_action(UiAction::ToggleAgentPicker);
    wheel(&mut w.app, 20.);
    assert_eq!(w.app.state.picker.selected_index, 0);
    wheel(&mut w.app, -20.);
    assert_eq!(
        w.app.state.picker.selected_index,
        AgentPreset::ALL.len() - 1
    );
    assert_eq!(w.app.state.panes[&pane].requested_scroll_offset, offset);
    w.app.handle_ui_action(UiAction::PickerCancel);
    w.app.handle_ui_action(UiAction::NewWorkspace);
    wheel(&mut w.app, 1.);
    assert!(w.app.wheel_scroll.target.is_none());
    assert!(w.idle());
}

#[test]
fn inspection_failures_leave_the_wizard_editable_and_a_closed_installer_is_rechecked() {
    let mut w = Workbench::new();
    w.app.handle_daemon_event(UiEvent::WorkspacesLoaded(vec![]));
    let save = || WorkspaceFormAction::Save {
        name: "Demo".into(),
        folder: PathBuf::from("/project"),
    };
    w.app.handle_workspace_form(save());
    let UiCommand::InspectSetup { session, .. } = w.command() else {
        panic!("expected inspection")
    };
    w.app.handle_daemon_event(UiEvent::SetupInspected {
        session,
        result: Err("missing folder".into()),
    });
    assert_eq!(
        w.app.state.workspace_form.error.as_deref(),
        Some("missing folder")
    );
    w.app.handle_workspace_form(save());
    let UiCommand::InspectSetup { session, .. } = w.command() else {
        panic!("expected retry")
    };
    w.app.handle_daemon_event(UiEvent::SetupInspected {
        session,
        result: Ok(inspection()),
    });
    w.app.handle_setup_action(SetupAction::Check);
    let UiCommand::InspectSetup { session, .. } = w.command() else {
        panic!("expected tools")
    };
    w.app.handle_daemon_event(UiEvent::SetupInspected {
        session,
        result: Err("unavailable".into()),
    });
    assert_eq!(w.app.state.setup_review.feedback, "unavailable");
    assert!(!w.app.state.setup_review.busy);
    w.app
        .handle_setup_action(SetupAction::Install(vec!["npm".into()]));
    let UiCommand::CreateManualWorkspace { session, .. } = w.command() else {
        panic!("expected creation")
    };
    let mut ws = workspace("Demo");
    let id = ws.id;
    let installer = ws.panes[1].id;
    w.app.handle_daemon_event(UiEvent::ManualWorkspaceCreated {
        session,
        workspace: ws.clone(),
    });
    assert!(
        matches!(w.command(), UiCommand::InstallSetupTool { workspace_id, .. } if workspace_id == id)
    );
    w.app.handle_daemon_event(UiEvent::SetupInstallerFailed {
        workspace_id: id,
        message: "cannot start".into(),
    });
    assert_eq!(w.app.state.setup_review.feedback, "cannot start");
    w.app
        .handle_setup_action(SetupAction::Install(vec!["npm".into()]));
    assert!(
        matches!(w.command(), UiCommand::InstallSetupTool { workspace_id, .. } if workspace_id == id)
    );
    w.app.handle_daemon_event(UiEvent::SetupInstallerStarted {
        workspace_id: id,
        pane_id: installer,
    });
    w.app.handle_daemon_event(UiEvent::Disconnected);
    ws.panes.retain(|p| p.id != installer);
    w.app.handle_daemon_event(UiEvent::Connected);
    w.app
        .handle_daemon_event(UiEvent::WorkspacesLoaded(vec![ws]));
    assert!(!w.app.setup_installer_is_running());
    let UiCommand::InspectSetup { session, .. } = w.command() else {
        panic!("expected refresh after recovery")
    };
    w.app.handle_daemon_event(UiEvent::SetupInspected {
        session,
        result: Ok(inspection()),
    });
    assert!(
        w.app
            .state
            .setup_review
            .feedback
            .contains("terminal closed")
    );
    w.app.handle_daemon_event(UiEvent::Disconnected);
    assert!(w.app.state.setup_review.open);
    assert!(
        w.app
            .state
            .setup_review
            .feedback
            .contains("Connection lost")
    );
    w.app.bridge = None;
    w.app.handle_setup_action(SetupAction::Check);
    assert!(!w.app.state.setup_review.busy);
    assert!(w.app.state.setup_review.feedback.contains("Reconnect"));
}

fn run_in_isolated_profile(test: &str, child: &str) -> bool {
    if std::env::var_os(child).is_some() {
        return true;
    }
    let profile = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env(child, "1")
        .env("DAEDALUS_CONFIG", profile.path().join("config.toml"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // A renamed test matches nothing and still exits 0.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("1 passed"),
        "the isolated run did not execute {test}: {stdout}"
    );
    false
}

fn runner_entry(name: &str) -> daedalus_ui::assignments::Assignment {
    daedalus_ui::assignments::Assignment {
        id: "setup-runner".into(),
        name: name.into(),
        role: PaneRole::Runner,
        argv: vec!["cargo".into(), "test".into()],
    }
}

fn daemon_capabilities(w: &mut Workbench, project_setups: bool) {
    w.app.handle_daemon_event(UiEvent::DaemonCapabilities {
        persistence_health: None,
        home_dir: None,
        project_setups,
        recent_projects: true,
    });
}

#[test]
fn daemon_backed_settings_save_sends_the_setup_and_follows_changes_from_the_daemon() {
    use daedalus_ui::settings::SettingsAction;
    let mut w = Workbench::new();
    daemon_capabilities(&mut w, true);
    let ws = workspace("Daemon");
    let (id, root) = (ws.id, ws.root_dir.clone());
    w.app
        .handle_daemon_event(UiEvent::WorkspacesLoaded(vec![ws]));
    let entry = runner_entry("Suite");
    w.app.handle_ui_action(UiAction::WorkspaceSettings);
    w.app
        .handle_settings_action(SettingsAction::Save(vec![entry.clone(), entry.clone()]));
    assert!(
        w.app
            .state
            .settings
            .error
            .as_deref()
            .unwrap()
            .contains("unique")
    );
    assert!(w.idle());
    w.app
        .handle_settings_action(SettingsAction::Save(vec![entry.clone()]));
    assert!(w.app.state.settings.open);
    let UiCommand::SaveWorkspaceSettings {
        workspace_id,
        assignments,
        request_id,
        ..
    } = w.command()
    else {
        panic!("expected settings save");
    };
    assert_eq!(workspace_id, id);
    assert_eq!(assignments, vec![entry]);
    w.app.handle_daemon_event(UiEvent::WorkspaceSettingsSaved {
        workspace_id: id,
        request_id,
        result: Ok(()),
    });
    assert!(!w.app.state.settings.open);
    let renamed = runner_entry("Renamed");
    w.app.handle_daemon_event(UiEvent::ProjectSetupChanged {
        root_dir: root,
        assignments: vec![renamed.clone()],
    });
    assert!(w.app.state.test_workspaces.contains(&id));
    w.app.handle_ui_action(UiAction::RunTests);
    assert!(
        matches!(w.command(), UiCommand::LaunchAssignment { assignment, .. } if assignment == renamed)
    );
}

#[test]
fn old_daemon_keeps_setups_in_local_files_and_sends_nothing() {
    use daedalus_ui::settings::SettingsAction;
    const CHILD: &str = "DAEDALUS_TEST_OLD_DAEMON_SETUP";
    if !run_in_isolated_profile(
        "controller_tests::old_daemon_keeps_setups_in_local_files_and_sends_nothing",
        CHILD,
    ) {
        return;
    }
    let mut w = Workbench::new();
    daemon_capabilities(&mut w, false);
    let ws = workspace("Old");
    let id = ws.id;
    w.app
        .handle_daemon_event(UiEvent::WorkspacesLoaded(vec![ws]));
    w.app.handle_ui_action(UiAction::WorkspaceSettings);
    let entry = runner_entry("Suite");
    w.app
        .handle_settings_action(SettingsAction::Save(vec![entry.clone()]));
    assert!(w.idle());
    assert_eq!(daedalus_ui::assignments::load(id).unwrap(), vec![entry]);
}

#[test]
fn local_setups_are_sent_to_the_daemon_once_and_the_files_stay() {
    const CHILD: &str = "DAEDALUS_TEST_SETUP_MIGRATION";
    if !run_in_isolated_profile(
        "controller_tests::local_setups_are_sent_to_the_daemon_once_and_the_files_stay",
        CHILD,
    ) {
        return;
    }
    let mut w = Workbench::new();
    let ws = workspace("Migrate");
    let entry = runner_entry("Local");
    daedalus_ui::assignments::save(ws.id, std::slice::from_ref(&entry)).unwrap();
    daemon_capabilities(&mut w, true);
    w.app
        .handle_daemon_event(UiEvent::WorkspacesLoaded(vec![ws.clone()]));
    assert!(
        matches!(w.command(), UiCommand::SaveProjectSetup { workspace_id, assignments, .. }
            if workspace_id == ws.id && assignments == vec![entry.clone()])
    );
    w.app
        .handle_daemon_event(UiEvent::WorkspacesLoaded(vec![ws.clone()]));
    assert!(w.idle());
    assert_eq!(daedalus_ui::assignments::load(ws.id).unwrap(), vec![entry]);
}

#[test]
fn opening_settings_retries_migration_and_ignores_late_rejections() {
    const CHILD: &str = "DAEDALUS_TEST_SETUP_MIGRATION_RETRY";
    if !run_in_isolated_profile(
        "controller_tests::opening_settings_retries_migration_and_ignores_late_rejections",
        CHILD,
    ) {
        return;
    }
    let mut w = Workbench::new();
    let ws = workspace("Retry");
    let id = ws.id;
    let entry = runner_entry("Local");
    daedalus_ui::assignments::save(id, std::slice::from_ref(&entry)).unwrap();
    daemon_capabilities(&mut w, true);
    w.app
        .handle_daemon_event(UiEvent::WorkspacesLoaded(vec![ws.clone()]));
    let UiCommand::SaveProjectSetup {
        request_id: first, ..
    } = w.command()
    else {
        panic!("expected initial migration");
    };
    let failed = |request_id| UiEvent::ProjectSetupMigrationFinished {
        workspace_id: id,
        request_id,
        result: Err("disk full".into()),
    };
    w.app.handle_daemon_event(failed(first));
    let toast = w.app.state.toast.as_ref().unwrap();
    assert!(toast.0.contains("disk full") && toast.1);
    w.app
        .handle_daemon_event(UiEvent::WorkspacesLoaded(vec![ws]));
    assert!(w.idle());
    w.app.handle_ui_action(UiAction::WorkspaceSettings);
    let UiCommand::SaveProjectSetup {
        request_id: retry,
        assignments,
        ..
    } = w.command()
    else {
        panic!("expected settings to retry migration");
    };
    assert_ne!(first, retry);
    assert_eq!(assignments, vec![entry.clone()]);
    w.app.handle_daemon_event(failed(first));
    assert_eq!(w.app.project_setups.load(id).unwrap(), vec![entry.clone()]);
    w.app
        .handle_daemon_event(UiEvent::ProjectSetupMigrationFinished {
            workspace_id: id,
            request_id: retry,
            result: Ok(()),
        });
    w.app.handle_daemon_event(failed(retry));
    assert_eq!(w.app.project_setups.load(id).unwrap(), vec![entry]);
}

#[test]
fn rejected_or_unsent_setup_saves_fall_back_to_the_confirmed_setup() {
    use daedalus_ui::settings::SettingsAction;
    let mut w = Workbench::new();
    daemon_capabilities(&mut w, true);
    let mut ws = workspace("Fallback");
    let confirmed = runner_entry("Confirmed");
    ws.setup = Some(vec![confirmed.clone()]);
    let id = ws.id;
    w.app
        .handle_daemon_event(UiEvent::WorkspacesLoaded(vec![ws]));
    let denied = runner_entry("Denied");
    w.app.handle_ui_action(UiAction::WorkspaceSettings);
    w.app
        .handle_settings_action(SettingsAction::Save(vec![denied.clone()]));
    let UiCommand::SaveWorkspaceSettings { request_id, .. } = w.command() else {
        panic!("expected settings save");
    };
    w.app.handle_daemon_event(UiEvent::WorkspaceSettingsSaved {
        workspace_id: id,
        request_id,
        result: Err("disk full".into()),
    });
    assert!(w.app.state.settings.open);
    assert_eq!(w.app.state.settings.error.as_deref(), Some("disk full"));
    assert_eq!(
        w.app.project_setups.load(id).unwrap(),
        vec![confirmed.clone()]
    );
    w.app.handle_ui_action(UiAction::WorkspaceSettings);
    w.app.bridge = None;
    w.app
        .handle_settings_action(SettingsAction::Save(vec![denied]));
    assert!(
        w.app
            .state
            .settings
            .error
            .as_deref()
            .unwrap()
            .contains("Reconnect")
    );
    assert_eq!(w.app.project_setups.load(id).unwrap(), vec![confirmed]);
}
#[test]
fn pane_setup_conflict_keeps_the_open_settings_draft_and_original_baseline() {
    use daedalus_ui::settings::{Control, SettingsAction};

    let mut w = Workbench::new();
    daemon_capabilities(&mut w, true);
    let original = runner_entry("Original runner");
    let mut ws = workspace("Concurrent settings");
    let id = ws.id;
    let root = ws.root_dir.clone();
    ws.setup = Some(vec![original.clone()]);
    w.app
        .handle_daemon_event(UiEvent::WorkspacesLoaded(vec![ws]));
    w.app.handle_ui_action(UiAction::WorkspaceSettings);
    w.app.state.settings.set_value(Control::Name, "My draft");

    let latest = runner_entry("Other client's runner");
    w.app.handle_daemon_event(UiEvent::ProjectSetupChanged {
        root_dir: root,
        assignments: vec![latest.clone()],
    });
    let submit = runner_entry("My draft");
    w.app
        .handle_settings_action(SettingsAction::Save(vec![submit.clone()]));
    let UiCommand::SaveWorkspaceSettings {
        request_id,
        expected,
        assignments,
        ..
    } = w.command()
    else {
        panic!("expected a conditional save");
    };
    assert_eq!(expected.assignments, Some(vec![original.clone()]));
    assert_eq!(assignments, vec![submit.clone()]);

    w.app.handle_daemon_event(UiEvent::WorkspaceSettingsSaved {
        workspace_id: id,
        request_id,
        result: Err("Pane setup changed on another client.".into()),
    });
    assert!(w.app.state.settings.open);
    assert!(!w.app.state.settings.is_saving());
    assert_eq!(w.app.state.settings.value(Control::Name), Some("My draft"));
    assert_eq!(w.app.project_setups.load(id).unwrap(), vec![latest]);

    w.app
        .handle_settings_action(SettingsAction::Save(vec![submit]));
    assert!(matches!(
        w.command(),
        UiCommand::SaveWorkspaceSettings { expected, .. }
            if expected.assignments == Some(vec![original])
    ));
}

#[test]
fn save_and_launch_waits_for_confirmation_and_preserves_failed_drafts() {
    use daedalus_ui::settings::Control;
    let mut w = Workbench::new();
    daemon_capabilities(&mut w, true);
    let ws = workspace("Retry");
    let id = ws.id;
    w.app
        .handle_daemon_event(UiEvent::WorkspacesLoaded(vec![ws]));
    let entry = runner_entry("Retry me");
    w.app.state.settings.begin(vec![entry.clone()]);

    let launch = w.app.state.settings.activate(Control::Launch);
    w.app.handle_settings_action(launch);
    let UiCommand::SaveWorkspaceSettings { request_id, .. } = w.command() else {
        panic!("expected settings save");
    };
    assert!(w.app.state.settings.open);
    assert!(w.app.state.settings.is_saving());
    assert!(w.idle());

    w.app.handle_daemon_event(UiEvent::WorkspaceSettingsSaved {
        workspace_id: WorkspaceId::new(),
        request_id,
        result: Ok(()),
    });
    assert!(w.app.state.settings.is_saving());
    assert!(w.idle());
    w.app.handle_daemon_event(UiEvent::WorkspaceSettingsSaved {
        workspace_id: id,
        request_id,
        result: Err("disk full".into()),
    });
    assert!(w.app.state.settings.open);
    assert_eq!(w.app.state.settings.error.as_deref(), Some("disk full"));
    assert_eq!(w.app.state.settings.value(Control::Name), Some("Retry me"));
    assert!(w.idle());

    let launch = w.app.state.settings.activate(Control::Launch);
    w.app.handle_settings_action(launch);
    let UiCommand::SaveWorkspaceSettings {
        request_id: retry_id,
        assignments,
        ..
    } = w.command()
    else {
        panic!("expected settings retry");
    };
    assert_ne!(request_id, retry_id);
    assert_eq!(assignments, vec![entry.clone()]);
    w.app.handle_daemon_event(UiEvent::WorkspaceSettingsSaved {
        workspace_id: id,
        request_id,
        result: Ok(()),
    });
    assert!(w.idle());
    w.app.handle_daemon_event(UiEvent::WorkspaceSettingsSaved {
        workspace_id: id,
        request_id: retry_id,
        result: Ok(()),
    });
    assert!(!w.app.state.settings.open);
    assert!(
        matches!(w.command(), UiCommand::LaunchAssignment { workspace_id, assignment, .. }
            if workspace_id == id && assignment == entry)
    );
    assert!(w.idle());
}

fn recent_project(name: &str, root: &str, has_setup: bool) -> daedalus_proto::RecentProject {
    daedalus_proto::RecentProject {
        name: name.into(),
        root_dir: root.into(),
        last_used: 1,
        has_setup,
    }
}

fn form_folder(w: &Workbench) -> String {
    w.app
        .state
        .workspace_form
        .value(daedalus_ui::workspace_form::Control::Folder)
        .unwrap()
        .to_owned()
}

#[test]
fn startup_without_dir_never_uses_working_directory() {
    // The workbench config carries a folder, as a stale working directory would.
    let mut w = Workbench::new();
    w.app.handle_daemon_event(UiEvent::WorkspacesLoaded(vec![]));
    assert!(w.app.state.workspace_form.open);
    assert_eq!(form_folder(&w), "");
    assert!(matches!(
        w.commands.try_recv(),
        Ok(UiCommand::RefreshRecentProjects)
    ));
    w.app.handle_daemon_event(UiEvent::DaemonCapabilities {
        persistence_health: None,
        home_dir: Some("/home/dev".into()),
        project_setups: true,
        recent_projects: true,
    });
    w.app.handle_ui_action(UiAction::NewWorkspace);
    assert_eq!(form_folder(&w), "/home/dev");
    assert!(matches!(
        w.commands.try_recv(),
        Ok(UiCommand::RefreshRecentProjects)
    ));

    let mut w = Workbench::new();
    w.app.config.bootstrap_explicit_dir = true;
    w.app.config.initial_dir = "/p".into();
    w.app.handle_daemon_event(UiEvent::DaemonCapabilities {
        persistence_health: None,
        home_dir: Some("/home/dev".into()),
        project_setups: true,
        recent_projects: true,
    });
    w.app.handle_ui_action(UiAction::NewWorkspace);
    assert_eq!(form_folder(&w), "/p");
}

#[test]
fn worktree_fallback_uses_folder_suggestion() {
    let mut w = Workbench::new();
    w.app.handle_daemon_event(UiEvent::DaemonCapabilities {
        persistence_health: None,
        home_dir: Some("/home/dev".into()),
        project_setups: true,
        recent_projects: true,
    });
    w.app.handle_daemon_event(UiEvent::WorkspacesLoaded(vec![]));
    w.app.handle_workspace_form(WorkspaceFormAction::Cancel);
    w.app.handle_ui_action(UiAction::ToggleWorktreeModal);
    w.app
        .handle_ui_action(UiAction::WorktreeModalSetText("topic".into()));
    w.app.handle_ui_action(UiAction::WorktreeModalConfirm);
    assert!(matches!(
        w.command(),
        UiCommand::CreateWorktreeWorkspace { repo_dir, branch, .. }
            if repo_dir == std::path::Path::new("/home/dev") && branch == "topic"
    ));
}

fn stored_setup() -> Vec<daedalus_ui::assignments::Assignment> {
    use daedalus_ui::assignments::Assignment;
    let entry = |id: &str, role| Assignment {
        id: id.into(),
        name: id.into(),
        role,
        argv: vec![id.into()],
    };
    vec![
        entry("agent", PaneRole::Agent),
        entry("tests", PaneRole::Runner),
        entry("server", PaneRole::Service),
        entry("reviewer", PaneRole::Agent),
    ]
}

/// A freshly started UI: it never saw `/project` open, and the daemon lists it as recent.
fn start_screen_with_recent(w: &mut Workbench, has_setup: bool) {
    w.app.handle_daemon_event(UiEvent::DaemonCapabilities {
        persistence_health: None,
        home_dir: None,
        project_setups: true,
        recent_projects: true,
    });
    w.app.handle_daemon_event(UiEvent::WorkspacesLoaded(vec![]));
    w.app
        .handle_daemon_event(UiEvent::RecentProjectsLoaded(vec![
            recent_project("Project", "/project", has_setup),
            recent_project("Other", "/other", false),
        ]));
    assert!(matches!(
        w.commands.try_recv(),
        Ok(UiCommand::RefreshRecentProjects)
    ));
    assert_eq!(w.app.state.workspace_form.recent().len(), 2);
}

/// Reopens the first recent project and answers with the workspace the daemon created.
fn reopen_first(
    w: &mut Workbench,
    setup: Option<Vec<daedalus_ui::assignments::Assignment>>,
) -> WorkspaceInfo {
    use daedalus_ui::workspace_form::Control;
    w.app.state.workspace_form.set_focus(Control::Recent(0));
    w.app.workspace_form_key(&Key::Named(NamedKey::Enter));
    let UiCommand::CreateManualWorkspace {
        session,
        name,
        root_dir,
    } = w.command()
    else {
        panic!("expected workspace creation")
    };
    assert_eq!(
        (name.as_str(), root_dir.as_path()),
        ("Project", std::path::Path::new("/project"))
    );
    assert!(w.app.state.workspace_form.submitting());
    assert!(!w.app.state.setup_review.open);
    let mut ws = workspace("Project");
    ws.setup = setup;
    w.app.handle_daemon_event(UiEvent::ManualWorkspaceCreated {
        session,
        workspace: ws.clone(),
    });
    ws
}

#[test]
fn reopen_with_setup_creates_and_launches_agents_only() {
    let mut w = Workbench::new();
    start_screen_with_recent(&mut w, true);
    // The setup only arrives on the workspace the daemon creates.
    let ws = reopen_first(&mut w, Some(stored_setup()));
    assert!(!w.app.state.workspace_form.open);
    let UiCommand::LaunchStoredSetup(workspace_id) = w.command() else {
        panic!("expected a launch without saving setup")
    };
    assert_eq!(workspace_id, ws.id);
    w.app.handle_daemon_event(UiEvent::SetupApplied {
        workspace_id,
        error: None,
    });
    assert_eq!(w.app.state.active_workspace_id, Some(ws.id));
    assert!(!w.app.state.setup_review.open);
}

#[test]
fn reopen_whose_created_workspace_has_no_setup_reviews_that_workspace() {
    let mut w = Workbench::new();
    start_screen_with_recent(&mut w, true);
    let ws = reopen_first(&mut w, None);
    let UiCommand::InspectSetup {
        session, root_dir, ..
    } = w.command()
    else {
        panic!("expected the environment review")
    };
    assert_eq!(root_dir, std::path::Path::new("/project"));
    w.app.handle_daemon_event(UiEvent::SetupInspected {
        session,
        result: Ok(inspection()),
    });
    assert!(w.app.state.setup_review.open);
    assert!(!w.app.state.workspace_form.open);
    assert_eq!(w.app.state.setup_review.name, "Project");
    // Opening the review applies to the workspace that already exists.
    w.app.handle_setup_action(SetupAction::Open);
    assert!(matches!(
        w.command(),
        UiCommand::ApplySetup { workspace_id, .. } if workspace_id == ws.id
    ));
}

#[test]
fn reopen_without_setup_opens_review() {
    let mut w = Workbench::new();
    start_screen_with_recent(&mut w, false);
    let projects = w.app.state.workspace_form.recent().to_vec();
    for project in projects {
        w.app
            .handle_workspace_form(WorkspaceFormAction::Reopen(project.clone()));
        let UiCommand::InspectSetup {
            session, root_dir, ..
        } = w.command()
        else {
            panic!("expected the environment review for {}", project.name)
        };
        assert_eq!(root_dir, project.root_dir);
        w.app.handle_daemon_event(UiEvent::SetupInspected {
            session,
            result: Ok(inspection()),
        });
        assert!(w.app.state.setup_review.open);
        assert_eq!(w.app.state.setup_review.name, project.name);
        w.app.handle_setup_action(SetupAction::Cancel);
        w.app.handle_ui_action(UiAction::NewWorkspace);
        assert!(matches!(
            w.commands.try_recv(),
            Ok(UiCommand::RefreshRecentProjects)
        ));
    }
}

#[test]
fn reopen_failures_are_reported_where_the_user_is() {
    use daedalus_ui::workspace_form::Control;
    let mut w = Workbench::new();
    start_screen_with_recent(&mut w, true);
    let project = w.app.state.workspace_form.recent()[0].clone();
    w.app
        .handle_workspace_form(WorkspaceFormAction::Reopen(project.clone()));
    let UiCommand::CreateManualWorkspace { session, .. } = w.command() else {
        panic!("expected workspace creation")
    };
    w.app.handle_daemon_event(UiEvent::ManualWorkspaceFailed {
        session,
        message: "occupied".into(),
    });
    let form = &w.app.state.workspace_form;
    assert!(form.open && !form.submitting());
    assert_eq!(form.error.as_deref(), Some("occupied"));
    assert!(form.enabled(Control::Recent(0)));

    w.app
        .handle_workspace_form(WorkspaceFormAction::Reopen(project.clone()));
    let UiCommand::CreateManualWorkspace { session, .. } = w.command() else {
        panic!("expected workspace creation")
    };
    let mut ws = workspace("Project");
    ws.setup = Some(stored_setup());
    w.app.handle_daemon_event(UiEvent::ManualWorkspaceCreated {
        session,
        workspace: ws.clone(),
    });
    assert!(matches!(w.command(), UiCommand::LaunchStoredSetup(_)));
    w.app.handle_daemon_event(UiEvent::SetupApplied {
        workspace_id: ws.id,
        error: Some("agent missing".into()),
    });
    assert!(!w.app.state.setup_review.open);
    assert!(matches!(&w.app.state.toast, Some((message, true, _)) if message == "agent missing"));

    w.app.handle_ui_action(UiAction::NewWorkspace);
    w.app.bridge = None;
    w.app
        .handle_workspace_form(WorkspaceFormAction::Reopen(project));
    assert_eq!(
        w.app.state.workspace_form.error.as_deref(),
        Some("Reconnect to open this project.")
    );
}

/// Reopens the first recent project, dismisses the form and opens it again before the
/// daemon answers, then answers with the created workspace.
fn reopen_dismissed_then_form_reopened(
    w: &mut Workbench,
    setup: Option<Vec<daedalus_ui::assignments::Assignment>>,
) -> WorkspaceInfo {
    use daedalus_ui::workspace_form::Control;
    w.app.state.workspace_form.set_focus(Control::Recent(0));
    w.app.workspace_form_key(&Key::Named(NamedKey::Enter));
    let UiCommand::CreateManualWorkspace { session, .. } = w.command() else {
        panic!("expected workspace creation")
    };
    w.app.handle_workspace_form(WorkspaceFormAction::Cancel);
    w.app.handle_ui_action(UiAction::NewWorkspace);
    assert!(w.app.state.workspace_form.open);
    let mut ws = workspace("Project");
    ws.setup = setup;
    w.app.handle_daemon_event(UiEvent::ManualWorkspaceCreated {
        session,
        workspace: ws.clone(),
    });
    ws
}

#[test]
fn dismissed_reopen_still_applies_the_stored_setup() {
    let mut w = Workbench::new();
    start_screen_with_recent(&mut w, true);
    let ws = reopen_dismissed_then_form_reopened(&mut w, Some(stored_setup()));
    let applied = std::iter::from_fn(|| w.commands.try_recv().ok())
        .find(|command| matches!(command, UiCommand::LaunchStoredSetup(_)));
    let Some(UiCommand::LaunchStoredSetup(workspace_id)) = applied else {
        panic!("the created workspace must get its setup")
    };
    assert_eq!(workspace_id, ws.id);
}

#[test]
fn dismissed_reopen_without_setup_still_reviews_the_workspace() {
    let mut w = Workbench::new();
    start_screen_with_recent(&mut w, true);
    let ws = reopen_dismissed_then_form_reopened(&mut w, None);
    let inspect = std::iter::from_fn(|| w.commands.try_recv().ok())
        .find(|command| matches!(command, UiCommand::InspectSetup { .. }));
    let Some(UiCommand::InspectSetup {
        session, root_dir, ..
    }) = inspect
    else {
        panic!("the created workspace must be reviewed, not left empty")
    };
    assert_eq!(root_dir, ws.root_dir);
    w.app.handle_daemon_event(UiEvent::SetupInspected {
        session,
        result: Ok(inspection()),
    });
    assert!(w.app.state.setup_review.open);
}

#[test]
fn dismissed_reopen_failure_is_still_reported() {
    let mut w = Workbench::new();
    start_screen_with_recent(&mut w, true);
    use daedalus_ui::workspace_form::Control;
    w.app.state.workspace_form.set_focus(Control::Recent(0));
    w.app.workspace_form_key(&Key::Named(NamedKey::Enter));
    let UiCommand::CreateManualWorkspace { session, .. } = w.command() else {
        panic!("expected workspace creation")
    };
    w.app.handle_workspace_form(WorkspaceFormAction::Cancel);
    w.app.handle_ui_action(UiAction::NewWorkspace);
    w.app.handle_daemon_event(UiEvent::ManualWorkspaceFailed {
        session,
        message: "occupied".into(),
    });
    assert!(matches!(&w.app.state.toast, Some((message, true, _)) if message == "occupied"));
}

#[test]
fn reopened_project_is_not_offered_again_while_the_refresh_is_pending() {
    let mut w = Workbench::new();
    start_screen_with_recent(&mut w, true);
    reopen_first(&mut w, Some(stored_setup()));
    // Opening the form again keeps the old list until the daemon answers.
    w.app.handle_ui_action(UiAction::NewWorkspace);
    let listed: Vec<_> = w
        .app
        .state
        .workspace_form
        .recent()
        .iter()
        .map(|project| project.name.as_str())
        .collect();
    assert_eq!(listed, ["Other"]);
    // A late answer that still lists the open project cannot bring it back.
    w.app
        .handle_daemon_event(UiEvent::RecentProjectsLoaded(vec![
            recent_project("Project", "/project", true),
            recent_project("Other", "/other", false),
        ]));
    assert_eq!(w.app.state.workspace_form.recent().len(), 1);
}

#[test]
fn recent_list_is_cleared_when_the_daemon_has_no_recent_projects() {
    let mut w = Workbench::new();
    start_screen_with_recent(&mut w, true);
    assert_eq!(w.app.state.workspace_form.recent().len(), 2);
    w.app.handle_daemon_event(UiEvent::DaemonCapabilities {
        persistence_health: None,
        home_dir: None,
        project_setups: false,
        recent_projects: false,
    });
    assert!(w.app.state.workspace_form.recent().is_empty());
}

#[test]
fn draggable_gutters_resize_terminal_cells_without_changing_focus() {
    let mut w = Workbench::new();
    w.app
        .handle_daemon_event(UiEvent::WorkspacesLoaded(vec![workspace("pairing")]));
    w.app.state.zoomed = false;
    let focus = w.app.state.focused_pane_id();
    for (width, first, second, vertical) in [
        (1440, PaneRole::Agent, PaneRole::Service, true),
        (1440, PaneRole::Service, PaneRole::Runner, false),
        (480, PaneRole::Agent, PaneRole::Service, false),
        (480, PaneRole::Service, PaneRole::Runner, false),
    ] {
        let metrics = ViewportMetrics {
            size: (width, 900).into(),
            scale_factor: 1.,
            cell: (8., 16.),
        };
        let layout = w.app.layout_for(metrics);
        let a = *layout.pane(first).unwrap();
        let b = layout.pane(second).unwrap().outer;
        let (x, y) = if vertical {
            ((a.outer.right() + b.x) * 0.5, b.y + b.height * 0.5)
        } else {
            (b.x + b.width * 0.5, (a.outer.bottom() + b.y) * 0.5)
        };
        w.app.handle_cursor_moved(x, y);
        w.app.click_at(metrics);
        if vertical {
            w.app.handle_cursor_moved(x + 60., y);
        } else {
            w.app.handle_cursor_moved(x, y + 50.);
        }
        let changed = *w.app.layout_for(metrics).pane(first).unwrap();
        assert!(if vertical {
            changed.cols > a.cols
        } else {
            changed.rows > a.rows
        });
        w.app.pointer.end_drag();
        w.app.handle_cursor_moved(x, y);
        assert_eq!(w.app.layout_for(metrics).pane(first).unwrap(), &changed);
        assert_eq!(w.app.state.focused_pane_id(), focus);
    }
}

#[test]
fn native_workspace_selection_disambiguates_names_restores_focus_and_rejects_stale_controls() {
    use accesskit::{Action, ActionRequest, TreeId};

    let mut w = Workbench::new();
    let mut first = workspace("Same name");
    first.root_dir = "/first".into();
    let selected_pane = first.panes[1].id;
    let mut second = workspace("Same name");
    second.root_dir = "/second".into();
    w.app.handle_daemon_event(UiEvent::WorkspacesLoaded(vec![
        first.clone(),
        second.clone(),
    ]));
    w.app.state.select_workspace(first.id);
    w.app
        .state
        .select_session(first.id, PaneRole::Service, selected_pane);
    let tree =
        accessibility::workbench_tree(&w.app.state, 1200., 700., 1., &mut w.app.terminal_controls);
    let node = |path| {
        tree.nodes
            .iter()
            .find(|(_, node)| node.label() == Some(path))
            .unwrap()
            .0
    };
    let first_node = node("Same name · /first");
    let second_node = node("Same name · /second");
    let click = |target_node| ActionRequest {
        target_tree: TreeId::ROOT,
        target_node,
        action: Action::Click,
        data: None,
    };
    for (node, workspace) in [(second_node, second.id), (first_node, first.id)] {
        let action = accessibility::action(&w.app.state, &click(node), &w.app.terminal_controls)
            .expect("workspace action missing");
        w.app.handle_ui_action(action);
        assert_eq!(w.app.state.active_workspace_id, Some(workspace));
    }
    assert_eq!(w.app.state.focused_pane_id(), Some(selected_pane));
    w.app
        .handle_daemon_event(UiEvent::WorkspacesLoaded(vec![first]));
    assert!(
        accessibility::action(&w.app.state, &click(second_node), &w.app.terminal_controls)
            .is_none()
    );
}

#[test]
fn image_preview_button_does_not_report_terminal_motion() {
    use daedalus_proto::{Cell, GridRow, MouseTracking};
    use daedalus_ui::selection::{CellPos, Selection};

    let mut w = Workbench::new();
    let ws = workspace("Preview");
    let pane = ws.panes[0].id;
    w.app
        .handle_daemon_event(UiEvent::WorkspacesLoaded(vec![ws]));
    let view = w.app.state.panes.get_mut(&pane).unwrap();
    view.terminal_modes.mouse = MouseTracking::Any;
    view.terminal_modes.sgr_mouse = true;
    view.selection = Some(Selection::new(CellPos::new(0, 0), CellPos::new(8, 0)));
    view.viewport_rows = vec![GridRow {
        row_index: 0,
        abs_row: 0,
        wraps: false,
        links: vec![],
        cells: "image.png"
            .chars()
            .map(|c| Cell {
                text: c.to_string(),
                ..Default::default()
            })
            .collect(),
    }];
    let metrics = ViewportMetrics {
        size: (1200, 700).into(),
        scale_factor: 1.,
        cell: (8., 16.),
    };
    let button = w
        .app
        .layout_for(metrics)
        .preview_button(&w.app.state, 1.)
        .unwrap();
    w.app
        .move_pointer(button.x + 2., button.y + 2., Some(metrics));
    assert!(w.idle(), "the image button sent terminal input");
    w.app
        .move_pointer(button.x - 10., button.y + 2., Some(metrics));
    assert!(matches!(w.command(), UiCommand::SendInput { pane_id, .. } if pane_id == pane));
}

#[test]
fn selected_png_preview_preserves_terminal_focus_selection_and_scroll() {
    use daedalus_proto::{Cell, GridRow, rpc::ImagePreview};
    use daedalus_ui::selection::{CellPos, Selection};
    let mut w = Workbench::new();
    let ws = workspace("Preview");
    let pane = ws.panes[0].id;
    let workspace_id = ws.id;
    w.app
        .handle_daemon_event(UiEvent::WorkspacesLoaded(vec![ws]));
    w.app.handle_ui_action(UiAction::PreviewImage);
    assert!(w.idle());
    let selection = Selection::new(CellPos::new(0, 3), CellPos::new(8, 3));
    let view = w.app.state.panes.get_mut(&pane).unwrap();
    view.scroll_offset = 4;
    view.requested_scroll_offset = 4;
    view.selection = Some(selection);
    view.scrolled_rows = Some(vec![GridRow {
        row_index: 0,
        abs_row: 3,
        wraps: false,
        links: vec![],
        cells: "image.png"
            .chars()
            .map(|c| Cell {
                text: c.to_string(),
                ..Default::default()
            })
            .collect(),
    }]);
    let metrics = ViewportMetrics {
        size: (1200, 700).into(),
        scale_factor: 1.,
        cell: (8., 16.),
    };
    let button = w
        .app
        .layout_for(metrics)
        .preview_button(&w.app.state, 1.)
        .unwrap();
    w.app.pointer.position = (button.x + 1., button.y + 1.);
    w.app.click_at(metrics);
    let UiCommand::PreviewImage {
        workspace_id: ws,
        pane_id,
        token: older,
        path,
    } = w.command()
    else {
        panic!("expected image preview request");
    };
    assert_eq!(
        (ws, pane_id, path.as_str()),
        (workspace_id, pane, "image.png")
    );
    w.app.handle_ui_action(UiAction::PreviewImage);
    let UiCommand::PreviewImage { token, .. } = w.command() else {
        panic!("expected replacement image preview request");
    };
    let image = ImagePreview {
        width: 3,
        height: 1,
        rgba: vec![255, 0, 0, 255, 255, 0, 0, 255, 0, 255, 0, 255],
    };
    w.app.handle_daemon_event(UiEvent::ImagePreview {
        pane_id: PaneId::new(),
        token,
        result: Ok(image.clone()),
    });
    w.app.handle_daemon_event(UiEvent::ImagePreview {
        pane_id: pane,
        token: older,
        result: Ok(image.clone()),
    });
    assert!(w.app.state.image_preview.is_none());
    w.app.handle_daemon_event(UiEvent::ImagePreview {
        pane_id: pane,
        token,
        result: Ok(image),
    });
    for width in [320, 1440] {
        let metrics = ViewportMetrics {
            size: (width, 700).into(),
            scale_factor: 1.,
            cell: (8., 16.),
        };
        let layout = w.app.layout_for(metrics);
        let panel = layout.preview_panel(&w.app.state, 1.).unwrap();
        let view = layout.pane(PaneRole::Agent).unwrap().viewport;
        assert!(panel.x > view.x && panel.right() <= view.right() + 0.01);
        let (quads, base) = daedalus_ui::renderer::collect_quads(
            &w.app.state,
            &layout,
            &daedalus_ui::theme::Theme::dark(),
            width as f32,
            700.,
            1.,
            (8., 16.),
        );
        assert!(
            quads[base as usize..]
                .iter()
                .any(|q| q.color == [1., 0., 0., 1.])
        );
        w.app.pointer.position = (panel.x + 1., panel.y + 1.);
        w.app
            .scroll_at(Some(metrics), MouseScrollDelta::LineDelta(0., -1.));
    }
    w.app.handle_ui_action(UiAction::PreviewImage);
    assert!(w.app.state.image_preview.is_none());
    assert_eq!(w.app.state.focused_pane_id(), Some(pane));
    let view = &w.app.state.panes[&pane];
    assert_eq!(
        (
            view.scroll_offset,
            view.requested_scroll_offset,
            view.selection
        ),
        (4, 4, Some(selection))
    );
    assert!(w.idle());
}

#[test]
fn terminal_activity_restores_title_and_only_marks_new_background_attention() {
    use daedalus_proto::{GridDelta, GridSnapshot, ServerEvent, types::TerminalActivity};
    let mut w = Workbench::new();
    let ws = workspace("Attention");
    let agent = ws.panes[0].id;
    w.app
        .handle_daemon_event(UiEvent::WorkspacesLoaded(vec![ws]));
    w.app.state.session_names.insert(agent, "Reviewer".into());
    let activity = |seq, title: &str| {
        Some(TerminalActivity {
            title: title.into(),
            attention_seq: seq,
        })
    };
    let snapshot = GridSnapshot {
        terminal_modes: Default::default(),
        pane_id: agent,
        seq: 1,
        cols: 80,
        rows: 24,
        cursor: Default::default(),
        rows_data: vec![],
        history_generation: 0,
        scrollback_len: 0,
        insight: None,
        activity: activity(3, "◐ Claude Code"),
    };
    w.app
        .handle_daemon_event(UiEvent::Snapshot(snapshot.clone()));
    assert_eq!(w.app.state.panes[&agent].tab_indicator(false), Some('◐'));
    assert_eq!(w.app.state.panes[&agent].tab_indicator(true), Some('•'));
    assert_eq!(w.app.state.session_names[&agent], "Reviewer");
    let delta = |seq, attention| {
        UiEvent::ServerEvent(ServerEvent::GridDelta(GridDelta {
            terminal_modes: Default::default(),
            pane_id: agent,
            seq,
            activity: activity(attention, "✳ Claude Code"),
            cols: 80,
            rows: 24,
            cursor: Default::default(),
            dirty_rows: vec![],
            history_generation: 0,
            scrollback_len: None,
            insight: None,
        }))
    };
    w.app.ui_config.notification_ping = true;
    w.app
        .state
        .panes
        .get_mut(&agent)
        .unwrap()
        .summary
        .agent_state = Some(daedalus_proto::AgentState::Thinking);
    w.app.handle_daemon_event(delta(2, 4));
    assert_eq!(w.app.state.panes[&agent].tab_indicator(false), Some('●'));
    w.app.focused = true;
    w.app.sync_history_visit();
    assert_eq!(w.app.state.panes[&agent].tab_indicator(false), None);
    w.app.focused = false;
    w.app.handle_daemon_event(delta(2, 4));
    w.app.handle_daemon_event(delta(3, 4));
    assert!(!w.app.state.panes[&agent].unread_attention);
    w.app.handle_daemon_event(UiEvent::Snapshot(GridSnapshot {
        seq: 6,
        activity: activity(6, "codex"),
        ..snapshot
    }));
    assert!(!w.app.state.panes[&agent].unread_attention);
}

#[test]
fn cargo_bar_runs_on_the_selected_workspace_without_taking_agent_focus() {
    use daedalus_ui::assignments::CargoPreset;

    let mut w = Workbench::new();
    let mut ws = workspace("Rust project");
    let id = ws.id;
    let agent = ws.panes[0].id;

    let viewport = |height| ViewportMetrics {
        size: winit::dpi::PhysicalSize::new(1000, height),
        scale_factor: 1.,
        cell: (8., 19.),
    };
    w.app.state.set_workspaces(vec![ws.clone()]);
    assert!(w.app.layout_for(viewport(570)).cargo_bar.is_none());
    w.app
        .handle_ui_action(UiAction::RunCargo(CargoPreset::Test));
    assert!(w.idle());

    ws.project.tools.push("Cargo".into());
    w.app.state.set_workspaces(vec![ws]);
    let layout = w.app.layout_for(viewport(570));
    let bar = layout.cargo_bar.expect("Cargo footer");
    assert_eq!(bar.bottom(), 570.);
    assert!(layout.panes.iter().all(|pane| pane.outer.bottom() <= bar.y));
    assert!(w.app.layout_for(viewport(140)).cargo_bar.is_none());
    for preset in CargoPreset::ALL {
        w.app.handle_ui_action(UiAction::RunCargo(preset));
        let UiCommand::LaunchAssignment {
            workspace_id,
            assignment,
            ..
        } = w.command()
        else {
            panic!("expected Cargo runner launch");
        };
        assert_eq!(workspace_id, id);
        assert_eq!(assignment.argv, ["cargo", preset.command(), "--workspace"]);

        let mut runner = workspace("Runner").panes.remove(0);
        runner.workspace_id = id;
        runner.role = PaneRole::Runner;
        let pane = runner.id;
        w.app.handle_daemon_event(UiEvent::PaneSpawned {
            summary: runner,
            select_tab: false,
        });
        w.app.handle_daemon_event(UiEvent::AssignmentStarted {
            workspace_id: id,
            pane_id: pane,
            assignment,
        });

        assert_eq!(w.app.state.focused_pane_id(), Some(agent));
        assert_eq!(w.app.state.pane_id_for_role(PaneRole::Runner), Some(pane));
        assert_eq!(w.app.state.panes[&pane].cargo_preset, Some(preset));
    }
    w.app.bridge = None;
    w.app
        .handle_ui_action(UiAction::RunCargo(CargoPreset::Check));
    assert_eq!(
        w.app.state.toast.as_ref().unwrap().0,
        "Reconnect to run Cargo"
    );
}

#[cfg(target_os = "windows")]
#[test]
fn windows_copy_with_no_selection_never_sends_terminal_input() {
    use winit::keyboard::{KeyCode, PhysicalKey};

    let mut workbench = Workbench::new();
    let workspace = workspace("Windows clipboard");
    let pane_id = workspace.panes[0].id;
    workbench.app.state.set_workspaces(vec![workspace]);
    workbench.app.modifiers = ModifiersState::CONTROL;
    workbench.app.key_pressed(
        PhysicalKey::Code(KeyCode::KeyC),
        &Key::Character("c".into()),
        Some("\x03"),
        false,
    );
    assert!(workbench.idle());

    workbench.app.key_pressed(
        PhysicalKey::Code(KeyCode::Period),
        &Key::Character(".".into()),
        None,
        false,
    );
    assert!(matches!(
        workbench.command(),
        UiCommand::SendInput { pane_id: target, data } if target == pane_id && data == b"\x03"
    ));
}

#[test]
fn ime_commit_reaches_form_opened_by_native_action_with_recap_selected() {
    use accesskit::{Action, ActionRequest, TreeId};
    use daedalus_ui::{recap::RecapEntry, workspace_form::Control};

    let mut w = Workbench::new();
    w.app
        .handle_daemon_event(UiEvent::WorkspacesLoaded(vec![workspace("Recap")]));
    w.app.state.recap.keyboard_row = Some(RecapEntry::from("Runner completed"));
    let tree =
        accessibility::workbench_tree(&w.app.state, 900., 600., 1., &mut w.app.terminal_controls);
    let node = tree
        .nodes
        .iter()
        .find(|(_, node)| node.label() == Some("New workspace"))
        .unwrap()
        .0;
    let request = ActionRequest {
        target_tree: TreeId::ROOT,
        target_node: node,
        action: Action::Click,
        data: None,
    };
    let action = accessibility::action(&w.app.state, &request, &w.app.terminal_controls).unwrap();
    w.app.handle_ui_action(action);
    assert!(w.app.state.workspace_form.open);
    assert!(w.app.state.recap.keyboard_row.is_some());
    w.app.commit_text("日本語".into());
    assert!(
        w.app
            .state
            .workspace_form
            .value(Control::Folder)
            .unwrap()
            .contains("日本語"),
        "the active form lost committed Japanese text"
    );
}

#[test]
fn temporary_persistence_warning_clears_on_healthy_or_legacy_reconnect() {
    use daedalus_proto::PersistenceHealth::{Durable, TemporarySnapshotRejected};
    let mut w = Workbench::new();
    for health in [
        Some(TemporarySnapshotRejected),
        Some(Durable),
        Some(TemporarySnapshotRejected),
        None,
    ] {
        w.app.handle_daemon_event(UiEvent::Disconnected);
        w.app.handle_daemon_event(UiEvent::DaemonCapabilities {
            persistence_health: health,
            home_dir: None,
            recent_projects: false,
            project_setups: false,
        });
        w.app.handle_daemon_event(UiEvent::Connected);
        assert_eq!(
            w.app.has_top_banner(),
            health == Some(TemporarySnapshotRejected)
        );
    }
}
