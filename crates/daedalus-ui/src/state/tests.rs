use super::test_support::*;
use super::*;
use daedalus_proto::{CursorState, GridDelta};
use std::path::PathBuf;

#[test]
fn recap_targets_named_siblings_independently_and_handles_closed_panes() {
    use daedalus_proto::{ActivityCursor, ActivityEntry, ActivityKind, ActivityPage};
    let (mut ws, first, _, _) = make_workspace_with_three_panes();
    let second = make_pane_summary(ws.id, PaneRole::Agent);
    let second_id = second.id;
    let workspace = ws.id;
    ws.panes.push(second);
    let mut state = AppState::new();
    state.set_workspaces(vec![ws]);
    state.recap.select(Some(workspace));
    state.session_names.insert(first, "Reviewer".into());
    let page = ActivityPage {
        workspace_id: workspace,
        cursor: ActivityCursor {
            epoch: workspace,
            seq: 2,
        },
        gap: false,
        entries: [first, second_id]
            .into_iter()
            .enumerate()
            .map(|(index, pane_id)| ActivityEntry {
                seq: index as u64 + 1,
                at_ms: 0,
                kind: ActivityKind::Command {
                    pane_id,
                    role: PaneRole::Agent,
                    exit_code: 0,
                },
            })
            .collect(),
    };
    state.recap.journal(&page, false, false, &state.panes);
    let rows = state.recap_rows(state.active_workspace().unwrap());
    assert_eq!(rows.len(), 2);
    assert!(rows[0].label.starts_with("Agent 2 ·"));
    assert!(rows[1].label.starts_with("Reviewer ·"));
    for (row, id) in rows.iter().zip([second_id, first]) {
        assert!(state.focus_recap_row(row));
        assert_eq!(state.focused_pane_id(), Some(id));
    }
    state.remove_session(second_id);
    assert!(!state.focus_recap_row(&rows[0]));
    assert!(
        state.recap_rows(state.active_workspace().unwrap())[0]
            .pane
            .is_none()
    );
    assert_eq!(state.focused_pane_id(), Some(first));
}

#[test]
fn terminal_tab_names_count_named_siblings_and_keep_workspace_order() {
    let (mut ws, first, service, runner) = make_workspace_with_three_panes();
    let mut second = make_pane_summary(ws.id, PaneRole::Agent);
    second.status = PaneStatus::Exited { code: Some(5) };
    let second_id = second.id;
    ws.panes.push(second);
    let mut state = AppState::new();
    state.set_workspaces(vec![ws]);
    state.session_names.insert(first, "Reviewer".into());
    assert_eq!(
        state.terminal_tabs(state.active_workspace().unwrap()),
        vec![
            (first, "Reviewer".into()),
            (service, "Service".into()),
            (runner, "Runner".into()),
            (second_id, "Exited 5 · Agent 2".into())
        ]
    );
}

#[test]
fn refresh_recovers_focus_when_a_terminal_closed_in_another_client() {
    let (mut ws, agent, _, runner) = make_workspace_with_three_panes();
    let mut state = AppState::new();
    state.set_workspaces(vec![ws.clone()]);
    state.select_session(ws.id, PaneRole::Runner, runner);
    ws.panes.retain(|pane| pane.id != runner);
    state.set_workspaces(vec![ws.clone()]);
    assert_eq!(state.focused_pane_id(), Some(agent));
    // An explicit visit to an empty role stays there across an unrelated refresh.
    state.focused_role = PaneRole::Runner;
    state.set_workspaces(vec![ws]);
    assert_eq!(state.focused_role, PaneRole::Runner);
}

#[test]
fn removing_last_workspace_dismisses_its_settings() {
    let (ws, _, _, _) = make_workspace_with_three_panes();
    let mut state = AppState::new();
    state.set_workspaces(vec![ws]);
    state.settings.begin(Vec::new());
    state.set_workspaces(Vec::new());
    assert!(!state.settings.open);
    assert!(state.active_workspace_id.is_none());
}

#[test]
fn closing_middle_tab_selects_its_right_neighbor_across_roles() {
    let (mut ws, first, service, runner) = make_workspace_with_three_panes();
    let extra = make_pane_summary(ws.id, PaneRole::Service);
    let extra_id = extra.id;
    ws.panes.push(extra);
    let mut state = AppState::new();
    state.set_workspaces(vec![ws.clone()]);
    state.select_session(ws.id, PaneRole::Service, service);
    state.remove_session(first);
    assert_eq!(state.focused_pane_id(), Some(service));
    state.remove_session(service);
    assert_eq!(state.focused_pane_id(), Some(runner));
    state.remove_session(runner);
    assert_eq!(state.focused_pane_id(), Some(extra_id));
}

#[test]
fn closing_sessions_preserves_siblings_and_handles_the_last_pane() {
    let (workspace, agent, service, runner) = make_workspace_with_three_panes();
    let id = workspace.id;
    let mut state = AppState::new();
    state.set_workspaces(vec![workspace]);
    state.select_session(id, PaneRole::Runner, runner);
    state.toggle_search(runner);
    state.remove_session(runner);
    assert!(state.search.is_none());
    // The final tab now steps left to its visible neighbor.
    assert_eq!(state.focused_pane_id(), Some(service));
    state.remove_session(service);
    assert_eq!(state.focused_pane_id(), Some(agent));
    state.remove_session(agent);
    assert_eq!(state.focused_pane_id(), None);
    assert_eq!(state.active_workspace_id, Some(id));
}

#[test]
fn forwarding_checks_source_and_selected_agent_again_at_confirmation() {
    let (mut ws, first, _, runner) = make_workspace_with_three_panes();
    ws.panes
        .iter_mut()
        .find(|p| p.id == runner)
        .unwrap()
        .has_failure_trace = true;
    let mut state = AppState::new();
    state.set_workspaces(vec![ws.clone()]);
    let single = state.failure_recipient_choices().unwrap();
    assert_eq!(single.agents, [first]);
    let other = make_pane_summary(ws.id, PaneRole::Agent);
    ws.panes.push(other.clone());
    state.set_workspaces(vec![ws.clone()]);
    state.select_session(ws.id, PaneRole::Agent, first);
    let mut choices = state.failure_recipient_choices().unwrap();
    assert_eq!(state.validate_failure_recipient(&choices), Ok(first));
    choices.advance(1);
    assert_eq!(state.validate_failure_recipient(&choices), Ok(other.id));
    ws.panes.last_mut().unwrap().status = PaneStatus::Exited { code: Some(0) };
    state.set_workspaces(vec![ws.clone()]);
    assert!(state.validate_failure_recipient(&choices).is_err());
    choices.advance(-1);
    ws.panes.retain(|p| p.id != runner);
    state.set_workspaces(vec![ws]);
    assert!(state.validate_failure_recipient(&choices).is_err());
    state.active_workspace_id = None;
    assert!(state.validate_failure_recipient(&choices).is_err());
}

#[test]
fn workspace_refresh_keeps_live_grid_and_selected_pane_metadata() {
    let (mut ws, first, _, _) = make_workspace_with_three_panes();
    let extra = make_pane_summary(ws.id, PaneRole::Agent);
    let id = extra.id;
    ws.panes.push(extra);
    let mut state = AppState::new();
    state.set_workspaces(vec![ws.clone()]);
    state.select_session(ws.id, PaneRole::Agent, id);
    state.apply_grid_snapshot(GridSnapshot {
        pane_id: id,
        seq: 0,
        cols: 99,
        rows: 2,
        cursor: CursorState::default(),
        rows_data: vec![make_row(0, 0, "ready")],
        history_generation: 0,
        scrollback_len: 0,
    });
    state.set_workspaces(vec![ws.clone()]);
    assert_eq!(state.panes[&id].summary.cols, 99);

    state.apply_server_event(ServerEvent::GridDelta(GridDelta {
        pane_id: id,
        seq: 1,
        cols: 100,
        rows: 3,
        cursor: CursorState::default(),
        dirty_rows: vec![make_row(1, 1, "resized")],
        history_generation: 0,
        scrollback_len: None,
    }));
    state.set_workspaces(vec![ws]);
    state.apply_server_event(ServerEvent::PaneExited {
        pane_id: id,
        exit_code: Some(7),
    });
    let workspace = state.active_workspace().unwrap();
    let selected = state.pane_for_role(PaneRole::Agent).unwrap();
    let listed = state
        .workspace_panes(workspace)
        .find(|p| p.id == id)
        .unwrap();
    assert!(std::ptr::eq(listed, &selected.summary));
    assert_eq!((listed.cols, listed.rows), (100, 3));
    assert_eq!(row_text(&selected.viewport_rows[0]), "ready");
    assert_eq!(row_text(&selected.viewport_rows[1]), "resized");
    assert_eq!(
        state.terminal_tabs(workspace).last().unwrap().1,
        "Exited 7 · Agent 2"
    );
    state.focus_role_shortcut(PaneRole::Agent);
    assert_eq!(state.focused_pane_id(), Some(first));
}

#[test]
fn snapshot_and_delta_patching_tracks_dirty_rows() {
    let mut state = AppState::new();
    let (ws, agent_id, _, _) = make_workspace_with_three_panes();
    state.set_workspaces(vec![ws]);

    let snapshot = GridSnapshot {
        pane_id: agent_id,
        seq: 1,
        cols: 10,
        rows: 3,
        cursor: CursorState {
            col: 2,
            row: 1,
            visible: true,
        },
        rows_data: vec![
            make_row(0, 10, "line zero"),
            make_row(1, 11, "line one"),
            make_row(2, 12, "line two"),
        ],
        history_generation: 0,
        scrollback_len: 10,
    };

    let pane = state.panes.get_mut(&agent_id).unwrap();
    pane.apply_snapshot(snapshot);
    assert_eq!(pane.dirty_row_indices, vec![0, 1, 2]);
    assert_eq!(pane.scrollback_len, 10);
    assert_eq!(pane.take_dirty_rows(), vec![0, 1, 2]);
    assert!(pane.dirty_row_indices.is_empty());

    let delta = GridDelta {
        pane_id: agent_id,
        seq: 2,
        cols: 10,
        rows: 3,
        cursor: CursorState {
            col: 5,
            row: 1,
            visible: true,
        },
        history_generation: 0,
        scrollback_len: None,
        dirty_rows: vec![make_row(1, 11, "PATCHED!")],
    };
    assert_eq!(
        state.apply_server_event(ServerEvent::GridDelta(delta)),
        None
    );

    let pane = state.panes.get(&agent_id).unwrap();
    assert_eq!(pane.dirty_row_indices, vec![1]);
    assert_eq!(row_text(&pane.visible_rows()[0]), "line zero");
    assert_eq!(row_text(&pane.visible_rows()[1]), "PATCHED!");
    assert_eq!(row_text(&pane.visible_rows()[2]), "line two");
    assert_eq!(pane.cursor.col, 5);
}

#[test]
fn resync_required_signals_pane_id() {
    let mut state = AppState::new();
    let pane_id = PaneId::new();
    let resync = state.apply_server_event(ServerEvent::ResyncRequired { pane_id });
    assert_eq!(resync, Some(pane_id));
}

#[test]
fn history_reset_discards_displayed_rows_inflight_reads_and_search_results() {
    let mut app = AppState::new();
    let (ws, pane_id, _, _) = make_workspace_with_three_panes();
    app.set_workspaces(vec![ws]);
    app.toggle_search(pane_id);
    let search = app.search.as_mut().unwrap();
    search.push_text("hit");
    let stale_search = search.generation;
    search.set_results(
        12,
        vec![SearchMatch {
            abs_row: 1,
            start_col: 0,
            end_col: 3,
        }],
    );
    let pane = app.panes.get_mut(&pane_id).unwrap();
    pane.viewport_rows = vec![make_row(0, 12, "live")];
    pane.set_scroll_offset(4);
    let initial = pane.begin_scrollback_request().unwrap();
    pane.apply_scrollback(initial, 12, vec![make_row(0, 8, "old")]);
    pane.set_scroll_offset(5);
    let late = pane.begin_scrollback_request().unwrap();
    let reset = GridDelta {
        pane_id,
        seq: 10,
        cols: pane.summary.cols,
        rows: pane.summary.rows,
        cursor: pane.cursor,
        dirty_rows: vec![],
        history_generation: 1,
        scrollback_len: Some(0),
    };
    app.apply_server_event(ServerEvent::GridDelta(reset));
    let pane = app.panes.get_mut(&pane_id).unwrap();
    pane.apply_scrollback(late, 12, vec![make_row(0, 7, "wrong")]);
    assert_eq!(pane.requested_scroll_offset, 0);
    assert_eq!(pane.scroll_offset, 0);
    assert_eq!(pane.scrollback_len, 0);
    assert_eq!(row_text(&pane.visible_rows()[0]), "live");
    assert!(!app.apply_search_results(pane_id, stale_search, 12, vec![]));
    assert!(app.search.as_ref().unwrap().matches.is_empty());
}

#[test]
fn resync_snapshot_invalidates_erased_history_even_if_new_lines_already_accumulated() {
    let mut app = AppState::new();
    let (ws, id, _, _) = make_workspace_with_three_panes();
    app.set_workspaces(vec![ws]);
    app.toggle_search(id);
    let search = app.search.as_mut().unwrap();
    search.push_text("old");
    let generation = search.generation;
    let pane = app.panes.get_mut(&id).unwrap();
    pane.set_scroll_offset(2);
    let pending = pane.begin_scrollback_request().unwrap();
    let reset = GridSnapshot {
        pane_id: id,
        seq: 4,
        cols: pane.summary.cols,
        rows: pane.summary.rows,
        cursor: pane.cursor,
        rows_data: vec![make_row(0, 50, "live")],
        scrollback_len: 5,
        history_generation: 1,
    };
    let mut stale = reset.clone();
    stale.history_generation = 0;
    stale.scrollback_len = 20;
    assert_eq!(app.apply_grid_snapshot(reset), None);
    assert_eq!(app.apply_grid_snapshot(stale), Some(id));
    assert!(!app.apply_search_results(id, generation, 20, vec![]));
    let pane = app.panes.get_mut(&id).unwrap();
    pane.apply_scrollback(pending, 20, vec![make_row(0, 1, "old")]);
    assert_eq!(pane.scrollback_len, 5);
    assert_eq!(pane.requested_scroll_offset, 0);
    assert_eq!(row_text(&pane.visible_rows()[0]), "live");
}

#[test]
fn failed_runner_tracks_each_workspaces_last_completed_runner_results() {
    let (mut ws, agent, _, first) = make_workspace_with_three_panes();
    let second = make_pane_summary(ws.id, PaneRole::Runner);
    let second_id = second.id;
    ws.panes.push(second);
    let (mut other, _, _, other_runner) = make_workspace_with_three_panes();
    other.panes.last_mut().unwrap().status = PaneStatus::Exited { code: Some(4) };
    let mut state = AppState::new();
    state.set_workspaces(vec![ws, other]);
    assert_eq!(
        state.failed_runner(&state.workspaces[1]),
        Some(other_runner)
    );
    for id in [agent, first, second_id] {
        state.apply_server_event(ServerEvent::CommandFinished {
            pane_id: id,
            command: None,
            exit_code: 1,
            has_failure_trace: true,
        });
    }
    assert_eq!(state.failed_runner(&state.workspaces[0]), Some(second_id));
    // Reconnected daemon summaries may retain a trace from before a successful run.
    state
        .panes
        .get_mut(&second_id)
        .unwrap()
        .summary
        .last_exit_code = Some(0);
    assert_eq!(state.failed_runner(&state.workspaces[0]), Some(first));
    state.remove_session(first);
    assert_eq!(state.failed_runner(&state.workspaces[0]), None);
}

#[test]
fn command_finished_updates_runner_failure_badge_and_workspace_summary() {
    let mut state = AppState::new();
    let (ws, _, _, runner_id) = make_workspace_with_three_panes();
    state.set_workspaces(vec![ws]);
    assert!(!state.runner_has_failure());

    state.apply_server_event(ServerEvent::CommandFinished {
        pane_id: runner_id,
        command: Some("cargo test".to_string()),
        exit_code: 101,
        has_failure_trace: true,
    });

    assert!(state.runner_has_failure());
    let runner = state.panes.get(&runner_id).unwrap();
    assert_eq!(runner.summary.last_exit_code, Some(101));
    assert!(runner.summary.has_failure_trace);

    let ws_runner = state
        .workspace_panes(state.active_workspace().unwrap())
        .find(|p| p.id == runner_id)
        .unwrap();
    assert_eq!(ws_runner.last_exit_code, Some(101));
    assert!(ws_runner.has_failure_trace);

    // Subsequent passing command clears failure badge
    state.apply_server_event(ServerEvent::CommandFinished {
        pane_id: runner_id,
        command: Some("cargo test".to_string()),
        exit_code: 0,
        has_failure_trace: false,
    });
    assert!(!state.runner_has_failure());
    assert_eq!(
        state.panes.get(&runner_id).unwrap().summary.last_exit_code,
        Some(0)
    );
}

#[test]
fn stacked_pane_focus_follows_vertical_order() {
    let mut state = AppState::new();
    assert_eq!(
        state.focus_direction(FocusDirection::Down, true),
        PaneRole::Service
    );
    assert_eq!(
        state.focus_direction(FocusDirection::Down, true),
        PaneRole::Runner
    );
    assert_eq!(
        state.focus_direction(FocusDirection::Up, true),
        PaneRole::Service
    );
    assert_eq!(
        state.focus_direction(FocusDirection::Up, true),
        PaneRole::Agent
    );
}

#[test]
fn directional_focus_navigates_three_pane_layout() {
    let mut state = AppState::new();
    assert_eq!(state.focused_role, PaneRole::Agent);

    assert_eq!(
        state.focus_direction(FocusDirection::Right, false),
        PaneRole::Service
    );
    assert_eq!(
        state.focus_direction(FocusDirection::Down, false),
        PaneRole::Runner
    );
    assert_eq!(
        state.focus_direction(FocusDirection::Up, false),
        PaneRole::Service
    );
    assert_eq!(
        state.focus_direction(FocusDirection::Left, false),
        PaneRole::Agent
    );
    // Moving left from Agent is a no-op
    assert_eq!(
        state.focus_direction(FocusDirection::Left, false),
        PaneRole::Agent
    );
}

#[test]
fn workspace_runtime_updated_replaces_bound_ports_and_containers() {
    use daedalus_proto::{ContainerHealth, PortMapping, PortSource};

    let mut state = AppState::new();
    let (ws, _, _, _) = make_workspace_with_three_panes();
    let ws_id = ws.id;
    state.set_workspaces(vec![ws]);

    let ports = vec![
        PortMapping {
            host_port: 3100,
            container_port: Some(3000),
            service: Some("web".to_string()),
            source: PortSource::Docker,
        },
        PortMapping {
            host_port: 5532,
            container_port: None,
            service: None,
            source: PortSource::LocalProcess,
        },
    ];
    let containers = vec![ContainerHealth {
        name: "demo-wt1-web-1".to_string(),
        service: Some("web".to_string()),
        state: "running".to_string(),
        health: Some("healthy".to_string()),
        ports: vec![3100],
    }];

    assert_eq!(
        state.apply_server_event(ServerEvent::WorkspaceRuntimeUpdated {
            workspace_id: ws_id,
            bound_ports: ports.clone(),
            containers: containers.clone(),
            git_status: None,
            git_branch: None,
        }),
        None
    );

    let updated = state.active_workspace().unwrap();
    assert_eq!(updated.bound_ports, ports);
    assert_eq!(updated.containers, containers);

    // Unknown workspace ID is safely ignored
    assert_eq!(
        state.apply_server_event(ServerEvent::WorkspaceRuntimeUpdated {
            workspace_id: WorkspaceId::new(),
            bound_ports: Vec::new(),
            containers: Vec::new(),
            git_status: None,
            git_branch: None,
        }),
        None
    );
    assert_eq!(state.active_workspace().unwrap().bound_ports.len(), 2);
}

#[test]
fn external_status_updated_upserts_by_source_and_caps_at_limit() {
    use daedalus_proto::StatusLevel;

    let mut state = AppState::new();
    let (ws, _, _, _) = make_workspace_with_three_panes();
    let ws_id = ws.id;
    state.set_workspaces(vec![ws]);

    let mk_status = |source: &str, level: StatusLevel, label: &str| ExternalStatus {
        source: source.to_string(),
        level,
        label: label.to_string(),
        detail: None,
        updated_at_ms: 1,
    };

    state.apply_server_event(ServerEvent::ExternalStatusUpdated {
        workspace_id: ws_id,
        status: mk_status("phrourion", StatusLevel::Warn, "3 dirty"),
    });
    state.apply_server_event(ServerEvent::ExternalStatusUpdated {
        workspace_id: ws_id,
        status: mk_status("cassandra", StatusLevel::Error, "retry"),
    });
    assert_eq!(state.active_workspace().unwrap().external_statuses.len(), 2);

    // Upsert existing source updates it without duplicating
    state.apply_server_event(ServerEvent::ExternalStatusUpdated {
        workspace_id: ws_id,
        status: mk_status("phrourion", StatusLevel::Ok, "clean"),
    });
    let statuses = &state.active_workspace().unwrap().external_statuses;
    assert_eq!(statuses.len(), 2);
    assert_eq!(statuses[0].source, "cassandra");
    assert_eq!(statuses[1].source, "phrourion");
    assert_eq!(statuses[1].label, "clean");
    assert_eq!(statuses[1].level, StatusLevel::Ok);

    // Exceeding MAX_EXTERNAL_STATUSES evicts oldest
    for i in 0..MAX_EXTERNAL_STATUSES {
        state.apply_server_event(ServerEvent::ExternalStatusUpdated {
            workspace_id: ws_id,
            status: mk_status(&format!("src-{i}"), StatusLevel::Info, "ok"),
        });
    }
    assert_eq!(
        state.active_workspace().unwrap().external_statuses.len(),
        MAX_EXTERNAL_STATUSES
    );
}

#[test]
fn external_status_updated_clears_matching_source_on_empty_label() {
    use daedalus_proto::StatusLevel;

    let mut state = AppState::new();
    let (ws, _, _, _) = make_workspace_with_three_panes();
    let ws_id = ws.id;
    state.set_workspaces(vec![ws]);

    let mk_status = |source: &str, label: &str| ExternalStatus {
        source: source.to_string(),
        level: StatusLevel::Info,
        label: label.to_string(),
        detail: None,
        updated_at_ms: 1,
    };

    state.apply_server_event(ServerEvent::ExternalStatusUpdated {
        workspace_id: ws_id,
        status: mk_status("phrourion", "3 dirty"),
    });
    state.apply_server_event(ServerEvent::ExternalStatusUpdated {
        workspace_id: ws_id,
        status: mk_status("cassandra", "ok"),
    });
    assert_eq!(state.active_workspace().unwrap().external_statuses.len(), 2);

    // Empty-label clear signal removes matching source without leaving a blank badge
    state.apply_server_event(ServerEvent::ExternalStatusUpdated {
        workspace_id: ws_id,
        status: mk_status("phrourion", ""),
    });
    let statuses = &state.active_workspace().unwrap().external_statuses;
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].source, "cassandra");

    // Clearing an unknown source with whitespace label is a no-op
    state.apply_server_event(ServerEvent::ExternalStatusUpdated {
        workspace_id: ws_id,
        status: mk_status("unknown", "   "),
    });
    assert_eq!(state.active_workspace().unwrap().external_statuses.len(), 1);
}

#[test]
fn worktree_modal_and_picker_are_mutually_exclusive() {
    let mut state = AppState::new();
    assert!(!state.picker.open);
    assert!(!state.worktree_modal.open);

    state.open_picker_with(AgentPreset::Claude);
    assert!(state.picker.open);
    assert!(!state.worktree_modal.open);

    // Opening worktree modal closes picker and resets branch input
    state.open_worktree_modal();
    assert!(!state.picker.open);
    assert!(state.worktree_modal.open);
    state.worktree_modal.push_char('f');
    state.worktree_modal.push_char('e');
    state.worktree_modal.pop_char();
    assert_eq!(state.worktree_modal.branch_input, "f");

    // Opening picker closes worktree modal and clears input
    state.open_picker_with(AgentPreset::Agy);
    assert!(state.picker.open);
    assert!(!state.worktree_modal.open);
    assert!(state.worktree_modal.branch_input.is_empty());

    state.toggle_worktree_modal();
    assert!(!state.picker.open);
    assert!(state.worktree_modal.open);
    state.toggle_worktree_modal();
    assert!(!state.worktree_modal.open);
}

#[test]
fn dormant_workspace_helpers_prevent_double_spawning() {
    let mut state = AppState::new();
    let ws_id = WorkspaceId::new();
    let dormant = WorkspaceInfo {
        project: Default::default(),
        setup: None,
        default_panes_pending: None,
        id: ws_id,
        name: "dormant".to_string(),
        root_dir: PathBuf::from("/tmp/dormant"),
        slot_index: 0,
        injected_env: HashMap::new(),
        panes: Vec::new(),
        port_offset: 0,
        compose_project_name: None,
        git_branch: None,
        git_status: None,
        bound_ports: Vec::new(),
        containers: Vec::new(),
        external_statuses: Vec::new(),
        manifest: None,
    };
    state.set_workspaces(vec![dormant]);

    // Dormant active workspace is detected before spawning begins
    assert_eq!(state.active_dormant_workspace().map(|w| w.id), Some(ws_id));
    assert!(state.mark_workspace_spawning(ws_id));
    // Second mark returns false and active_dormant_workspace suppresses double-spawn
    assert!(!state.mark_workspace_spawning(ws_id));
    assert!(state.active_dormant_workspace().is_none());

    // Clearing spawning flag without panes still sees it as dormant again
    state.finish_workspace_spawning(ws_id, false);
    assert_eq!(state.active_dormant_workspace().map(|w| w.id), Some(ws_id));

    // Legacy daemons infer completion from the first pane.
    assert!(state.mark_workspace_spawning(ws_id));
    state.register_pane(make_pane_summary(ws_id, PaneRole::Agent), None);
    state.finish_workspace_spawning(ws_id, false);
    assert!(state.active_dormant_workspace().is_none());

    // A newer daemon can explicitly report a partially provisioned workspace.
    state.active_workspace_mut().unwrap().default_panes_pending = Some(true);
    assert_eq!(state.active_dormant_workspace().map(|w| w.id), Some(ws_id));
    state.mark_workspace_spawning(ws_id);
    state.finish_workspace_spawning(ws_id, false);
    assert!(state.active_dormant_workspace().is_some());
    state.finish_workspace_spawning(ws_id, true);
    state.active_workspace_mut().unwrap().panes.clear();
    assert!(state.active_dormant_workspace().is_none());
}

#[test]
fn stale_search_results_are_ignored_when_generation_or_pane_mismatches() {
    let mut state = AppState::new();
    let (ws, agent_id, service_id, _) = make_workspace_with_three_panes();
    state.set_workspaces(vec![ws]);
    state.toggle_search(agent_id);

    let search = state.search.as_mut().unwrap();
    let gen_init = search.generation;
    search.push_text("err");
    let gen_err = search.generation;
    assert!(gen_err > gen_init);

    // User keeps typing before the response for "err" comes back
    search.push_text("or");
    let gen_error = search.generation;
    assert!(gen_error > gen_err);

    let stale_match = vec![SearchMatch {
        abs_row: 3,
        start_col: 0,
        end_col: 3,
    }];
    let fresh_match = vec![SearchMatch {
        abs_row: 8,
        start_col: 1,
        end_col: 6,
    }];

    // Stale generation from "err" is dropped
    assert!(!state.apply_search_results(agent_id, gen_err, 20, stale_match.clone()));
    assert!(state.search.as_ref().unwrap().matches.is_empty());

    // Results for a different pane are dropped
    assert!(!state.apply_search_results(service_id, gen_error, 20, fresh_match.clone()));
    assert!(state.search.as_ref().unwrap().matches.is_empty());

    // Matching pane_id + current generation applies cleanly
    assert!(state.apply_search_results(agent_id, gen_error, 20, fresh_match.clone()));
    assert_eq!(state.search.as_ref().unwrap().matches, fresh_match);

    // If user backspaces back to empty query, any in-flight response for earlier gen is dropped
    for _ in 0..5 {
        state.search.as_mut().unwrap().pop_char();
    }
    assert!(state.search.as_ref().unwrap().query.is_empty());
    assert!(!state.apply_search_results(agent_id, gen_error, 20, fresh_match));
    assert!(state.search.as_ref().unwrap().matches.is_empty());
}

#[test]
fn backspace_removes_one_grapheme_in_search_and_branch_fields() {
    let mut search = SearchState::new(PaneId::new());
    let mut branch = WorktreeModalState::default();
    // Some input methods deliver a base and its combining mark separately.
    for committed in ["e", "\u{301}", "🇳🇱", "👩🏽‍💻"] {
        search.push_text(committed);
        branch.push_text(committed);
    }
    for expected in ["e\u{301}🇳🇱", "e\u{301}", ""] {
        let generation = search.generation;
        search.set_results(0, Vec::new());
        assert!(search.pop_char());
        branch.pop_char();
        assert_eq!(search.query, expected);
        assert_eq!(branch.branch_input, expected);
        assert_ne!(search.generation, generation);
        assert!(!search.apply_results(generation, 0, Vec::new()));
    }
    let empty_generation = search.generation;
    assert!(!search.pop_char());
    branch.pop_char();
    assert!(branch.branch_input.is_empty());
    assert_eq!(search.generation, empty_generation);
}

#[test]
fn closing_and_reopening_search_rejects_earlier_session_results() {
    let mut state = AppState::new();
    let (ws, agent_id, _, _) = make_workspace_with_three_panes();
    state.set_workspaces(vec![ws]);

    // Session 1: open search, type "a", then close
    state.toggle_search(agent_id);
    state.search.as_mut().unwrap().push_text("a");
    let gen_session1 = state.search.as_ref().unwrap().generation;
    state.close_search();

    // Session 2: reopen search on same pane and type "z"
    state.toggle_search(agent_id);
    state.search.as_mut().unwrap().push_text("z");
    let gen_session2 = state.search.as_ref().unwrap().generation;
    assert_ne!(gen_session1, gen_session2);

    let old_match = vec![SearchMatch {
        abs_row: 1,
        start_col: 0,
        end_col: 1,
    }];
    let new_match = vec![SearchMatch {
        abs_row: 4,
        start_col: 2,
        end_col: 3,
    }];

    // Late response from session 1 is rejected even though query length matches
    assert!(!state.apply_search_results(agent_id, gen_session1, 10, old_match));
    assert!(state.search.as_ref().unwrap().matches.is_empty());

    // Current session 2 response applies cleanly
    assert!(state.apply_search_results(agent_id, gen_session2, 10, new_match.clone()));
    assert_eq!(state.search.as_ref().unwrap().matches, new_match);
}

#[test]
fn pending_close_retains_options_until_workspace_disappears() {
    let mut state = AppState::new();
    let (mut workspace, _, _, _) = make_workspace_with_three_panes();
    workspace.compose_project_name = Some("test".into());
    let id = workspace.id;
    state.set_workspaces(vec![workspace.clone()]);
    assert!(state.open_close_modal_for_workspace(id));
    state.close_modal.toggle_compose();
    state.close_modal.submitting = true;
    state.close_modal.toggle_compose();
    state.set_workspaces(vec![workspace]);
    assert!(state.close_modal.open);
    assert!(!state.close_modal.compose_down);
    state.set_workspaces(Vec::new());
    assert!(!state.close_modal.open);
    assert!(!state.close_modal.submitting);
}

#[test]
fn close_workspace_modal_defaults_toggles_and_mutual_exclusion() {
    let mut state = AppState::new();
    let (mut primary_ws, agent_id, _, _) = make_workspace_with_three_panes();
    primary_ws.name = "primary".to_string();
    primary_ws.git_branch = Some("master".to_string());
    primary_ws.git_status = Some(GitWorkspaceStatus {
        is_worktree: false,
        dirty: false,
        ahead: 0,
        behind: 0,
    });

    let (mut wt_ws, _, _, _) = make_workspace_with_three_panes();
    wt_ws.name = "feat-wt".to_string();
    wt_ws.compose_project_name = Some("daedalus-wt1".to_string());
    wt_ws.git_branch = Some("feat/wt".to_string());
    wt_ws.git_status = Some(GitWorkspaceStatus {
        is_worktree: true,
        dirty: true,
        ahead: 2,
        behind: 1,
    });
    let primary_id = primary_ws.id;
    let wt_id = wt_ws.id;

    state.set_workspaces(vec![primary_ws, wt_ws]);
    state.open_picker_with(AgentPreset::Claude);
    state.toggle_search(agent_id);

    // Opening close modal for worktree workspace closes search/modals and sets defaults
    assert!(state.open_close_modal_for_workspace(wt_id));
    assert!(!state.picker.open);
    assert!(state.search.is_none());
    assert!(state.close_modal.open);
    assert_eq!(state.close_modal.workspace_id, Some(wt_id));
    assert_eq!(state.close_modal.workspace_name, "feat-wt");
    assert_eq!(state.close_modal.branch.as_deref(), Some("feat/wt"));
    assert!(state.close_modal.is_worktree);
    assert!(state.close_modal.is_dirty);
    assert!(state.close_modal.has_compose);
    assert!(state.close_modal.compose_down);
    assert!(!state.close_modal.remove_worktree);
    assert!(!state.close_modal.force_remove_worktree);

    // Force cannot be toggled before remove_worktree is enabled
    state.close_modal.toggle_force();
    assert!(!state.close_modal.force_remove_worktree);

    // Enable remove_worktree, then enable force
    state.close_modal.toggle_worktree();
    assert!(state.close_modal.remove_worktree);
    state.close_modal.toggle_force();
    assert!(state.close_modal.force_remove_worktree);

    // Disabling remove_worktree also clears force_remove_worktree
    state.close_modal.toggle_worktree();
    assert!(!state.close_modal.remove_worktree);
    assert!(!state.close_modal.force_remove_worktree);

    // Toggle compose off/on
    state.close_modal.toggle_compose();
    assert!(!state.close_modal.compose_down);
    state.close_modal.toggle_compose();
    assert!(state.close_modal.compose_down);

    // Clean worktree can toggle remove_worktree, but cannot enable force_remove_worktree
    let (mut clean_wt, _, _, _) = make_workspace_with_three_panes();
    clean_wt.name = "clean-wt".to_string();
    clean_wt.git_status = Some(GitWorkspaceStatus {
        is_worktree: true,
        dirty: false,
        ahead: 0,
        behind: 0,
    });
    let clean_wt_id = clean_wt.id;
    state.upsert_workspace(clean_wt);
    assert!(state.open_close_modal_for_workspace(clean_wt_id));
    assert!(state.close_modal.is_worktree);
    assert!(!state.close_modal.is_dirty);
    state.close_modal.toggle_worktree();
    assert!(state.close_modal.remove_worktree);
    state.close_modal.toggle_force();
    assert!(!state.close_modal.force_remove_worktree);

    // Primary checkout cannot enable remove_worktree or force
    assert!(state.open_close_modal_for_workspace(primary_id));
    assert!(!state.close_modal.is_worktree);
    assert!(!state.close_modal.has_compose);
    assert!(!state.close_modal.compose_down);
    state.close_modal.toggle_worktree();
    assert!(!state.close_modal.remove_worktree);
    state.close_modal.toggle_force();
    assert!(!state.close_modal.force_remove_worktree);
}

#[test]
fn agent_state_and_workspace_runtime_updates_sync_state() {
    let mut state = AppState::new();
    let (ws, agent_id, _, _) = make_workspace_with_three_panes();
    let ws_id = ws.id;
    state.set_workspaces(vec![ws]);

    assert_eq!(
        state.workspace_agent_state(state.active_workspace().unwrap()),
        None
    );

    state.apply_server_event(ServerEvent::AgentStateChanged {
        workspace_id: ws_id,
        pane_id: agent_id,
        state: AgentState::WaitingForApproval,
    });

    assert_eq!(
        state.panes.get(&agent_id).unwrap().summary.agent_state,
        Some(AgentState::WaitingForApproval)
    );
    assert_eq!(
        state.workspace_agent_state(state.active_workspace().unwrap()),
        Some(AgentState::WaitingForApproval)
    );

    let gs = GitWorkspaceStatus {
        is_worktree: true,
        dirty: true,
        ahead: 3,
        behind: 1,
    };
    state.apply_server_event(ServerEvent::WorkspaceRuntimeUpdated {
        workspace_id: ws_id,
        bound_ports: Vec::new(),
        containers: Vec::new(),
        git_status: Some(gs),
        git_branch: Some("feat/v0.0.3".to_string()),
    });
    let updated = state.active_workspace().unwrap();
    assert_eq!(updated.git_status, Some(gs));
    assert_eq!(updated.git_branch.as_deref(), Some("feat/v0.0.3"));
}

#[test]
fn repeated_role_shortcut_selects_a_specific_session_and_restores_it() {
    let mut state = AppState::new();
    let (mut first, original, _, _) = make_workspace_with_three_panes();
    let second_agent = make_pane_summary(first.id, PaneRole::Agent);
    let newest = second_agent.id;
    first.panes.push(second_agent);
    let (other, _, _, _) = make_workspace_with_three_panes();
    state.set_workspaces(vec![first, other]);
    assert_eq!(state.focused_pane_id(), Some(newest));
    state.focus_role_shortcut(PaneRole::Agent);
    assert_eq!(state.focused_pane_id(), Some(original));
    state.cycle_workspace(1);
    state.focus_role_shortcut(PaneRole::Runner);
    state.cycle_workspace(-1);
    assert_eq!(state.focused_pane_id(), Some(original));
    state.focus_role_shortcut(PaneRole::Agent);
    assert_eq!(state.focused_pane_id(), Some(newest));
    state.cycle_workspace(1);
    assert_eq!(state.focused_role, PaneRole::Runner);
}

#[test]
fn clicking_a_different_pane_releases_keyboard_input_from_search() {
    let (workspace, agent, _, _) = make_workspace_with_three_panes();
    let mut state = AppState::new();
    state.set_workspaces(vec![workspace]);
    state.toggle_search(agent);
    state.focus_clicked_pane(PaneRole::Agent);
    assert_eq!(state.search.as_ref().unwrap().pane_id, agent);
    state.focus_clicked_pane(PaneRole::Service);
    assert_eq!(state.focused_role, PaneRole::Service);
    assert!(state.search.is_none());
}

#[test]
fn next_failed_workspace_visits_current_then_wraps_in_sidebar_order() {
    let (mut primary, _, _, first) = make_workspace_with_three_panes();
    let (mut unrelated, _, _, second) = make_workspace_with_three_panes();
    let (mut linked, _, _, third) = make_workspace_with_three_panes();
    primary.project.repository = Some("/one".into());
    linked.project.repository = primary.project.repository.clone();
    unrelated.project.repository = Some("/two".into());
    let expected = [
        (primary.id, first),
        (linked.id, third),
        (unrelated.id, second),
    ];
    let mut state = AppState::new();
    state.set_workspaces(vec![primary, unrelated, linked]);
    assert_eq!(state.next_failed_workspace(), None);
    for (_, id) in expected {
        state.panes.get_mut(&id).unwrap().summary.last_exit_code = Some(1);
    }
    for (ws, pane) in expected {
        assert_eq!(state.next_failed_workspace(), Some(ws));
        state.select_workspace(ws);
        state.select_session(ws, PaneRole::Runner, pane);
    }
    assert_eq!(state.next_failed_workspace(), Some(expected[0].0));
}

#[test]
fn workspace_shortcuts_follow_repository_groups_in_sidebar_order() {
    let (mut primary, _, _, _) = make_workspace_with_three_panes();
    let (mut unrelated, _, _, _) = make_workspace_with_three_panes();
    let (mut linked, _, _, _) = make_workspace_with_three_panes();
    primary.project.repository = Some("/one".into());
    linked.project.repository = primary.project.repository.clone();
    unrelated.project.repository = Some("/two".into());
    let expected = [primary.id, linked.id, unrelated.id];
    let mut state = AppState::new();
    state.set_workspaces(vec![primary, unrelated, linked]);
    assert_eq!(state.cycle_workspace(1), Some(expected[1]));
    assert_eq!(state.cycle_workspace(1), Some(expected[2]));
    assert_eq!(state.cycle_workspace(1), Some(expected[0]));
    assert_eq!(state.cycle_workspace(-1), Some(expected[2]));
}

#[test]
fn cycle_workspace_wraps_cleanly_forward_and_backward() {
    let mut state = AppState::new();
    assert_eq!(state.cycle_workspace(1), None);
    assert_eq!(state.cycle_workspace(-1), None);

    let (ws1, _, _, _) = make_workspace_with_three_panes();
    let (ws2, _, _, _) = make_workspace_with_three_panes();
    let (ws3, _, _, _) = make_workspace_with_three_panes();
    let id1 = ws1.id;
    let id2 = ws2.id;
    let id3 = ws3.id;
    state.set_workspaces(vec![ws1, ws2, ws3]);
    assert_eq!(state.active_workspace_id, Some(id1));

    assert_eq!(state.cycle_workspace(1), Some(id2));
    assert_eq!(state.cycle_workspace(1), Some(id3));
    assert_eq!(state.cycle_workspace(1), Some(id1));
    assert_eq!(state.cycle_workspace(-1), Some(id3));
    assert_eq!(state.cycle_workspace(-1), Some(id2));
    assert_eq!(state.cycle_workspace(-1), Some(id1));
}
