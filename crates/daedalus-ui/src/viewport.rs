use super::DaedalusApp;
use daedalus_proto::PaneRole;
use daedalus_ui::{
    bridge::UiCommand,
    layout::{
        LayoutInput, WIDGET_BUTTON_GAP_LOGICAL, WIDGET_BUTTON_MIN_LOGICAL_WIDTH, WorkspaceLayout,
        compute_split_layout,
    },
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
        let widgets: Vec<_> = self
            .state
            .workbench
            .as_ref()
            .filter(|(id, state)| self.state.active_workspace_id == Some(*id) && state.revision > 0)
            .map(|(_, state)| {
                state
                    .document
                    .widgets
                    .iter()
                    .enumerate()
                    .filter(|(_, widget)| widget.pinned)
                    .map(|(index, _)| index)
                    .collect()
            })
            .unwrap_or_default();
        let cargo_presets = daedalus_ui::assignments::CargoPreset::ALL
            .map(|preset| self.state.cargo_preset_visible(preset));
        let cargo = cargo_presets.contains(&true);
        let show_footer =
            (cargo || !widgets.is_empty()) && size.height as f32 >= footer_height * 5.;
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
            if !widgets.is_empty() {
                layout.widget_bar = Some(bar);
                let padding = WIDGET_BUTTON_GAP_LOGICAL * scale_factor;
                let start = layout
                    .cargo_buttons(scale_factor)
                    .last()
                    .map_or(padding, |(_, rect)| rect.right() + padding);
                let room = (bar.right() - start - padding).max(0.);
                let minimum = WIDGET_BUTTON_MIN_LOGICAL_WIDTH * scale_factor;
                let max = ((room + padding) / (minimum + padding)) as usize;
                if max > 0 {
                    let visible = if widgets.len() > max {
                        max.saturating_sub(1)
                    } else {
                        widgets.len()
                    };
                    let count = visible + usize::from(widgets.len() > visible);
                    let width = ((room - (count as f32 - 1.) * padding) / count as f32)
                        .min(190. * scale_factor);
                    for (position, index) in widgets
                        .into_iter()
                        .take(visible)
                        .map(Some)
                        .chain((visible < count).then_some(None))
                        .enumerate()
                    {
                        layout.widget_buttons.push((
                            index,
                            daedalus_ui::layout::Rect::new(
                                start + position as f32 * (width + padding),
                                bar.y + padding,
                                width,
                                bar.height - padding * 2.,
                            ),
                        ));
                    }
                }
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
