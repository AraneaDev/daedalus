use super::DaedalusApp;
use daedalus_proto::PaneRole;
use daedalus_ui::{
    bridge::UiCommand,
    layout::{LayoutInput, WorkspaceLayout, compute_split_layout},
    state::ConnectionState,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

const RESIZE_DEBOUNCE: Duration = Duration::from_millis(32);

#[derive(Clone, Copy)]
pub(super) struct ViewportMetrics {
    pub(super) size: winit::dpi::PhysicalSize<u32>,
    pub(super) scale_factor: f32,
    pub(super) cell: (f32, f32),
}

impl DaedalusApp {
    pub(super) fn schedule_resize_flush(&mut self) {
        self.pending_resize_flush_at = Some(Instant::now() + RESIZE_DEBOUNCE);
    }

    pub(super) fn has_top_banner(&self) -> bool {
        self.state.connection != ConnectionState::Connected
            || self.state.toast.is_some()
            || self.state.persistence_warning().is_some()
    }

    pub(super) fn viewport_metrics(&self) -> Option<ViewportMetrics> {
        let window = self.window.as_ref()?;
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return None;
        }
        Some(ViewportMetrics {
            size,
            scale_factor: window.scale_factor() as f32,
            cell: self.renderer.as_ref()?.cell_size(),
        })
    }

    pub(super) fn compute_current_layout(&self) -> Option<WorkspaceLayout> {
        Some(self.layout_for(self.viewport_metrics()?))
    }

    fn pane_geometry(&self, metrics: ViewportMetrics) -> WorkspaceLayout {
        let ViewportMetrics {
            size,
            scale_factor,
            cell: (cell_w, cell_h),
        } = metrics;
        let footer_height = daedalus_ui::layout::CARGO_BAR_LOGICAL_HEIGHT * scale_factor;
        let widget_document = self
            .state
            .workbench
            .as_ref()
            .filter(|(id, state)| self.state.active_workspace_id == Some(*id) && state.revision > 0)
            .map(|(_, state)| &state.document)
            .filter(|document| document.widgets.iter().any(|widget| widget.pinned));
        let cargo_presets = daedalus_ui::assignments::CargoPreset::ALL
            .map(|preset| self.state.cargo_preset_visible(preset));
        let cargo = cargo_presets.contains(&true);
        let show_footer =
            (cargo || widget_document.is_some()) && size.height as f32 >= footer_height * 5.;
        let height = size.height as f32 - if show_footer { footer_height } else { 0. };
        let mut layout = compute_split_layout(
            &LayoutInput {
                window_w: size.width as f32,
                window_h: height,
                scale_factor,
                cell_w,
                cell_h,
                sidebar_override: self.state.sidebar_override,
                has_top_banner: self.has_top_banner(),
                zoomed: self.state.zoomed,
                focused_role: self.state.focused_role,
                runner_has_failure: self.state.runner_has_failure(),
            },
            &self.pointer.splits,
        );
        if show_footer {
            let bar = daedalus_ui::layout::Rect::new(0., height, size.width as f32, footer_height);
            if cargo {
                layout.cargo_bar = Some(bar);
                layout.cargo_presets = cargo_presets;
            }
            if let Some(document) = widget_document {
                layout.set_widget_footer(bar, document, &self.state.widget_results, scale_factor);
            }
        }
        layout
    }

    pub(super) fn layout_for(&self, metrics: ViewportMetrics) -> WorkspaceLayout {
        let scale_factor = metrics.scale_factor;
        let mut layout = self.pane_geometry(metrics);
        if self.state.zoomed {
            let roles: Vec<_> = [PaneRole::Service, PaneRole::Runner]
                .into_iter()
                .filter(|&role| self.state.widget_present(role))
                .collect();
            layout.reserve_insights(&roles, scale_factor, metrics.cell.1);
        }
        if self.state.workspaces.is_empty() {
            for pane in &mut layout.panes {
                pane.action_pill = None;
            }
        }
        if self.state.zoomed
            && let Some(ws) = self.state.active_workspace()
        {
            let entries = self.state.terminal_tabs(ws);
            layout.set_named_tabs(&entries, self.state.focused_pane_id(), scale_factor);
        }
        layout.sidebar_first = self.state.sidebar_first;
        if let Some(sidebar) = layout.sidebar {
            use daedalus_ui::sidebar_content::{SidebarContent, SidebarRow};
            let list = SidebarContent::for_workspaces(
                &self.state.workspaces,
                sidebar,
                self.state.sidebar_first,
                scale_factor,
            );
            for (row, rect) in list.rows[list.visible.clone()].iter().zip(&list.rects) {
                if let SidebarRow::Workspace(index) = row {
                    let ws = &self.state.workspaces[*index];
                    if self.state.failed_runner(ws).is_some() {
                        layout.failed_run_buttons.push((
                            ws.id,
                            daedalus_ui::layout::sidebar_failed_run_rect(*rect, scale_factor),
                        ));
                    }
                }
            }
            layout.sidebar_content = Some(Arc::new(list));
        }
        if self.state.connection == ConnectionState::Connected
            && self.state.active_workspace_id.is_some()
        {
            layout.enable_launch_button(scale_factor);
        }
        if self.state.active_test_workspace().is_some() {
            layout.enable_run_tests(scale_factor);
        }
        if self.state.active_workspace_id.is_some() {
            layout.enable_layout_button(scale_factor);
        }
        layout
    }

    pub(super) fn flush_due_resizes(&mut self, force: bool) {
        if !force {
            let Some(deadline) = self.pending_resize_flush_at else {
                return;
            };
            if Instant::now() < deadline {
                return;
            }
        }
        self.pending_resize_flush_at = None;
        if self.state.connection != ConnectionState::Connected {
            return;
        }

        let Some(layout) = self.compute_current_layout() else {
            return;
        };

        self.resize_panes(&layout);
    }

    pub(super) fn resize_panes(&mut self, layout: &WorkspaceLayout) {
        let mut pending = Vec::new();
        for pane_rect in &layout.panes {
            let Some(pane_id) = self.state.pane_id_for_role(pane_rect.role) else {
                continue;
            };
            let target = (pane_rect.cols.max(1), pane_rect.rows.max(1));
            if self.state.last_sent_sizes.get(&pane_id).copied() != Some(target) {
                pending.push((pane_id, target.0, target.1));
            }
        }

        for (pane_id, cols, rows) in pending {
            if self.send_command(UiCommand::ResizePane {
                pane_id,
                cols,
                rows,
            }) {
                self.state.last_sent_sizes.insert(pane_id, (cols, rows));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accessibility::{TerminalControls, workbench_tree_with_layout};
    use crate::{BridgeConfig, UiConfig};
    use accesskit::{Action, ActionRequest, Role, TreeId};
    use daedalus_proto::workbench::{
        NamedAction, WidgetAction, WidgetDefinition, WidgetDisplay, WidgetElement, WidgetInstance,
        WidgetResult, WidgetSnapshot, WorkbenchState, WorkspaceDocument,
    };
    use daedalus_proto::{StatusLevel, WorkspaceId};
    use daedalus_ui::layout::{HitTarget, hit_test};
    use std::path::Path;

    fn app_with_composed_footer(profile: &Path) -> DaedalusApp {
        let mut app = DaedalusApp::new(
            BridgeConfig {
                socket_path: profile.join("daemon.sock"),
                auth_token: None,
                initial_dir: profile.into(),
                default_agent: Default::default(),
                bootstrap_explicit_dir: false,
                auto_spawn_daemon: false,
            },
            UiConfig::default(),
            None,
            &profile.join("profile.toml"),
        );
        let workspace = WorkspaceId::new();
        app.state.active_workspace_id = Some(workspace);
        app.state.workspace_list_loaded = true;
        let mut document = WorkspaceDocument::new("Build".into());
        document.widgets.push(WidgetInstance {
            id: "build".into(),
            name: "Build pipeline".into(),
            pinned: true,
            template: None,
            definition: Some(WidgetDefinition {
                display: WidgetDisplay {
                    elements: vec![
                        WidgetElement::Label {
                            text: "Build pipeline".into(),
                        },
                        WidgetElement::Button {
                            action: "test".into(),
                        },
                        WidgetElement::Status,
                        WidgetElement::Button {
                            action: "deploy".into(),
                        },
                    ],
                    ..Default::default()
                },
                actions: [("test", "Run tests"), ("deploy", "Open service results")]
                    .into_iter()
                    .map(|(id, label)| NamedAction {
                        id: id.into(),
                        label: label.into(),
                        action: WidgetAction::Terminal {
                            role: PaneRole::Runner,
                        },
                    })
                    .collect(),
                ..Default::default()
            }),
        });
        app.state.workbench = Some((
            workspace,
            WorkbenchState {
                revision: 1,
                document,
                repo_baseline: None,
            },
        ));
        app
    }

    fn layout_at(app: &DaedalusApp, width: f32, scale: f32) -> WorkspaceLayout {
        app.layout_for(ViewportMetrics {
            size: ((width * scale) as u32, (500. * scale) as u32).into(),
            scale_factor: scale,
            cell: (8. * scale, 16. * scale),
        })
    }

    #[test]
    fn composed_footer_expands_for_distinct_actions_and_collapses_to_details() {
        let profile = tempfile::tempdir().unwrap();
        let app = app_with_composed_footer(profile.path());
        for scale in [1., 2.] {
            let wide = layout_at(&app, 800., scale);
            let targets = |layout: &WorkspaceLayout| {
                let bounds = layout.widget_buttons[0].1;
                (bounds.x as u32..bounds.right() as u32)
                    .map(|x| {
                        hit_test(
                            layout,
                            0,
                            scale,
                            8. * scale,
                            16. * scale,
                            x as f32,
                            bounds.y + bounds.height * 0.5,
                        )
                    })
                    .collect::<Vec<_>>()
            };
            assert!(wide.widget_buttons[0].1.width > 190. * scale);
            let wide_hits = targets(&wide);
            let first = wide_hits.iter().position(|hit| {
                *hit == HitTarget::WidgetElementAction {
                    widget: 0,
                    element: 1,
                }
            });
            let second = wide_hits.iter().position(|hit| {
                *hit == HitTarget::WidgetElementAction {
                    widget: 0,
                    element: 3,
                }
            });
            assert!(matches!((first, second), (Some(a), Some(b)) if a < b));
            assert!(
                targets(&layout_at(&app, 240., scale))
                    .iter()
                    .all(|hit| *hit == HitTarget::CustomWidget(Some(0)))
            );
        }
    }

    #[test]
    fn composed_footer_accesskit_buttons_match_pointer_bounds_and_expire_when_collapsed() {
        use daedalus_proto::workbench::{WidgetResult, WidgetSnapshot};

        let profile = tempfile::tempdir().unwrap();
        let mut app = app_with_composed_footer(profile.path());
        app.state.widget_results.push(WidgetSnapshot {
            id: "build".into(),
            result: WidgetResult {
                label: "Ready".into(),
                level: daedalus_proto::StatusLevel::Ok,
                details: Vec::new(),
                element_values: Default::default(),
            },
        });
        let mut controls = TerminalControls::default();
        let wide = layout_at(&app, 800., 1.);
        let tree = workbench_tree_with_layout(
            &app.state,
            800.,
            500.,
            1.,
            &mut controls,
            None,
            Some(&wide),
        );
        let buttons = [("Run tests", 1), ("Open service results", 3)].map(|(label, index)| {
            let (id, button) = tree
                .nodes
                .iter()
                .find(|(_, node)| node.label() == Some(label))
                .unwrap();
            assert_eq!(button.role(), Role::Button);
            let rect = wide.widget_row(Some(0)).unwrap().cells[index].bounds;
            let button_bounds = button.bounds().unwrap();
            assert_eq!(
                (button_bounds.x0, button_bounds.x1),
                (f64::from(rect.x), f64::from(rect.right()))
            );
            let click = ActionRequest {
                action: Action::Click,
                target_tree: TreeId::ROOT,
                target_node: *id,
                data: None,
            };
            assert_eq!(
                controls.action_with_layout(&app.state, &click, || Some(wide.clone())),
                Some(HitTarget::WidgetElementAction {
                    widget: 0,
                    element: index
                })
            );
            click
        });

        let narrow = layout_at(&app, 240., 1.);
        for request in &buttons {
            assert_eq!(
                controls.action_with_layout(&app.state, request, || Some(narrow.clone())),
                None,
                "a click queued before the resized tree is published must not activate a hidden button",
            );
        }
        let tree = workbench_tree_with_layout(
            &app.state,
            240.,
            500.,
            1.,
            &mut controls,
            None,
            Some(&narrow),
        );
        assert!(tree.nodes.iter().any(|(_, node)| node.label()
            == Some("Open Build pipeline widget")
            && node.description() == Some("OK · Ready")));
        for request in buttons {
            assert!(!tree.nodes.iter().any(|(id, _)| *id == request.target_node));
            assert_eq!(
                controls.action_with_layout(&app.state, &request, || Some(narrow.clone())),
                None
            );
        }
    }

    #[test]
    fn composed_footer_accesskit_buttons_expire_across_an_overlay() {
        let profile = tempfile::tempdir().unwrap();
        let mut app = app_with_composed_footer(profile.path());
        let mut controls = TerminalControls::default();
        let wide = layout_at(&app, 800., 1.);
        let tree = workbench_tree_with_layout(
            &app.state,
            800.,
            500.,
            1.,
            &mut controls,
            None,
            Some(&wide),
        );
        let old_action = tree
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("Run tests"))
            .unwrap()
            .0;
        let request = ActionRequest {
            action: Action::Click,
            target_tree: TreeId::ROOT,
            target_node: old_action,
            data: None,
        };

        app.state.settings.begin(Vec::new());
        workbench_tree_with_layout(&app.state, 800., 500., 1., &mut controls, None, None);
        app.state.settings.open = false;
        assert_eq!(
            controls.action_with_layout(&app.state, &request, || Some(wide.clone())),
            None
        );
        let tree = workbench_tree_with_layout(
            &app.state,
            800.,
            500.,
            1.,
            &mut controls,
            None,
            Some(&wide),
        );
        let current_action = tree
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("Run tests"))
            .unwrap()
            .0;
        assert_ne!(current_action, old_action);
    }

    #[test]
    fn composed_footer_actions_keep_their_bounds_when_status_grows() {
        let profile = tempfile::tempdir().unwrap();
        let mut app = app_with_composed_footer(profile.path());
        let mut action_bounds = |label| {
            app.state.widget_results = vec![WidgetSnapshot {
                id: "build".into(),
                result: WidgetResult {
                    label,
                    level: StatusLevel::Warn,
                    details: Vec::new(),
                    element_values: Default::default(),
                },
            }];
            let layout = layout_at(&app, 800., 1.);
            let row = layout.widget_row(Some(0)).unwrap();
            [row.cells[1].bounds, row.cells[3].bounds]
        };
        assert_eq!(
            action_bounds("Pending".into()),
            action_bounds("Running a long sequence of checks. ".repeat(60)),
        );
    }
}
