use super::*;
use daedalus_ui::layout::{Rect, overlays, settings_button, sidebar_workspace_close_rect};
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

#[test]
fn pointer_opens_workspace_settings_from_the_sidebar_heading() {
    let mut workbench = Workbench::new();
    let app = &mut workbench.app;
    app.state
        .set_workspaces(vec![workspace("Sidebar controls")]);
    app.state.sidebar_override = Some(true);

    let layout = app.layout_for(metrics());
    let header = layout.sidebar_toggle.unwrap();
    let arrows = layout.sidebar_content.unwrap().arrows;
    click(app, settings_button(header, arrows, 1.));
    assert!(app.state.settings.open);
}

#[test]
fn pointer_switches_layout_and_focus_and_releases_resized_divider() {
    let mut workbench = Workbench::new();
    let app = &mut workbench.app;
    app.state.set_workspaces(vec![workspace("Split controls")]);

    let layout = app.layout_for(metrics());
    assert!(app.state.zoomed);
    click(app, layout.layout_button.unwrap());
    assert!(!app.state.zoomed);

    let layout = app.layout_for(metrics());
    assert!(!layout.stacked);
    let agent = layout.pane(PaneRole::Agent).unwrap().outer;
    let service = layout.pane(PaneRole::Service).unwrap();
    let divider_x = (agent.right() + service.outer.x) / 2.;
    let divider_y = agent.y + agent.height / 2.;
    let split = app.pointer.splits.portable(true).splits["column"];
    app.move_pointer(divider_x, divider_y, Some(metrics()));
    app.click_at(metrics());
    assert!(app.pointer.dragging_divider());
    app.move_pointer(divider_x + 80., divider_y, Some(metrics()));
    app.handle_left_mouse_up();
    assert!(!app.pointer.dragging_divider());
    let resized = app.pointer.splits.portable(true).splits["column"];
    assert!(resized > split);
    app.move_pointer(divider_x, divider_y, Some(metrics()));
    assert_eq!(app.pointer.splits.portable(true).splits["column"], resized);

    let header = app
        .layout_for(metrics())
        .pane(PaneRole::Service)
        .unwrap()
        .header;
    click(app, header);
    assert_eq!(app.state.focused_role, PaneRole::Service);
}

#[test]
fn pointer_drag_selects_cells_from_a_visible_terminal_row() {
    use daedalus_proto::{Cell, GridRow};
    use daedalus_ui::selection::{CellPos, Selection};

    let mut workbench = Workbench::new();
    let ws = workspace("Select cells");
    let agent = ws.panes[0].id;
    let app = &mut workbench.app;
    app.state.set_workspaces(vec![ws]);
    app.state.panes.get_mut(&agent).unwrap().viewport_rows = vec![GridRow {
        row_index: 0,
        abs_row: 42,
        wraps: false,
        links: vec![],
        cells: "selected"
            .chars()
            .map(|c| Cell {
                text: c.to_string(),
                ..Default::default()
            })
            .collect(),
    }];
    let pane = app
        .layout_for(metrics())
        .pane(PaneRole::Agent)
        .unwrap()
        .viewport;
    app.move_pointer(pane.x + 9., pane.y + 2., Some(metrics()));
    app.click_at(metrics());
    app.move_pointer(pane.x + 41., pane.y + 2., Some(metrics()));
    app.handle_left_mouse_up();
    assert_eq!(
        app.state.panes[&agent].selection,
        Some(Selection::new(CellPos::new(1, 42), CellPos::new(5, 42)))
    );
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
