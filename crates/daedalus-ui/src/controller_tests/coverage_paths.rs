use super::*;
use daedalus_ui::layout::{Rect, overlays, sidebar_workspace_close_rect};
use daedalus_ui::sidebar_content::SidebarRow;

fn metrics() -> ViewportMetrics {
    ViewportMetrics {
        size: (1200, 500).into(),
        scale_factor: 1.,
        cell: (8., 16.),
    }
}

fn click(app: &mut DaedalusApp, at: Rect) {
    app.move_pointer(at.x + at.width / 2., at.y + at.height / 2., Some(metrics()));
    app.click_at(metrics());
    app.handle_left_mouse_up();
}

#[test]
fn sidebar_paging_and_card_controls_target_the_visible_workspace() {
    let mut workbench = Workbench::new();
    let app = &mut workbench.app;
    app.state.set_workspaces(
        (0..9)
            .map(|index| workspace(&format!("Workspace {index}")))
            .collect(),
    );
    app.state.sidebar_override = Some(true);
    let before = app.layout_for(metrics()).sidebar_content.unwrap();
    let (up, down) = before.arrows.unwrap();
    click(app, down);
    assert!(app.state.sidebar_first > before.visible.start);
    click(app, up);
    assert_eq!(app.state.sidebar_first, before.visible.start);
    let layout = app.layout_for(metrics()).sidebar_content.unwrap();
    let (index, card) = layout.rows[layout.visible.clone()]
        .iter()
        .zip(&layout.rects)
        .filter_map(|(row, rect)| match row {
            SidebarRow::Workspace(index) => Some((*index, *rect)),
            _ => None,
        })
        .nth(1)
        .unwrap();
    let target = app.state.workspaces[index].id;
    click(app, card);
    assert_eq!(app.state.active_workspace_id, Some(target));
    click(app, sidebar_workspace_close_rect(card, 1.));
    assert_eq!(app.state.close_modal.workspace_id, Some(target));
    app.state.close_modal.close();
    click(app, layout.new_worktree);
    assert!(app.state.worktree_modal.open);
}

#[test]
fn pointer_routes_search_and_preview_controls_to_the_active_pane() {
    let mut workbench = Workbench::new();
    let ws = workspace("Search and preview");
    let agent = ws.panes[0].id;
    workbench.app.state.set_workspaces(vec![ws]);
    workbench.app.state.sidebar_override = Some(true);
    workbench.app.handle_ui_action(UiAction::ToggleSearch);
    let app = &mut workbench.app;
    let layout = app.layout_for(metrics());
    let pane = layout.pane(PaneRole::Agent).unwrap();
    let [_, _, match_case] = overlays::search_controls(overlays::search_bar_rect(pane, 1.));
    click(app, match_case);
    assert!(app.state.search.as_ref().unwrap().case_sensitive);
    app.state.close_search();

    app.state.image_preview = Some((
        agent,
        daedalus_proto::rpc::ImagePreview {
            width: 1,
            height: 1,
            rgba: vec![0; 4],
        },
    ));
    let preview = app
        .layout_for(metrics())
        .preview_panel(&app.state, 1.)
        .unwrap();
    click(app, preview);
    assert!(app.state.image_preview.is_none());

    let agent_header = app
        .layout_for(metrics())
        .pane(PaneRole::Agent)
        .unwrap()
        .action_pill
        .unwrap();
    click(app, agent_header);
    assert!(app.state.picker.open);
    app.state.picker.close();

    let new_workspace = app
        .layout_for(metrics())
        .sidebar_content
        .unwrap()
        .new_workspace;
    click(app, new_workspace);
    assert!(app.state.workspace_form.open);
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "run under the isolated Xvfb display in the quality gate"]
fn terminal_copy_and_paste_use_the_real_clipboard_and_focused_pane() {
    use daedalus_proto::{Cell, GridRow};
    use daedalus_ui::selection::{CellPos, Selection};

    let mut workbench = Workbench::new();
    let mut ws = workspace("Clipboard");
    ws.panes.truncate(1);
    let agent = ws.panes[0].id;
    workbench.app.state.set_workspaces(vec![ws]);
    workbench.app.handle_ui_action(UiAction::CopySelection);
    assert!(workbench.app.state.toast.is_none());

    let pane = workbench.app.state.panes.get_mut(&agent).unwrap();
    pane.viewport_rows = vec![GridRow {
        row_index: 0,
        abs_row: 0,
        wraps: false,
        links: vec![],
        cells: "hello"
            .chars()
            .map(|c| Cell {
                text: c.to_string(),
                ..Default::default()
            })
            .collect(),
    }];
    pane.selection = Some(Selection::new(CellPos::new(0, 0), CellPos::new(4, 0)));
    workbench.app.handle_ui_action(UiAction::CopySelection);
    assert_eq!(workbench.app.clipboard_text().as_deref(), Some("hello"));
    workbench.app.handle_ui_action(UiAction::PasteClipboard);
    assert!(
        matches!(workbench.command(), UiCommand::Paste { pane_id, data }
        if pane_id == agent && data == b"hello")
    );
    assert!(workbench.app.state.panes[&agent].selection.is_none());

    workbench
        .app
        .handle_daemon_event(UiEvent::PaneClosed { pane_id: agent });
    workbench.app.handle_ui_action(UiAction::PasteClipboard);
    assert!(workbench.idle());
}
