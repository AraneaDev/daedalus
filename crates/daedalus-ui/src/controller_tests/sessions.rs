use super::*;
use daedalus_ui::{
    assignments::Assignment,
    bridge::AssignmentLaunch,
    layout::{
        Rect,
        overlays::{action_palette_layout, picker_item_rect},
        settings_button,
    },
    project_setups::ProjectSetups,
};

fn click(app: &mut DaedalusApp, metrics: ViewportMetrics, rect: Rect) {
    app.handle_cursor_moved(rect.x + rect.width / 2., rect.y + rect.height / 2.);
    app.click_at(metrics);
}

fn metrics(width: u32, scale: u32) -> ViewportMetrics {
    ViewportMetrics {
        size: (width * scale, 550 * scale).into(),
        scale_factor: scale as f32,
        cell: (8. * scale as f32, 16. * scale as f32),
    }
}

fn runner() -> Assignment {
    Assignment {
        id: "saved-checks".into(),
        name: "Project checks".into(),
        role: PaneRole::Runner,
        argv: vec!["cargo".into(), "test".into()],
    }
}

#[test]
fn workbench_launch_opens_saved_sessions_and_stops_live_panes_without_saving() {
    let mut w = Workbench::new();
    let mut ws = workspace("Sessions project");
    ws.panes.truncate(1);
    let agent = ws.panes[0].id;
    let id = ws.id;
    let runner = runner();
    ws.setup = Some(vec![runner.clone()]);
    w.app.project_setups.set_daemon_backed(true);
    w.app.project_setups.absorb(std::slice::from_ref(&ws));
    w.app.state.set_workspaces(vec![ws]);

    for (zoomed, dpi) in [(false, 1), (true, 2)] {
        w.app.state.zoomed = zoomed;
        w.app.state.sidebar_override = Some(true);
        w.app.state.test_workspaces.insert(id);
        let metrics = metrics(320, dpi);
        let layout = w.app.layout_for(metrics);
        let launch = layout.launch_button.unwrap();
        let settings = settings_button(
            layout.sidebar_toggle.unwrap(),
            layout.sidebar_content.as_ref().and_then(|list| list.arrows),
            metrics.scale_factor,
        );
        assert!(launch.right() <= settings.x);
        assert!(layout.layout_button.unwrap().right() <= launch.x);
        click(&mut w.app, metrics, launch);
        assert!(!w.app.state.settings.open);
        assert_eq!(w.app.state.palette_len(), 2);
        let (modal, _) = action_palette_layout(
            metrics.size.width as f32,
            metrics.size.height as f32,
            metrics.scale_factor,
            w.app.state.palette_len(),
            0,
        );
        let index = usize::from(zoomed);
        click(
            &mut w.app,
            metrics,
            picker_item_rect(modal, index, metrics.scale_factor),
        );
        if zoomed {
            assert!(matches!(w.command(), UiCommand::ClosePane(pane) if pane == agent));
        } else {
            assert!(matches!(
                w.command(),
                UiCommand::LaunchAssignment { workspace_id, assignment, launch: AssignmentLaunch::ReviewedRunner, .. }
                    if workspace_id == id && assignment == runner
            ));
        }
        assert!(w.idle());
    }
    w.app
        .handle_daemon_event(UiEvent::PaneClosed { pane_id: agent });
    w.app.handle_ui_action(UiAction::OpenSessions);
    assert_eq!(w.app.state.palette_len(), 1);
    w.app.handle_ui_action(UiAction::ActionPaletteConfirm);
    assert!(matches!(
        w.command(),
        UiCommand::LaunchAssignment { workspace_id, assignment, .. }
            if workspace_id == id && assignment == runner
    ));
    assert!(w.idle());
}

#[test]
fn reopening_launch_reads_current_setup_and_rejects_stale_choices() {
    let mut w = Workbench::new();
    let ws = workspace("Changed setup");
    let id = ws.id;
    w.app.state.set_workspaces(vec![ws]);
    w.app.project_setups.set_daemon_backed(true);
    w.app.project_setups.workspace_changed(id, vec![runner()]);
    w.app.handle_ui_action(UiAction::OpenSessions);
    let mut updated = runner();
    updated.argv.push("--locked".into());
    w.app
        .project_setups
        .workspace_changed(id, vec![updated.clone()]);
    w.app.handle_ui_action(UiAction::ActionPaletteConfirm);
    assert!(w.idle());
    assert!(w.app.state.toast.is_some());
    w.app.handle_ui_action(UiAction::OpenSessions);
    w.app.handle_ui_action(UiAction::ActionPaletteConfirm);
    assert!(
        matches!(w.command(), UiCommand::LaunchAssignment { assignment, .. } if assignment == updated)
    );
}

#[test]
fn empty_setup_can_launch_a_shell_without_writing_settings() {
    let mut w = Workbench::new();
    let mut ws = workspace("No saved setup");
    ws.panes.clear();
    let id = ws.id;
    w.app.state.set_workspaces(vec![ws]);
    w.app.config.default_agent = AgentPreset::Shell;
    w.app.project_setups = ProjectSetups::in_dir(w.root.path().join("settings"));
    w.app.handle_ui_action(UiAction::OpenSessions);
    w.app.handle_ui_action(UiAction::ActionPaletteConfirm);
    assert!(matches!(
        w.command(),
        UiCommand::LaunchAssignment { workspace_id, assignment, launch: AssignmentLaunch::Direct, .. }
            if workspace_id == id && assignment.role == PaneRole::Agent && assignment.argv.is_empty()
    ));
    assert!(!w.root.path().join("settings").exists());
}
