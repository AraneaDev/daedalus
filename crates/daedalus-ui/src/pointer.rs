use super::{DaedalusApp, ViewportMetrics};
use daedalus_proto::{MouseTracking, PaneId, PaneRole, PaneStatus, TerminalModes};
use daedalus_ui::bridge::UiCommand;
use daedalus_ui::input::UiAction;
use daedalus_ui::layout::overlays::{
    action_palette_layout, close_modal_cancel_rect, close_modal_compose_rect,
    close_modal_confirm_rect, close_modal_force_rect, close_modal_worktree_rect,
    close_workspace_modal_rect, picker_item_rect, preset_picker_layout, search_bar_rect,
    search_controls, worktree_button_rects, worktree_modal_rect,
};
use daedalus_ui::layout::{Divider, HitTarget, PaneSplits, Rect, WorkspaceLayout, hit_test};
use daedalus_ui::selection::{CellPos, Selection, select_word_at};
use daedalus_ui::settings::SettingsAction;
use daedalus_ui::state::{ActiveOverlay, AgentPreset, AppState, PaneViewState};
use daedalus_ui::workspace_form::WorkspaceFormAction;
use std::time::{Duration, Instant};
use winit::keyboard::ModifiersState;

const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(400);

#[derive(Clone, Copy)]
struct DividerDrag {
    divider: Divider,
    origin: f32,
    extent: f32,
    offset: f32,
}

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

fn divider_at(layout: &WorkspaceLayout, x: f32, y: f32, scale: f32) -> Option<DividerDrag> {
    let agent = layout.pane(PaneRole::Agent)?.outer;
    let service = layout.pane(PaneRole::Service)?.outer;
    let runner = layout.pane(PaneRole::Runner)?.outer;
    let reach = 4. * scale;
    let drag = |divider, origin, end, center, pointer| {
        Some(DividerDrag {
            divider,
            origin,
            extent: end - origin,
            offset: center - pointer,
        })
    };
    if layout.stacked {
        if x < agent.x || x >= agent.right() {
            return None;
        }
        for (divider, center) in [
            (Divider::UpperRow, service.y),
            (Divider::LowerRow, runner.y),
        ] {
            if (y - center).abs() <= reach {
                return drag(divider, agent.y, runner.bottom(), center, y);
            }
        }
    } else {
        let gutter = (service.x - agent.right()) / 2.;
        let center = agent.right() + gutter;
        if (x - center).abs() <= reach && y >= agent.y && y <= agent.bottom() {
            return drag(
                Divider::Column,
                agent.x - gutter,
                service.right() + gutter,
                center,
                x,
            );
        }
        let center = service.bottom() + gutter;
        if x >= service.x && x <= service.right() && (y - center).abs() <= reach {
            return drag(
                Divider::RightRow,
                service.y - gutter,
                runner.bottom() + gutter,
                center,
                y,
            );
        }
    }
    None
}

#[derive(Default)]
pub(super) struct PointerState {
    pub(super) position: (f32, f32),
    pub(super) splits: PaneSplits,
    hovered_url: Option<String>,
    divider_drag: Option<DividerDrag>,
    drag_anchor: Option<(PaneRole, PaneId, CellPos)>,
    last_click: Option<(Instant, PaneRole, u16, u16)>,
    terminal_press: Option<TerminalPress>,
    terminal_hover: Option<(PaneId, u16, u16)>,
}

#[derive(Clone, Copy)]
struct TerminalPress {
    pane_id: PaneId,
    modes: TerminalModes,
    cell: (u16, u16),
}

#[derive(Clone, Copy)]
enum TerminalMouseEvent {
    Press,
    Release,
    Motion { pressed: bool },
}

fn terminal_mouse_report(
    modes: TerminalModes,
    event: TerminalMouseEvent,
    modifiers: ModifiersState,
    (col, row): (u16, u16),
) -> Option<Vec<u8>> {
    use TerminalMouseEvent::{Motion, Press, Release};
    let button = match (modes.mouse, event) {
        (MouseTracking::Off, _) => return None,
        (_, Press) => 0,
        (MouseTracking::Press, Release) => return None,
        (_, Release) if modes.sgr_mouse => 0,
        (_, Release) => 3,
        (MouseTracking::Drag | MouseTracking::Any, Motion { pressed: true }) => 32,
        (MouseTracking::Any, Motion { pressed: false }) => 35,
        (_, Motion { .. }) => return None,
    };
    let modifiers = if modes.mouse == MouseTracking::Press {
        0
    } else {
        (u8::from(modifiers.shift_key()) * 4)
            | (u8::from(modifiers.alt_key()) * 8)
            | (u8::from(modifiers.control_key()) * 16)
    };
    let code = button | modifiers;
    let x = u32::from(col) + 1;
    let y = u32::from(row) + 1;
    if modes.sgr_mouse {
        let end = if matches!(event, Release) { 'm' } else { 'M' };
        Some(format!("\x1b[<{code};{x};{y}{end}").into_bytes())
    } else {
        Some(vec![
            b'\x1b',
            b'[',
            b'M',
            code + 32,
            u8::try_from(x + 32).ok()?,
            u8::try_from(y + 32).ok()?,
        ])
    }
}

fn live_terminal_modes(pane: &PaneViewState) -> Option<TerminalModes> {
    (pane.scroll_offset == 0
        && pane.requested_scroll_offset == 0
        && !matches!(pane.summary.status, PaneStatus::Exited { .. }))
    .then_some(pane.terminal_modes)
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

    fn select_at(&mut self, pane: &mut PaneViewState, col: u16, row: u16, now: Instant) {
        let role = pane.summary.role;
        let pane_id = pane.summary.id;
        let grid_row = pane.visible_rows().get(row as usize);
        if self
            .last_click
            .is_some_and(|(ts, prev_role, prev_col, prev_row)| {
                now.duration_since(ts) <= DOUBLE_CLICK_WINDOW
                    && (prev_role, prev_col, prev_row) == (role, col, row)
            })
        {
            self.cancel_gestures();
            pane.selection = grid_row.and_then(|grid_row| select_word_at(grid_row, col));
        } else {
            self.last_click = Some((now, role, col, row));
            self.drag_anchor =
                grid_row.map(|grid_row| (role, pane_id, CellPos::new(col, grid_row.abs_row)));
            pane.selection = None;
        }
    }
}

enum OverlayClick {
    Consumed,
    Redraw,
    SendFailure,
    Setup(daedalus_ui::setup_review::SetupAction),
    WorkspaceForm(WorkspaceFormAction),
    Settings(SettingsAction),
    ConfirmAction,
    ApplyPreset(AgentPreset),
    Action(UiAction),
}

fn handle_overlay_click(
    state: &mut AppState,
    (x, y): (f32, f32),
    width: f32,
    height: f32,
    scale: f32,
) -> Option<OverlayClick> {
    let clicked_item = |modal: Rect, visible: std::ops::Range<usize>| {
        visible.enumerate().find_map(|(row, index)| {
            picker_item_rect(modal, row, scale)
                .contains(x, y)
                .then_some(index)
        })
    };
    let overlay = state.active_overlay();
    let effect = if overlay == Some(ActiveOverlay::FailureRecipients)
        && let Some(choices) = &mut state.failure_recipients
    {
        let (modal, visible) =
            action_palette_layout(width, height, scale, choices.agents.len(), choices.selected);
        if !modal.contains(x, y) {
            state.failure_recipients = None;
            OverlayClick::Redraw
        } else if let Some(index) = clicked_item(modal, visible) {
            choices.selected = index;
            OverlayClick::SendFailure
        } else {
            OverlayClick::Redraw
        }
    } else if overlay == Some(ActiveOverlay::SetupReview) {
        OverlayClick::Setup(state.setup_review.click(x, y, width, height, scale))
    } else if overlay == Some(ActiveOverlay::WorkspaceForm) {
        OverlayClick::WorkspaceForm(state.workspace_form.click(x, y, width, height, scale))
    } else if overlay == Some(ActiveOverlay::Settings) {
        OverlayClick::Settings(state.settings.click(x, y, width, height, scale))
    } else if overlay == Some(ActiveOverlay::ActionPalette) {
        let (modal, visible) = action_palette_layout(
            width,
            height,
            scale,
            state.active_actions().len(),
            state.action_palette.selected_index,
        );
        if !modal.contains(x, y) {
            state.action_palette.close();
            OverlayClick::Redraw
        } else if let Some(index) = clicked_item(modal, visible) {
            state.action_palette.selected_index = index;
            OverlayClick::ConfirmAction
        } else {
            OverlayClick::Consumed
        }
    } else if overlay == Some(ActiveOverlay::AgentPicker) {
        let (modal, visible) =
            preset_picker_layout(width, height, scale, state.picker.selected_index);
        if !modal.contains(x, y) {
            state.picker.close();
            OverlayClick::Redraw
        } else if let Some(index) = clicked_item(modal, visible) {
            state.picker.selected_index = index;
            OverlayClick::ApplyPreset(AgentPreset::ALL[index])
        } else {
            OverlayClick::Consumed
        }
    } else if overlay == Some(ActiveOverlay::Worktree) {
        let modal = worktree_modal_rect(width, height, scale);
        OverlayClick::Action(worktree_modal_click(
            modal,
            scale,
            state.can_create_worktree(),
            x,
            y,
        ))
    } else if overlay == Some(ActiveOverlay::CloseWorkspace) {
        let modal = close_workspace_modal_rect(width, height, scale);
        OverlayClick::Action(close_modal_click(modal, &state.close_modal, scale, x, y))
    } else {
        return None;
    };
    Some(effect)
}

fn search_click(bar: Rect, x: f32, y: f32) -> Option<UiAction> {
    if !bar.contains(x, y) {
        return None;
    }
    let actions = [
        UiAction::SearchPrev,
        UiAction::SearchNext,
        UiAction::SearchToggleCase,
    ];
    search_controls(bar)
        .into_iter()
        .zip(actions)
        .find(|(rect, _)| rect.contains(x, y))
        .map(|(_, action)| action)
        .or(Some(UiAction::Ignore))
}

impl DaedalusApp {
    pub(super) fn handle_left_mouse_down(&mut self) {
        if let Some(metrics) = self.viewport_metrics() {
            self.click_at(metrics);
        }
    }

    pub(super) fn click_at(&mut self, metrics: ViewportMetrics) {
        let ViewportMetrics {
            size, scale_factor, ..
        } = metrics;
        match handle_overlay_click(
            &mut self.state,
            self.pointer.position,
            size.width as f32,
            size.height as f32,
            scale_factor,
        ) {
            Some(OverlayClick::Consumed) => {}
            Some(OverlayClick::Redraw) => self.request_redraw(),
            Some(OverlayClick::SendFailure) => {
                if let Some(choices) = self.state.failure_recipients.take() {
                    self.send_failure_to_recipient(choices);
                }
                self.request_redraw();
            }
            Some(OverlayClick::Setup(action)) => self.handle_setup_action(action),
            Some(OverlayClick::WorkspaceForm(action)) => self.handle_workspace_form(action),
            Some(OverlayClick::Settings(action)) => self.handle_settings_action(action),
            Some(OverlayClick::ConfirmAction) => self.confirm_workspace_action(),
            Some(OverlayClick::ApplyPreset(preset)) => self.apply_agent_preset(preset, false),
            Some(OverlayClick::Action(action)) => self.handle_ui_action(action),
            None => self.handle_viewport_click(metrics),
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
                    self.open_close_modal_for_workspace(ws_id);
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

    fn handle_terminal_press(&mut self, role: PaneRole, col: u16, row: u16) {
        self.state.focus_clicked_pane(role);
        if is_link_click_modifier(self.modifiers) {
            self.open_clicked_link(role, col, row);
            self.request_redraw();
            return;
        }
        if let Some(pane_id) = self.state.pane_id_for_role(role)
            && let Some(pane) = self.state.panes.get_mut(&pane_id)
        {
            let cell = (col, row);
            if !self.modifiers.shift_key()
                && let Some(modes) = live_terminal_modes(pane)
                && let Some(data) =
                    terminal_mouse_report(modes, TerminalMouseEvent::Press, self.modifiers, cell)
            {
                self.pointer.last_click = None;
                self.pointer.terminal_hover = None;
                if self.send_command(UiCommand::SendInput { pane_id, data }) {
                    self.pointer.terminal_press = Some(TerminalPress {
                        pane_id,
                        modes,
                        cell,
                    });
                }
            } else {
                self.pointer.select_at(pane, col, row, Instant::now());
            }
        }
        self.request_redraw();
    }

    pub(super) fn handle_left_mouse_up(&mut self) {
        let resized = self.pointer.divider_drag.is_some();
        self.pointer.end_drag();
        if resized {
            self.save_workbench_layout();
        }
        if let Some(press) = self.pointer.terminal_press.take()
            && let Some(pane) = self.state.panes.get(&press.pane_id)
            && !matches!(pane.summary.status, PaneStatus::Exited { .. })
            && pane.terminal_modes.mouse != MouseTracking::Off
            && pane.terminal_modes.sgr_mouse == press.modes.sgr_mouse
            && let Some(data) = terminal_mouse_report(
                press.modes,
                TerminalMouseEvent::Release,
                self.modifiers,
                press.cell,
            )
        {
            self.send_command(UiCommand::SendInput {
                pane_id: press.pane_id,
                data,
            });
        }
    }

    fn report_terminal_motion(&mut self, target: Option<HitTarget>) {
        let Some(HitTarget::PaneViewport { role, col, row }) = target else {
            self.pointer.terminal_hover = None;
            return;
        };
        let Some(pane) = self.state.pane_for_role(role) else {
            return;
        };
        let pane_id = pane.summary.id;
        let Some(modes) = live_terminal_modes(pane) else {
            return;
        };
        let cell = (col, row);
        let pressed = if let Some(press) = &mut self.pointer.terminal_press {
            if press.pane_id != pane_id
                || press.cell == cell
                || (!press.modes.sgr_mouse && (col >= 223 || row >= 223))
            {
                return;
            }
            press.cell = cell;
            true
        } else {
            if self.pointer.terminal_hover == Some((pane_id, col, row))
                || modes.mouse != MouseTracking::Any
                || self.modifiers.shift_key()
            {
                return;
            }
            self.pointer.terminal_hover = Some((pane_id, col, row));
            false
        };
        if let Some(data) = terminal_mouse_report(
            modes,
            TerminalMouseEvent::Motion { pressed },
            self.modifiers,
            cell,
        ) {
            self.send_command(UiCommand::SendInput { pane_id, data });
        }
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
        if let Some(drag) = self.pointer.divider_drag {
            let coordinate = if drag.divider == Divider::Column {
                x
            } else {
                y
            };
            if drag.extent > 0. {
                self.pointer.splits.move_divider(
                    drag.divider,
                    (coordinate + drag.offset - drag.origin) / drag.extent,
                );
                self.schedule_resize_flush();
                self.request_redraw();
            }
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

    pub(super) fn drag_selection(&mut self, metrics: ViewportMetrics) {
        let Some((drag_role, drag_pane_id, anchor)) = self.pointer.drag_anchor else {
            return;
        };
        let (x, y) = self.pointer.position;
        let (cell_w, cell_h) = metrics.cell;
        let layout = self.layout_for(metrics);
        let Some(pane_rect) = layout.pane(drag_role) else {
            return;
        };

        let col = if cell_w > 0.0 {
            (((x - pane_rect.viewport.x) / cell_w).floor().max(0.0) as u16)
                .min(pane_rect.cols.saturating_sub(1))
        } else {
            0
        };
        let row = if cell_h > 0.0 {
            (((y - pane_rect.viewport.y) / cell_h).floor().max(0.0) as u16)
                .min(pane_rect.rows.saturating_sub(1))
        } else {
            0
        };

        if let Some(pane) = self.state.panes.get_mut(&drag_pane_id)
            && extend_selection(pane, anchor, col, row)
        {
            self.request_redraw();
        }
    }
}

fn extend_selection(pane: &mut PaneViewState, anchor: CellPos, col: u16, row: u16) -> bool {
    let Some(grid_row) = pane.visible_rows().get(row as usize) else {
        return false;
    };
    let head = CellPos::new(col, grid_row.abs_row);
    if head == anchor && pane.selection.is_none() {
        return false;
    }
    pane.selection = Some(Selection::new(anchor, head));
    true
}

fn worktree_modal_click(
    modal: daedalus_ui::layout::Rect,
    scale: f32,
    can_create: bool,
    x: f32,
    y: f32,
) -> UiAction {
    let (cancel, create) = worktree_button_rects(modal, scale);
    if !modal.contains(x, y) || cancel.contains(x, y) {
        UiAction::WorktreeModalCancel
    } else if can_create && create.contains(x, y) {
        UiAction::WorktreeModalConfirm
    } else {
        UiAction::Ignore
    }
}

fn close_modal_click(
    modal: daedalus_ui::layout::Rect,
    state: &daedalus_ui::state::CloseWorkspaceModalState,
    scale: f32,
    x: f32,
    y: f32,
) -> UiAction {
    if !modal.contains(x, y) || close_modal_cancel_rect(modal, scale).contains(x, y) {
        UiAction::CloseModalCancel
    } else if close_modal_confirm_rect(modal, scale).contains(x, y) {
        UiAction::CloseModalConfirm
    } else if close_modal_compose_rect(modal, state, scale).contains(x, y) {
        UiAction::CloseModalToggleCompose
    } else if close_modal_worktree_rect(modal, state, scale).is_some_and(|r| r.contains(x, y)) {
        UiAction::CloseModalToggleWorktree
    } else if close_modal_force_rect(modal, state, scale).is_some_and(|r| r.contains(x, y)) {
        UiAction::CloseModalToggleForce
    } else {
        UiAction::Ignore
    }
}

fn is_link_click_modifier_for_platform(mods: ModifiersState, is_macos: bool) -> bool {
    if is_macos {
        mods.super_key() && !mods.control_key() && !mods.alt_key()
    } else {
        mods.control_key() && !mods.super_key() && !mods.alt_key()
    }
}

fn is_link_click_modifier(mods: ModifiersState) -> bool {
    is_link_click_modifier_for_platform(mods, cfg!(target_os = "macos"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use daedalus_ui::{input::translate_key, state::ConnectionState};

    #[test]
    fn terminal_mouse_formats_legacy_clicks_and_sgr_motion() {
        use TerminalMouseEvent::{Motion, Press, Release};
        let mut modes = TerminalModes {
            mouse: MouseTracking::Button,
            ..TerminalModes::default()
        };
        let modifiers = ModifiersState::empty();
        let cell = (1, 2);
        assert_eq!(
            terminal_mouse_report(modes, Press, modifiers, cell).as_deref(),
            Some(&b"\x1b[M \"#"[..])
        );
        assert_eq!(
            terminal_mouse_report(modes, Release, modifiers, cell).as_deref(),
            Some(&b"\x1b[M#\"#"[..])
        );
        assert!(terminal_mouse_report(modes, Motion { pressed: true }, modifiers, cell).is_none());
        assert!(terminal_mouse_report(modes, Press, modifiers, (223, 0)).is_none());
        modes.mouse = MouseTracking::Drag;
        modes.sgr_mouse = true;
        assert_eq!(
            terminal_mouse_report(modes, Motion { pressed: true }, modifiers, cell).as_deref(),
            Some(&b"\x1b[<32;2;3M"[..])
        );
        assert!(terminal_mouse_report(modes, Motion { pressed: false }, modifiers, cell).is_none());
    }

    #[test]
    fn search_clicks_use_the_rendered_control_regions() {
        let bar = Rect::new(20.0, 40.0, 340.0, 58.0);
        assert_eq!(search_click(bar, 10.0, 60.0), None);
        assert_eq!(search_click(bar, 180.0, 54.0), Some(UiAction::Ignore));
        assert_eq!(search_click(bar, 48.0, 84.0), Some(UiAction::SearchPrev));
        assert_eq!(search_click(bar, 150.0, 84.0), Some(UiAction::SearchNext));
        assert_eq!(
            search_click(bar, 228.0, 84.0),
            Some(UiAction::SearchToggleCase)
        );
    }

    #[test]
    fn a_modal_consumes_even_clicks_outside_its_controls() {
        let mut state = AppState::new();
        let click = |state: &mut AppState| handle_overlay_click(state, (0., 0.), 900., 600., 1.);
        state.close_modal.open = true;
        state.worktree_modal.open = true;
        assert!(matches!(
            click(&mut state),
            Some(OverlayClick::Action(UiAction::WorktreeModalCancel))
        ));
        state.worktree_modal.open = false;
        assert!(matches!(
            click(&mut state),
            Some(OverlayClick::Action(UiAction::CloseModalCancel))
        ));
        state.picker.open = true;
        assert!(matches!(click(&mut state), Some(OverlayClick::Redraw)));
        assert!(!state.picker.open);
        state.close_modal.open = false;
        assert!(click(&mut state).is_none());
    }

    #[test]
    fn a_second_click_selects_a_word_and_releases_the_drag_anchor() {
        use daedalus_proto::{Cell, GridRow, PaneStatus, PaneSummary, WorkspaceId};
        let id = PaneId::new();
        let role = PaneRole::Runner;
        let mut pane = PaneViewState::from_summary(PaneSummary {
            id,
            role,
            workspace_id: WorkspaceId::new(),
            status: PaneStatus::Idle,
            cols: 5,
            rows: 1,
            cwd: None,
            last_exit_code: None,
            has_failure_trace: false,
            launch_preset: None,
            agent_state: None,
        });
        pane.viewport_rows.push(GridRow {
            links: Vec::new(),
            wraps: false,
            row_index: 0,
            abs_row: 42,
            cells: "a cat"
                .chars()
                .map(|c| Cell {
                    text: c.to_string(),
                    ..Cell::default()
                })
                .collect(),
        });
        let mut pointer = PointerState::default();
        let first = Instant::now();
        pointer.select_at(&mut pane, 3, 0, first);
        assert_eq!(pointer.drag_anchor, Some((role, id, CellPos::new(3, 42))));
        let (_, _, anchor) = pointer.drag_anchor.unwrap();
        assert!(!extend_selection(&mut pane, anchor, 3, 0));
        assert!(extend_selection(&mut pane, anchor, 4, 0));
        assert_eq!(
            pane.selection,
            Some(Selection::new(anchor, CellPos::new(4, 42)))
        );
        pointer.end_drag();
        pointer.select_at(&mut pane, 3, 0, first + DOUBLE_CLICK_WINDOW);
        assert_eq!(pointer.drag_anchor, None);
        assert_eq!(
            pane.selection,
            Some(Selection::new(CellPos::new(2, 42), CellPos::new(4, 42)))
        );
        pointer.select_at(&mut pane, 3, 0, first + Duration::from_secs(2));
        assert!(pane.selection.is_none());
        assert!(pointer.drag_anchor.is_some());
    }

    #[test]
    fn worktree_buttons_share_rendered_targets_and_require_nonblank_input() {
        for (w, h, dpi) in [
            (1200.0, 800.0, 1.0),
            (192.0, 128.0, 1.0),
            (640.0, 224.0, 2.0),
        ] {
            let modal = worktree_modal_rect(w, h, dpi);
            let (cancel, create) = worktree_button_rects(modal, dpi);
            let field = daedalus_ui::layout::overlays::worktree_input_rect(modal, dpi);
            assert!(field.bottom() < cancel.y && cancel.right() < create.x);
            assert!(create.bottom() <= modal.bottom() && create.right() <= modal.right());
            let click = |rect: daedalus_ui::layout::Rect, state: &AppState| {
                worktree_modal_click(
                    modal,
                    dpi,
                    state.can_create_worktree(),
                    rect.x + rect.width * 0.5,
                    rect.y + rect.height * 0.5,
                )
            };
            let mut state = AppState::new();
            state.worktree_modal.push_text("topic");
            assert_eq!(click(create, &state), UiAction::Ignore);
            state.connection = ConnectionState::Connected;
            assert_eq!(click(create, &state), UiAction::WorktreeModalConfirm);
            assert_eq!(click(cancel, &state), UiAction::WorktreeModalCancel);
            assert_eq!(click(field, &state), UiAction::Ignore);
            state.worktree_modal.branch_input = "  ".into();
            assert_eq!(click(create, &state), UiAction::Ignore);
            assert_eq!(
                worktree_modal_click(modal, dpi, false, 0.0, 0.0),
                UiAction::WorktreeModalCancel
            );
        }
    }

    #[test]
    fn compact_close_modal_targets_match_their_keyboard_actions() {
        use winit::keyboard::{Key, NamedKey};
        let mut app_state = AppState::new();
        app_state.close_modal.open = true;
        app_state.close_modal.is_dirty = true;
        app_state.close_modal.is_worktree = true;
        app_state.close_modal.remove_worktree = true;
        let mut state = app_state.close_modal;
        for (width, height, scale) in [
            (320.0, 180.0, 1.0),
            (640.0, 360.0, 2.0),
            (180.0, 120.0, 1.0),
            (1000.0, 720.0, 1.0),
        ] {
            let modal = close_workspace_modal_rect(width, height, scale);
            assert!(modal.right() <= width && modal.bottom() <= height);
            let compose = close_modal_compose_rect(modal, &state, scale);
            let worktree = close_modal_worktree_rect(modal, &state, scale).unwrap();
            let force = close_modal_force_rect(modal, &state, scale).unwrap();
            let cancel = close_modal_cancel_rect(modal, scale);
            let confirm = close_modal_confirm_rect(modal, scale);
            assert!(compose.bottom() <= worktree.y && worktree.bottom() <= force.y);
            assert!(force.bottom() <= cancel.y && cancel.right() < confirm.x);
            for (target, key) in [
                (compose, Key::Character("c".into())),
                (worktree, Key::Character("w".into())),
                (force, Key::Character("f".into())),
                (cancel, Key::Named(NamedKey::Escape)),
                (confirm, Key::Named(NamedKey::Enter)),
            ] {
                assert!(
                    target.width > 0.0 && target.height > 0.0 && target.bottom() <= modal.bottom()
                );
                let keyboard = translate_key(
                    &key,
                    None,
                    ModifiersState::empty(),
                    false,
                    false,
                    true,
                    false,
                    false,
                );
                assert_ne!(keyboard, UiAction::Ignore);
                assert_eq!(
                    close_modal_click(
                        modal,
                        &state,
                        scale,
                        target.x + target.width / 2.0,
                        target.y + target.height / 2.0
                    ),
                    keyboard
                );
            }
            state.toggle_worktree();
            assert_eq!(
                close_modal_click(modal, &state, scale, force.x + 1.0, force.y + 1.0),
                UiAction::Ignore
            );
            state.toggle_worktree();
        }
    }

    #[test]
    fn link_click_modifier_matches_platform_convention() {
        let ctrl = ModifiersState::CONTROL;
        let cmd = ModifiersState::SUPER;

        // Linux uses Ctrl+Click
        assert!(is_link_click_modifier_for_platform(ctrl, false));
        assert!(!is_link_click_modifier_for_platform(cmd, false));
        assert!(!is_link_click_modifier_for_platform(
            ctrl | ModifiersState::ALT,
            false
        ));

        // macOS uses Cmd+Click
        assert!(is_link_click_modifier_for_platform(cmd, true));
        assert!(!is_link_click_modifier_for_platform(ctrl, true));
        assert!(!is_link_click_modifier_for_platform(
            cmd | ModifiersState::ALT,
            true
        ));

        let expected_current = if cfg!(target_os = "macos") {
            is_link_click_modifier(cmd) && !is_link_click_modifier(ctrl)
        } else {
            is_link_click_modifier(ctrl) && !is_link_click_modifier(cmd)
        };
        assert!(expected_current);
    }
}
