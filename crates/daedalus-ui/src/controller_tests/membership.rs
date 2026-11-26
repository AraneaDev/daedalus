use super::*;
use daedalus_proto::{Cell, GridRow, GridSnapshot};

#[test]
fn terminal_readiness_starts_the_hydrated_dormant_workspace() {
    let mut w = Workbench::new();
    let mut dormant = workspace("Dormant");
    let id = dormant.id;
    dormant.panes.clear();
    dormant.default_panes_pending = Some(true);

    w.app.handle_daemon_event(UiEvent::Disconnected);
    w.app
        .handle_daemon_event(UiEvent::WorkspacesLoaded(vec![dormant]));
    assert!(w.idle());

    w.app.handle_daemon_event(UiEvent::Connected);
    assert!(matches!(
        w.command(),
        UiCommand::EnsureWorkspacePanes { workspace_id, .. } if workspace_id == id
    ));
    assert!(w.idle());
}

#[test]
fn membership_reconciles_new_panes_without_stealing_focus_or_resetting_other_grids() {
    let mut w = Workbench::new();
    let mut active = workspace("Current");
    let other = workspace("Background");
    let original = active.panes[0].id;
    let cached = other.panes[2].id;
    let active_id = active.id;
    w.app.handle_daemon_event(UiEvent::WorkspacesLoaded(vec![
        active.clone(),
        other.clone(),
    ]));
    assert_eq!(w.app.state.focused_pane_id(), Some(original));
    w.app.state.toggle_search(original);
    w.app.handle_daemon_event(UiEvent::Snapshot(GridSnapshot {
        pane_id: cached,
        seq: 7,
        cols: 91,
        rows: 25,
        cursor: Default::default(),
        rows_data: vec![GridRow {
            row_index: 0,
            abs_row: 0,
            cells: vec![Cell {
                text: "cached".into(),
                ..Default::default()
            }],
            wraps: false,
            links: Vec::new(),
        }],
        history_generation: 0,
        scrollback_len: 0,
        terminal_modes: Default::default(),
        activity: None,
        insight: None,
    }));
    while w.commands.try_recv().is_ok() {}

    let mut added = active.panes[0].clone();
    added.id = PaneId::new();
    let added_id = added.id;
    active.panes.push(added);
    w.app
        .handle_daemon_event(UiEvent::WorkspaceMembershipLoaded(vec![
            active.clone(),
            other.clone(),
        ]));
    assert_eq!(w.app.state.focused_pane_id(), Some(original));
    assert_eq!(w.app.state.search.as_ref().unwrap().pane_id, original);
    assert!(matches!(
        w.command(),
        UiCommand::RequestSnapshot { pane_id } if pane_id == added_id
    ));
    assert!(w.idle());

    active.panes.retain(|pane| pane.id != original);
    let mut replacement = active.panes.last().unwrap().clone();
    replacement.id = PaneId::new();
    let replacement_id = replacement.id;
    active.panes.push(replacement);
    w.app
        .handle_daemon_event(UiEvent::WorkspaceMembershipLoaded(vec![active, other]));
    assert_eq!(w.app.state.active_workspace_id, Some(active_id));
    assert_eq!(w.app.state.focused_pane_id(), Some(replacement_id));
    assert!(!w.app.state.panes.contains_key(&original));
    assert!(w.app.state.search.is_none());
    let background = &w.app.state.panes[&cached];
    assert_eq!((background.summary.cols, background.summary.rows), (91, 25));
    assert_eq!(background.viewport_rows[0].cells[0].text, "cached");
    assert!(matches!(
        w.command(),
        UiCommand::RequestSnapshot { pane_id } if pane_id == replacement_id
    ));
    assert!(w.idle());
}

#[test]
fn membership_focuses_the_first_pane_in_a_previously_empty_workspace() {
    let mut w = Workbench::new();
    let mut empty = workspace("Waiting");
    let runner = empty.panes.pop().unwrap();
    let id = runner.id;
    empty.default_panes_pending = Some(false);
    empty.panes.clear();
    w.app
        .handle_daemon_event(UiEvent::WorkspacesLoaded(vec![empty.clone()]));
    assert_eq!(w.app.state.focused_pane_id(), None);
    assert!(w.idle());

    empty.panes.push(runner);
    w.app
        .handle_daemon_event(UiEvent::WorkspaceMembershipLoaded(vec![empty]));
    assert_eq!(w.app.state.focused_pane_id(), Some(id));
    assert!(matches!(
        w.command(),
        UiCommand::RequestSnapshot { pane_id } if pane_id == id
    ));
    assert!(w.idle());
}
