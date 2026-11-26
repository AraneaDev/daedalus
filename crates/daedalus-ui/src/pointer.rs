mod dividers;
mod overlays;
mod selection;
mod terminal_mouse;

use super::{DaedalusApp, ViewportMetrics};
use daedalus_proto::{PaneId, PaneRole};
use daedalus_ui::input::UiAction;
use daedalus_ui::layout::overlays::search_bar_rect;
use daedalus_ui::layout::{Divider, HitTarget, PaneSplits, WorkspaceLayout, hit_test};
use daedalus_ui::selection::CellPos;
use daedalus_ui::state::AppState;
use std::time::Instant;

use dividers::{DividerDrag, divider_at};
use overlays::search_click;
use terminal_mouse::TerminalPress;

#[derive(Clone, Copy)]
enum PreviewTarget {
    Button,
    Panel,
}

fn preview_at(
    layout: &WorkspaceLayout,
    state: &AppState,
    scale: f32,
    (x, y): (f32, f32),
) -> Option<PreviewTarget> {
    if layout
        .preview_panel(state, scale)
        .is_some_and(|rect| rect.contains(x, y))
    {
        return Some(PreviewTarget::Panel);
    }
    layout
        .preview_button(state, scale)
        .filter(|rect| rect.contains(x, y))
        .map(|_| PreviewTarget::Button)
}

#[derive(Default)]
pub(super) struct PointerState {
    pub(super) position: (f32, f32),
    pub(super) splits: PaneSplits,
    hovered_url: Option<String>,
    divider_drag: Option<DividerDrag>,
    drag_anchor: Option<(PaneRole, PaneId, CellPos)>,
    last_click: Option<(Instant, PaneId, u16, u16)>,
    terminal_press: Option<TerminalPress>,
    terminal_hover: Option<(PaneId, u16, u16)>,
}

impl PointerState {
    pub(super) fn dragging_divider(&self) -> bool {
        self.divider_drag.is_some()
    }

    pub(super) fn end_drag(&mut self) {
        self.divider_drag = None;
        self.drag_anchor = None;
    }

    pub(super) fn cancel_gestures(&mut self) {
        self.end_drag();
        self.last_click = None;
        self.terminal_press = None;
        self.terminal_hover = None;
    }
}

impl DaedalusApp {
    pub(super) fn handle_left_mouse_down(&mut self) {
        if let Some(metrics) = self.viewport_metrics() {
            self.click_at(metrics);
        }
    }

    pub(super) fn click_at(&mut self, metrics: ViewportMetrics) {
        if !self.dispatch_overlay_click(metrics) {
            self.handle_viewport_click(metrics);
        }
    }

    fn handle_viewport_click(&mut self, metrics: ViewportMetrics) {
        let ViewportMetrics {
            scale_factor,
            cell: (cell_w, cell_h),
            ..
        } = metrics;
        let (x, y) = self.pointer.position;
        let layout = self.layout_for(metrics);
        if self.handle_viewport_controls(&layout, scale_factor) {
            return;
        }
        let target = hit_test(
            &layout,
            self.state.workspaces.len(),
            scale_factor,
            cell_w,
            cell_h,
            x,
            y,
        );

        self.activate_hit_target(&layout, target);
    }

    fn handle_viewport_controls(&mut self, layout: &WorkspaceLayout, scale: f32) -> bool {
        let (x, y) = self.pointer.position;
        if let Some(target) = preview_at(layout, &self.state, scale, (x, y)) {
            match target {
                PreviewTarget::Panel => {
                    self.state.image_preview = None;
                    self.request_redraw();
                }
                PreviewTarget::Button => self.handle_ui_action(UiAction::PreviewImage),
            }
            return true;
        }
        if let Some(drag) = divider_at(layout, x, y, scale) {
            self.pointer.divider_drag = Some(drag);
            return true;
        }
        if let Some(search) = self.state.search.as_ref()
            && let Some(role) = self
                .state
                .active_workspace()
                .and_then(|ws| {
                    self.state
                        .workspace_panes(ws)
                        .find(|p| p.id == search.pane_id)
                })
                .map(|p| p.role)
            && let Some(pane_rect) = layout.pane(role)
        {
            let bar = search_bar_rect(pane_rect, scale);
            if let Some(action) = search_click(bar, x, y) {
                self.handle_ui_action(action);
                return true;
            }
        }
        self.state.recap.keyboard_row = None;
        if let Some((_, row)) = self
            .renderer
            .as_ref()
            .into_iter()
            .flat_map(|r| r.recap_targets())
            .find(|(rect, _)| rect.contains(x, y))
        {
            if self.state.focus_recap_row(row) {
                self.schedule_resize_flush();
                self.request_redraw();
            }
            return true;
        }
        false
    }

    fn activate_hit_target(&mut self, layout: &WorkspaceLayout, target: HitTarget) {
        match target {
            HitTarget::SidebarPageUp | HitTarget::SidebarPageDown => {
                if let Some(list) = &layout.sidebar_content {
                    let page = list.visible.len() as isize;
                    self.scroll_sidebar(
                        list,
                        if target == HitTarget::SidebarPageUp {
                            -page
                        } else {
                            page
                        },
                    );
                }
            }
            HitTarget::ToggleLayout => self.handle_ui_action(UiAction::ToggleZoom),
            HitTarget::ToggleSidebar => self.handle_ui_action(UiAction::ToggleSidebar),
            HitTarget::SidebarFailedRun(workspace) => {
                self.handle_ui_action(UiAction::OpenFailedRun(workspace));
            }
            HitTarget::SidebarWorkspace(idx) => {
                if let Some(id) = self.state.workspaces.get(idx).map(|workspace| workspace.id) {
                    self.activate_workspace(id);
                }
            }
            HitTarget::SidebarWorkspaceClose(idx) => {
                if let Some(ws_id) = self.state.workspaces.get(idx).map(|ws| ws.id) {
                    self.handle_ui_action(UiAction::CloseWorkspaceFromSidebar(ws_id));
                }
            }
            HitTarget::Widget(_) | HitTarget::CloseSession(_) | HitTarget::SelectSession(_) => {
                self.activate_session(target)
            }
            HitTarget::SessionTab(role) => self.handle_ui_action(UiAction::FocusRole(role)),
            HitTarget::RunCargo(preset) => self.handle_ui_action(UiAction::RunCargo(preset)),
            HitTarget::RunTests => self.handle_ui_action(UiAction::RunTests),
            HitTarget::CustomWidget(index) => self.open_widget_details(index),
            HitTarget::WorkspaceSettings => self.open_workspace_settings(),
            HitTarget::SidebarNewWorkspace => self.create_new_workspace(),
            HitTarget::SidebarNewWorktree => self.open_worktree_modal(),
            HitTarget::PaneHeaderAction(PaneRole::Agent) => {
                self.state.focus_clicked_pane(PaneRole::Agent);
                self.open_agent_picker();
            }
            HitTarget::PaneHeaderAction(PaneRole::Runner) => {
                self.forward_runner_failure_to_agent();
            }
            HitTarget::PaneHeaderAction(role) | HitTarget::PaneHeader(role) => {
                self.state.focus_clicked_pane(role);
                self.request_redraw();
            }
            HitTarget::PaneViewport { role, col, row } => {
                self.handle_terminal_press(role, col, row);
            }
            HitTarget::None => {}
        }
    }

    pub(super) fn handle_left_mouse_up(&mut self) {
        let resized = self.pointer.divider_drag.is_some();
        self.pointer.end_drag();
        if resized {
            self.save_workbench_layout();
        }
        self.release_terminal_mouse();
    }

    pub(super) fn update_link_hover(&mut self, url: Option<String>) {
        if self.pointer.hovered_url != url {
            if let Some(window) = &self.window {
                let title = url
                    .as_ref()
                    .map(|url| format!("Daedalus Workbench — {url}"));
                window.set_title(title.as_deref().unwrap_or("Daedalus Workbench"));
            }
            self.pointer.hovered_url = url;
        }
    }

    pub(super) fn handle_cursor_moved(&mut self, x: f32, y: f32) {
        self.move_pointer(x, y, self.viewport_metrics());
    }

    pub(super) fn move_pointer(&mut self, x: f32, y: f32, metrics: Option<ViewportMetrics>) {
        self.pointer.position = (x, y);
        if self.drag_divider(x, y) {
            return;
        }
        if self.pointer.drag_anchor.is_none()
            && let Some(metrics) = metrics
        {
            let layout = self.layout_for(metrics);
            let handle = (!self.state.zoomed)
                .then(|| divider_at(&layout, x, y, metrics.scale_factor))
                .flatten();
            let target = hit_test(
                &layout,
                self.state.workspaces.len(),
                metrics.scale_factor,
                metrics.cell.0,
                metrics.cell.1,
                x,
                y,
            );
            let viewport_available = handle.is_none()
                && !self.state.has_input_overlay()
                && preview_at(&layout, &self.state, metrics.scale_factor, (x, y)).is_none();
            self.report_terminal_motion(viewport_available.then_some(target));
            let url = if viewport_available
                && let HitTarget::PaneViewport { role, col, row } = target
                && let Some(pane) = self.state.pane_for_role(role)
                && let Some(daedalus_ui::links::DetectedLink::Url(url)) =
                    daedalus_ui::links::browser_link_at(
                        pane.visible_rows(),
                        row as usize,
                        col as usize,
                    ) {
                Some(url)
            } else {
                None
            };
            self.update_link_hover(url);
            if let Some(window) = &self.window {
                use winit::window::CursorIcon;
                window.set_cursor(match handle.map(|h| h.divider) {
                    Some(Divider::Column) => CursorIcon::ColResize,
                    Some(_) => CursorIcon::RowResize,
                    None if self.pointer.hovered_url.is_some() => CursorIcon::Pointer,
                    None => CursorIcon::Default,
                });
            }
        }

        if self.pointer.drag_anchor.is_some()
            && let Some(metrics) = metrics
        {
            self.drag_selection(metrics);
        }
    }
}
