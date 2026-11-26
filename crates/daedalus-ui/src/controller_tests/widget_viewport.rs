use super::*;
use accesskit::{Action, ActionRequest, Role, TreeId};
use daedalus_proto::workbench::{
    NamedAction, WidgetAction, WidgetActionResult, WidgetDefinition, WidgetInstance, WidgetResult,
    WidgetSnapshot, WorkbenchState, WorkspaceDocument,
};
use daedalus_proto::{ClientMessage, ResponsePayload, StatusLevel};
use daedalus_ui::layout::{CARGO_BAR_LOGICAL_HEIGHT, HitTarget, Rect, hit_test};
use daedalus_ui::settings::Control;

fn widgets(pinned: &[bool]) -> WorkbenchState {
    let mut document = WorkspaceDocument::new("Widgets".into());
    document.widgets = pinned
        .iter()
        .enumerate()
        .map(|(index, &pinned)| WidgetInstance {
            id: format!("widget-{index}"),
            name: format!("Widget {index}"),
            pinned,
            template: None,
            definition: Some(WidgetDefinition {
                actions: vec![NamedAction {
                    id: "terminal".into(),
                    label: "Open runner terminal".into(),
                    action: WidgetAction::Terminal {
                        role: PaneRole::Runner,
                    },
                }],
                ..Default::default()
            }),
        })
        .collect();
    WorkbenchState {
        revision: 1,
        document,
        repo_baseline: None,
    }
}

fn metrics(w: u32, h: u32, dpi: u32) -> ViewportMetrics {
    ViewportMetrics {
        size: (w * dpi, h * dpi).into(),
        scale_factor: dpi as f32,
        cell: (8. * dpi as f32, 16. * dpi as f32),
    }
}

fn center(bounds: Rect) -> (f32, f32) {
    (bounds.x + bounds.width / 2., bounds.y + bounds.height / 2.)
}

#[test]
fn narrow_cargo_footer_keeps_pinned_widgets_reachable() {
    let mut w = Workbench::new();
    let mut ws = workspace("Cargo and widgets");
    ws.project.tools.push("Cargo".into());
    let id = ws.id;
    w.app.state.set_workspaces(vec![ws]);

    for dpi in [1, 2] {
        let scale = dpi as f32;
        let viewport = metrics(320, 550, dpi);
        for (pins, expected) in [(&[true][..], Some(0)), (&[true, true][..], None)] {
            w.app.state.workbench = Some((id, widgets(pins)));
            let layout = w.app.layout_for(viewport);
            let cargo = layout.cargo_buttons(scale).collect::<Vec<_>>();
            let [(index, widget)] = layout.widget_buttons.as_slice() else {
                panic!("expected one reachable widget slot at scale {scale}");
            };
            assert_eq!(*index, expected);
            assert_eq!(cargo.len(), 2);
            assert!(cargo.iter().all(|(_, rect)| rect.width >= 72. * scale));
            assert!(cargo[1].1.right() < widget.x);
            assert!(widget.right() <= viewport.size.width as f32);
            let (x, y) = center(*widget);
            assert_eq!(
                hit_test(&layout, 1, scale, viewport.cell.0, viewport.cell.1, x, y),
                HitTarget::CustomWidget(expected),
            );
        }
    }
}

fn click(app: &mut DaedalusApp, metrics: ViewportMetrics, bounds: Rect) {
    let (x, y) = center(bounds);
    app.handle_cursor_moved(x, y);
    app.click_at(metrics);
    app.pointer.end_drag();
}

#[test]
fn delayed_widget_output_keeps_the_current_details_view() {
    let mut w = Workbench::new();
    let ws = workspace("Delayed actions");
    let id = ws.id;
    w.app.state.set_workspaces(vec![ws]);
    w.app
        .handle_daemon_event(UiEvent::WorkbenchCapability(true));
    w.app.state.workbench = Some((id, widgets(&[true, true])));
    let refresh = WidgetResult {
        label: "Current details".into(),
        level: StatusLevel::Ok,
        details: vec![],
    };
    w.app.state.widget_results = [0, 1]
        .map(|index| WidgetSnapshot {
            id: format!("widget-{index}"),
            result: refresh.clone(),
        })
        .to_vec();
    let output = WidgetResult {
        label: "Earlier action output".into(),
        level: StatusLevel::Info,
        details: vec![],
    };

    for next_widget in [1, 0] {
        w.app.open_widget_details(Some(0));
        w.commands.try_iter().for_each(drop);
        let action = w.app.state.settings.activate(Control::CustomViewAction(0));
        w.app.handle_settings_action(action);
        let UiCommand::Workbench { operation, .. } = w.command() else {
            panic!("expected a widget action")
        };
        if next_widget == 0 {
            let close = w.app.state.settings.activate(Control::Cancel);
            w.app.handle_settings_action(close);
        }
        w.app.open_widget_details(Some(next_widget));
        assert_eq!(
            w.app.state.settings.heading(),
            format!("Widget {next_widget}")
        );
        w.app.handle_daemon_event(UiEvent::Workbench {
            workspace_id: id,
            operation,
            response: Box::new(Ok(ResponsePayload::WidgetAction(
                WidgetActionResult::Output(output.clone()),
            ))),
        });
        assert_eq!(w.app.state.settings.custom.preview, Some(refresh.clone()));
        assert!(!w.app.state.settings.custom.action_output);
    }
}

#[test]
fn widget_footer_keeps_pinned_document_order_next_to_cargo_at_both_scales() {
    let mut w = Workbench::new();
    let mut ws = workspace("Cargo widgets");
    ws.project.tools.push("Cargo".into());
    let id = ws.id;
    w.app.state.set_workspaces(vec![ws]);
    w.app.state.workbench = Some((id, widgets(&[false, true, false, true])));

    for dpi in [1, 2] {
        let metrics = metrics(1300, 760, dpi);
        let scale = metrics.scale_factor;
        let layout = w.app.layout_for(metrics);
        let footer = layout.widget_bar.unwrap();
        assert_eq!(layout.cargo_bar, Some(footer));
        assert_eq!(footer.bottom(), metrics.size.height as f32);
        assert_eq!(footer.height, CARGO_BAR_LOGICAL_HEIGHT * scale);
        assert!(
            layout
                .panes
                .iter()
                .all(|pane| pane.outer.bottom() <= footer.y)
        );
        let last_cargo = layout.cargo_buttons(scale).last().unwrap().1;
        let mut right = last_cargo.right();
        assert_eq!(
            layout
                .widget_buttons
                .iter()
                .map(|(index, _)| *index)
                .collect::<Vec<_>>(),
            [Some(1), Some(3)],
        );
        for (index, rect) in layout.widget_buttons.iter().copied() {
            assert!(rect.x >= right + 5. * scale);
            assert!(rect.bottom() <= footer.bottom());
            assert!(rect.right() <= footer.right());
            assert_eq!(rect.width, 190. * scale);
            right = rect.right();
            let (x, y) = center(rect);
            assert_eq!(
                hit_test(&layout, 1, scale, metrics.cell.0, metrics.cell.1, x, y),
                HitTarget::CustomWidget(index),
            );
        }
    }

    // The footer requires room for five footer heights, even with widgets and Cargo available.
    let too_short = w.app.layout_for(metrics(1300, 169, 2));
    assert!(too_short.widget_bar.is_none());
    assert!(too_short.cargo_bar.is_none());
    assert!(too_short.widget_buttons.is_empty());
    assert!(w.app.layout_for(metrics(1300, 170, 2)).widget_bar.is_some());
}

#[test]
fn widget_footer_ignores_inactive_unsaved_and_unpinned_definitions() {
    let mut w = Workbench::new();
    let a = workspace("Active");
    let b = workspace("Inactive");
    let (a_id, b_id) = (a.id, b.id);
    w.app.state.set_workspaces(vec![a, b]);
    w.app.state.select_workspace(a_id);
    let metrics = metrics(700, 460, 1);

    let pinned = widgets(&[true]);
    let mut unsaved = pinned.clone();
    unsaved.revision = 0;
    for (id, state) in [
        (b_id, pinned.clone()),
        (a_id, unsaved),
        (a_id, widgets(&[false])),
    ] {
        w.app.state.workbench = Some((id, state));
        let layout = w.app.layout_for(metrics);
        assert!(layout.widget_bar.is_none());
        assert!(layout.widget_buttons.is_empty());
    }
    w.app.state.workbench = Some((a_id, pinned));
    assert_eq!(w.app.layout_for(metrics).widget_buttons[0].0, Some(0));
}

#[test]
fn footer_overflow_opens_the_custom_list_and_details_actions_restore_terminal_focus() {
    let mut w = Workbench::new();
    let ws = workspace("Overflow");
    let id = ws.id;
    let agent = ws.panes[0].id;
    let runner = ws.panes[2].id;
    w.app.state.set_workspaces(vec![ws]);
    w.app
        .handle_daemon_event(UiEvent::WorkbenchCapability(true));
    w.app.state.workbench = Some((id, widgets(&[false, true, true, true, true])));
    w.app.state.widget_results = vec![WidgetSnapshot {
        id: "widget-1".into(),
        result: WidgetResult {
            label: "Tests ready".into(),
            level: StatusLevel::Ok,
            details: vec![("Passed".into(), "12".into())],
        },
    }];

    // A narrow footer reserves its final slot for overflow, preserving the first pinned index.
    let narrow = metrics(270, 550, 1);
    let layout = w.app.layout_for(narrow);
    assert_eq!(
        layout
            .widget_buttons
            .iter()
            .map(|(index, _)| *index)
            .collect::<Vec<_>>(),
        [Some(1), None],
    );
    let tiny = w.app.layout_for(metrics(100, 550, 1));
    assert!(tiny.widget_buttons.is_empty());
    let single_slot = w.app.layout_for(metrics(130, 550, 1));
    assert_eq!(single_slot.widget_buttons.len(), 1);
    assert_eq!(single_slot.widget_buttons[0].0, None);

    w.app.state.select_session(id, PaneRole::Agent, agent);
    click(&mut w.app, narrow, layout.widget_buttons[1].1);
    assert!(w.app.state.settings.is_custom_page());
    assert_eq!(w.app.state.settings.heading(), "Custom widgets");
    assert_eq!(w.app.state.settings.focus, Control::CustomNew);
    let cancel = w.app.state.settings.activate(Control::Cancel);
    w.app.handle_settings_action(cancel);
    assert_eq!(w.app.state.focused_pane_id(), Some(agent));

    click(&mut w.app, narrow, layout.widget_buttons[0].1);
    w.commands.try_iter().for_each(drop);
    let settings = &w.app.state.settings;
    assert_eq!(settings.heading(), "Widget 1");
    assert_eq!(settings.focus, Control::CustomRefresh);
    assert_eq!(settings.label(Control::CustomDetail(0)), "Passed · 12");

    let mut controls = accessibility::TerminalControls::default();
    let tree = accessibility::workbench_tree(&w.app.state, 270., 550., 1., &mut controls);
    let node = |name| {
        tree.nodes
            .iter()
            .find(|(_, node)| node.label() == Some(name))
            .unwrap()
    };
    assert_eq!(node("Refresh widget").0, tree.focus);
    assert_eq!(node("Passed · 12").1.role(), Role::Button);
    assert!(node("Passed · 12").1.is_disabled());
    let (target_node, action_node) = node("Open runner terminal");
    assert_eq!(action_node.role(), Role::Button);
    assert!(action_node.supports_action(Action::Click));
    let rect = settings
        .controls(settings.modal_rect(270., 550., 1.))
        .find(|(control, _)| *control == Control::CustomViewAction(0))
        .unwrap()
        .1;
    let bounds = action_node.bounds().unwrap();
    assert_eq!(
        (bounds.x0, bounds.y0, bounds.x1, bounds.y1),
        (
            f64::from(rect.x),
            f64::from(rect.y),
            f64::from(rect.right()),
            f64::from(rect.bottom())
        )
    );

    let request = |action| ActionRequest {
        action,
        target_node: *target_node,
        target_tree: TreeId::ROOT,
        data: None,
    };
    let focus = accessibility::settings_action(&mut w.app.state, &request(Action::Focus)).unwrap();
    w.app.handle_settings_action(focus);
    assert_eq!(w.app.state.settings.focus, Control::CustomViewAction(0));
    let action = accessibility::settings_action(&mut w.app.state, &request(Action::Click)).unwrap();
    w.app.handle_settings_action(action);
    let UiCommand::Workbench {
        workspace_id,
        operation,
        request,
    } = w.command()
    else {
        panic!("expected a widget action")
    };
    assert_eq!(workspace_id, id);
    assert!(
        matches!(request.as_ref(), ClientMessage::InvokeWidgetAction { widget, action, .. }
            if widget == "widget-1" && action == "terminal")
    );
    w.app.open_widget_details(Some(2));
    w.commands.try_iter().for_each(drop);
    w.app.handle_daemon_event(UiEvent::Workbench {
        workspace_id: id,
        operation,
        response: Box::new(Ok(ResponsePayload::WidgetAction(WidgetActionResult::Pane(
            runner,
        )))),
    });
    assert!(!w.app.state.settings.open);
    assert_eq!(w.app.state.focused_pane_id(), Some(runner));
    w.app.key_pressed(
        winit::keyboard::KeyCode::Tab.into(),
        &Key::Named(NamedKey::Tab),
        None,
        false,
    );
    assert!(matches!(w.command(), UiCommand::SendInput { pane_id, data }
            if pane_id == runner && data == b"\t"));
}
