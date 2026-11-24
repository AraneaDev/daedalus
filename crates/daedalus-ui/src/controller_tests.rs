use super::*;
use daedalus_proto::{PaneStatus, PaneSummary, SetupInspection, WorkspaceInfo};
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
    let ws = workspace("Workbench");
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
    w.app
        .handle_daemon_event(UiEvent::WorkspacesLoaded(vec![ws]));
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
        matches!(w.command(), UiCommand::ApplySetup { workspace_id, assignments, .. }
            if workspace_id == id && assignments.iter().any(|a| a.id == "setup-runner" && a.argv == ["cargo", "test"]))
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
        insight: None,
        pane_id: pane,
        seq: 1,
        cols: 80,
        rows: 24,
        cursor: Default::default(),
        rows_data: (0..24)
            .map(|n| GridRow {
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
    w.app.handle_daemon_event(UiEvent::PaneSpawned {
        summary: second,
        agent_preset: Some(AgentPreset::Claude),
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
        matches!(w.command(), UiCommand::LaunchAssignment { workspace_id, assignment, cols, rows}
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
        .find(|(id, _, _)| *id == runner)
        .unwrap();
    click(&mut w.app, tab.2);
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
            insight: None,
            pane_id: agent,
            seq: 1,
            cols: 80,
            rows: 24,
            cursor: Default::default(),
            dirty_rows: vec![GridRow {
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
    assert!(!w.app.state.settings.open);
    assert!(
        matches!(w.command(), UiCommand::SaveProjectSetup { workspace_id, assignments }
            if workspace_id == id && assignments == vec![entry.clone()])
    );
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
    w.app.handle_daemon_event(UiEvent::ProjectSetupSaveFailed {
        workspace_id: id,
        message: "disk full".into(),
    });
    let toast = w.app.state.toast.as_ref().unwrap();
    assert!(toast.0.contains("disk full") && toast.1);
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
        matches!(w.command(), UiCommand::SaveProjectSetup { workspace_id, assignments }
            if workspace_id == ws.id && assignments == vec![entry.clone()])
    );
    w.app
        .handle_daemon_event(UiEvent::WorkspacesLoaded(vec![ws.clone()]));
    assert!(w.idle());
    assert_eq!(daedalus_ui::assignments::load(ws.id).unwrap(), vec![entry]);
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
    assert!(matches!(w.command(), UiCommand::SaveProjectSetup { .. }));
    w.app.handle_daemon_event(UiEvent::ProjectSetupSaveFailed {
        workspace_id: id,
        message: "no".into(),
    });
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
    let UiCommand::ApplySetup {
        workspace_id,
        assignments,
        launch,
    } = w.command()
    else {
        panic!("expected the stored setup")
    };
    assert_eq!(workspace_id, ws.id);
    assert_eq!(assignments, stored_setup());
    assert_eq!(launch, ["agent", "reviewer"]);
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
    assert!(matches!(w.command(), UiCommand::ApplySetup { .. }));
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
        .find(|command| matches!(command, UiCommand::ApplySetup { .. }));
    let Some(UiCommand::ApplySetup {
        workspace_id,
        assignments,
        launch,
    }) = applied
    else {
        panic!("the created workspace must get its setup")
    };
    assert_eq!(workspace_id, ws.id);
    assert_eq!(assignments, stored_setup());
    assert_eq!(launch, ["agent", "reviewer"]);
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
        home_dir: None,
        project_setups: false,
        recent_projects: false,
    });
    assert!(w.app.state.workspace_form.recent().is_empty());
}
