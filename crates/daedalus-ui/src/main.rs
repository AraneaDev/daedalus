use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser;
use daedalus_proto::{PaneId, PaneRole, default_socket_path, wrap_bracketed_paste};
use daedalus_ui::bridge::{BridgeConfig, DaemonBridge, UiCommand, UiEvent, UiEventSink};
use daedalus_ui::input::{UiAction, translate_key};
use daedalus_ui::layout::{HitTarget, LayoutInput, WorkspaceLayout, compute_layout, hit_test};
use daedalus_ui::renderer::{GpuRenderer, picker_item_rect, picker_modal_rect};
use daedalus_ui::selection::{CellPos, Selection, extract_selected_text, select_word_at};
use daedalus_ui::state::{AgentPreset, AppState};
use tracing::{error, warn};
use tracing_subscriber::EnvFilter;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

const INITIAL_WINDOW_WIDTH: f64 = 1440.0;
const INITIAL_WINDOW_HEIGHT: f64 = 900.0;
const RESIZE_DEBOUNCE: Duration = Duration::from_millis(32);
const TOAST_TTL: Duration = Duration::from_secs(5);
const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(400);
const WHEEL_SCROLL_LINES: isize = 3;
const FALLBACK_PANE_COLS: u16 = 120;
const FALLBACK_PANE_ROWS: u16 = 36;

#[derive(Debug, Parser)]
#[command(
    name = "daedalus-ui",
    about = "Daedalus native GPU terminal workbench"
)]
struct Args {
    /// Unix socket path for `daedalusd`.
    #[arg(long, env = "DAEDALUS_SOCKET")]
    socket: Option<PathBuf>,

    /// Initial workspace directory (defaults to current working directory).
    #[arg(long)]
    dir: Option<PathBuf>,

    /// Default Agent pane preset.
    #[arg(long, value_enum, default_value_t = AgentPreset::Shell)]
    agent: AgentPreset,

    /// Disable automatic `daedalusd` launch if the daemon is not running.
    #[arg(long)]
    no_auto_spawn: bool,
}

struct EventLoopProxySink(EventLoopProxy<UiEvent>);

impl UiEventSink for EventLoopProxySink {
    fn emit(&self, event: UiEvent) {
        let _ = self.0.send_event(event);
    }
}

struct DaedalusApp {
    config: BridgeConfig,
    proxy: EventLoopProxy<UiEvent>,
    window: Option<Arc<Window>>,
    renderer: Option<GpuRenderer>,
    bridge: Option<DaemonBridge>,
    state: AppState,
    modifiers: ModifiersState,
    cursor_pos: (f32, f32),
    drag_anchor: Option<(PaneRole, PaneId, CellPos)>,
    last_click: Option<(Instant, PaneRole, u16, u16)>,
    pending_resize_flush_at: Option<Instant>,
    clipboard: Option<arboard::Clipboard>,
}

impl DaedalusApp {
    fn new(config: BridgeConfig, proxy: EventLoopProxy<UiEvent>) -> Self {
        Self {
            config,
            proxy,
            window: None,
            renderer: None,
            bridge: None,
            state: AppState::new(),
            modifiers: ModifiersState::empty(),
            cursor_pos: (0.0, 0.0),
            drag_anchor: None,
            last_click: None,
            pending_resize_flush_at: None,
            clipboard: None,
        }
    }

    fn request_redraw(&self) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn send_command(&self, cmd: UiCommand) {
        if let Some(bridge) = &self.bridge {
            bridge.send(cmd);
        }
    }

    fn schedule_resize_flush(&mut self) {
        self.pending_resize_flush_at = Some(Instant::now() + RESIZE_DEBOUNCE);
    }

    fn has_top_banner(&self) -> bool {
        !self.state.connected || self.state.toast.is_some()
    }

    fn compute_current_layout(&self) -> Option<WorkspaceLayout> {
        let window = self.window.as_ref()?;
        let renderer = self.renderer.as_ref()?;
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return None;
        }
        let scale_factor = window.scale_factor() as f32;
        let (cell_w, cell_h) = renderer.cell_size();
        Some(compute_layout(&LayoutInput {
            window_w: size.width as f32,
            window_h: size.height as f32,
            scale_factor,
            cell_w,
            cell_h,
            sidebar_visible: self.state.sidebar_visible,
            has_top_banner: self.has_top_banner(),
            zoomed: self.state.zoomed,
            focused_role: self.state.focused_role,
            runner_has_failure: self.state.runner_has_failure(),
        }))
    }

    fn flush_due_resizes(&mut self, force: bool) {
        if !force {
            let Some(deadline) = self.pending_resize_flush_at else {
                return;
            };
            if Instant::now() < deadline {
                return;
            }
        }
        self.pending_resize_flush_at = None;

        let Some(layout) = self.compute_current_layout() else {
            return;
        };

        let mut pending = Vec::new();
        for pane_rect in &layout.panes {
            let Some(pane_id) = self.state.pane_id_for_role(pane_rect.role) else {
                continue;
            };
            let target = (pane_rect.cols.max(1), pane_rect.rows.max(1));
            if self.state.last_sent_sizes.get(&pane_id).copied() != Some(target) {
                self.state.last_sent_sizes.insert(pane_id, target);
                pending.push((pane_id, target.0, target.1));
            }
        }

        for (pane_id, cols, rows) in pending {
            self.send_command(UiCommand::ResizePane {
                pane_id,
                cols,
                rows,
            });
        }
    }

    fn expire_toast_if_due(&mut self, now: Instant) {
        let expired = self
            .state
            .toast
            .as_ref()
            .is_some_and(|(_, _, created_at)| now.duration_since(*created_at) >= TOAST_TTL);
        if expired {
            let had_banner = self.has_top_banner();
            self.state.toast = None;
            if had_banner != self.has_top_banner() {
                self.schedule_resize_flush();
            }
            self.request_redraw();
        }
    }

    fn show_toast(&mut self, message: impl Into<String>, is_error: bool) {
        let had_banner = self.has_top_banner();
        self.state.show_toast(message, is_error);
        if !had_banner {
            self.schedule_resize_flush();
        }
        self.request_redraw();
    }

    fn ensure_clipboard(&mut self) -> Result<&mut arboard::Clipboard, String> {
        if self.clipboard.is_none() {
            let cb = arboard::Clipboard::new()
                .map_err(|err| format!("Clipboard unavailable: {err}"))?;
            self.clipboard = Some(cb);
        }
        Ok(self.clipboard.as_mut().expect("clipboard initialized"))
    }

    fn copy_focused_selection(&mut self) {
        let Some(pane_id) = self.state.focused_pane_id() else {
            return;
        };
        let selected_text = self.state.panes.get(&pane_id).and_then(|pane| {
            let sel = pane.selection?;
            let text = extract_selected_text(pane.visible_rows(), &sel);
            (!text.is_empty()).then_some(text)
        });
        let Some(text) = selected_text else {
            return;
        };

        match self.ensure_clipboard().and_then(|cb| {
            cb.set_text(text)
                .map_err(|err| format!("Failed to copy to clipboard: {err}"))
        }) {
            Ok(()) => self.show_toast("Copied selection to clipboard", false),
            Err(err) => self.show_toast(err, true),
        }
    }

    fn paste_clipboard_to_focused(&mut self) {
        let Some(pane_id) = self.state.focused_pane_id() else {
            return;
        };
        let text = match self.ensure_clipboard().and_then(|cb| {
            cb.get_text()
                .map_err(|err| format!("Failed to read clipboard: {err}"))
        }) {
            Ok(text) if !text.is_empty() => text,
            Ok(_) => return,
            Err(err) => {
                self.show_toast(err, true);
                return;
            }
        };

        if let Some(pane) = self.state.panes.get_mut(&pane_id) {
            pane.reset_scroll();
            pane.selection = None;
        }
        let data = wrap_bracketed_paste(text.as_bytes());
        self.send_command(UiCommand::SendInput { pane_id, data });
        self.request_redraw();
    }

    fn create_new_workspace(&mut self) {
        let root_dir = self.config.initial_dir.clone();
        let name = workspace_name_from_dir(&root_dir);
        self.send_command(UiCommand::CreateWorkspace {
            name,
            root_dir,
            agent_preset: self.config.default_agent,
        });
    }

    fn open_agent_picker(&mut self) {
        let current = self
            .state
            .pane_for_role(PaneRole::Agent)
            .and_then(|p| p.agent_preset)
            .unwrap_or(self.config.default_agent);
        self.state.picker.open_with(current);
        self.request_redraw();
    }

    fn apply_agent_preset(&mut self, preset: AgentPreset) {
        self.state.picker.close();
        let Some(workspace_id) = self.state.active_workspace_id else {
            self.show_toast("No active workspace for Agent preset switch", true);
            return;
        };
        let old_pane_id = self.state.pane_id_for_role(PaneRole::Agent);
        let (cols, rows) = self
            .compute_current_layout()
            .and_then(|layout| layout.pane(PaneRole::Agent).map(|p| (p.cols, p.rows)))
            .unwrap_or((FALLBACK_PANE_COLS, FALLBACK_PANE_ROWS));

        self.send_command(UiCommand::ReplaceAgentPreset {
            workspace_id,
            old_pane_id,
            preset,
            cols,
            rows,
        });
        self.state.focused_role = PaneRole::Agent;
        self.request_redraw();
    }

    fn forward_runner_failure_to_agent(&mut self) {
        let Some(runner_pane_id) = self.state.pane_id_for_role(PaneRole::Runner) else {
            self.show_toast("Runner pane is not available", true);
            return;
        };
        let Some(agent_pane_id) = self.state.pane_id_for_role(PaneRole::Agent) else {
            self.show_toast("Agent pane is not available", true);
            return;
        };

        self.send_command(UiCommand::ForwardRunnerFailureToAgent {
            runner_pane_id,
            agent_pane_id,
        });
        self.state.focused_role = PaneRole::Agent;
        if self.state.zoomed {
            self.schedule_resize_flush();
        }
        self.request_redraw();
    }

    fn scroll_pane(&mut self, role: PaneRole, delta: isize) {
        if delta == 0 {
            return;
        }
        let Some(pane_id) = self.state.pane_id_for_role(role) else {
            return;
        };
        let request = {
            let Some(pane) = self.state.panes.get_mut(&pane_id) else {
                return;
            };
            let count = (pane.rows as usize).max(1);
            if delta > 0 {
                // Allow probing beyond cached `scrollback_len` because `GridDelta`
                // does not carry updated scrollback lengths; `apply_scrollback`
                // clamps to the daemon's authoritative `total_scrollback`.
                let next = pane.scroll_offset.saturating_add(delta as usize);
                pane.scroll_offset = next;
                Some((next, count))
            } else {
                let prev = pane.scroll_offset;
                let next = pane.adjust_scroll_offset(delta);
                if next != prev && next > 0 {
                    Some((next, count))
                } else {
                    None
                }
            }
        };

        if let Some((offset_from_bottom, count)) = request {
            self.send_command(UiCommand::RequestScrollback {
                pane_id,
                offset_from_bottom,
                count,
            });
        }
        self.request_redraw();
    }

    fn handle_ui_action(&mut self, action: UiAction) {
        match action {
            UiAction::FocusRole(role) => {
                self.state.focused_role = role;
                if self.state.zoomed {
                    self.schedule_resize_flush();
                }
                self.request_redraw();
            }
            UiAction::FocusDir(dir) => {
                self.state.focus_direction(dir);
                if self.state.zoomed {
                    self.schedule_resize_flush();
                }
                self.request_redraw();
            }
            UiAction::ToggleZoom => {
                self.state.zoomed = !self.state.zoomed;
                self.schedule_resize_flush();
                self.request_redraw();
            }
            UiAction::ToggleSidebar => {
                self.state.sidebar_visible = !self.state.sidebar_visible;
                self.schedule_resize_flush();
                self.request_redraw();
            }
            UiAction::ToggleAgentPicker => {
                if self.state.picker.open {
                    self.state.picker.close();
                    self.request_redraw();
                } else {
                    self.open_agent_picker();
                }
            }
            UiAction::NewWorkspace => self.create_new_workspace(),
            UiAction::CopySelection => self.copy_focused_selection(),
            UiAction::PasteClipboard => self.paste_clipboard_to_focused(),
            UiAction::ForwardFailureToAgent => self.forward_runner_failure_to_agent(),
            UiAction::ScrollPageUp => {
                let page = self
                    .state
                    .pane_for_role(self.state.focused_role)
                    .map_or(20, |p| (p.rows as isize).saturating_sub(1).max(1));
                self.scroll_pane(self.state.focused_role, page);
            }
            UiAction::ScrollPageDown => {
                let page = self
                    .state
                    .pane_for_role(self.state.focused_role)
                    .map_or(20, |p| (p.rows as isize).saturating_sub(1).max(1));
                self.scroll_pane(self.state.focused_role, -page);
            }
            UiAction::ScrollToBottom => {
                if let Some(pane) = self.state.pane_for_role_mut(self.state.focused_role) {
                    pane.reset_scroll();
                }
                self.request_redraw();
            }
            UiAction::SendBytes(data) => {
                if let Some(pane_id) = self.state.focused_pane_id() {
                    if let Some(pane) = self.state.panes.get_mut(&pane_id) {
                        pane.reset_scroll();
                        pane.selection = None;
                    }
                    self.send_command(UiCommand::SendInput { pane_id, data });
                    self.request_redraw();
                }
            }
            UiAction::PickerUp => {
                self.state.picker.move_up();
                self.request_redraw();
            }
            UiAction::PickerDown => {
                self.state.picker.move_down();
                self.request_redraw();
            }
            UiAction::PickerConfirm => {
                let preset = self.state.picker.selected_preset();
                self.apply_agent_preset(preset);
            }
            UiAction::PickerCancel => {
                self.state.picker.close();
                self.request_redraw();
            }
            UiAction::Ignore => {}
        }
    }

    fn handle_left_mouse_down(&mut self) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        let Some(renderer) = self.renderer.as_ref() else {
            return;
        };
        let size = window.inner_size();
        let scale_factor = window.scale_factor() as f32;
        let (cell_w, cell_h) = renderer.cell_size();
        let (x, y) = self.cursor_pos;

        if self.state.picker.open {
            let modal = picker_modal_rect(size.width as f32, size.height as f32, scale_factor);
            if modal.contains(x, y) {
                for (idx, preset) in AgentPreset::ALL.iter().enumerate() {
                    let item = picker_item_rect(modal, idx, scale_factor);
                    if item.contains(x, y) {
                        self.state.picker.selected_index = idx;
                        self.apply_agent_preset(*preset);
                        return;
                    }
                }
            } else {
                self.state.picker.close();
                self.request_redraw();
            }
            return;
        }

        let Some(layout) = self.compute_current_layout() else {
            return;
        };
        let target = hit_test(
            &layout,
            self.state.workspaces.len(),
            scale_factor,
            cell_w,
            cell_h,
            x,
            y,
        );

        match target {
            HitTarget::SidebarWorkspace(idx) => {
                if let Some(ws) = self.state.workspaces.get(idx) {
                    let ws_id = ws.id;
                    if self.state.active_workspace_id != Some(ws_id) {
                        self.state.active_workspace_id = Some(ws_id);
                        self.schedule_resize_flush();
                        self.request_redraw();
                    }
                }
            }
            HitTarget::SidebarNewWorkspace => self.create_new_workspace(),
            HitTarget::PaneHeaderAction(PaneRole::Agent) => {
                self.state.focused_role = PaneRole::Agent;
                self.open_agent_picker();
            }
            HitTarget::PaneHeaderAction(PaneRole::Runner) => {
                self.forward_runner_failure_to_agent();
            }
            HitTarget::PaneHeaderAction(role) | HitTarget::PaneHeader(role) => {
                self.state.focused_role = role;
                self.request_redraw();
            }
            HitTarget::PaneViewport { role, col, row } => {
                self.state.focused_role = role;
                let now = Instant::now();
                let is_double_click = self
                    .last_click
                    .is_some_and(|(ts, prev_role, prev_col, prev_row)| {
                        now.duration_since(ts) <= DOUBLE_CLICK_WINDOW
                            && prev_role == role
                            && prev_col == col
                            && prev_row == row
                    });

                if let Some(pane_id) = self.state.pane_id_for_role(role) {
                    if is_double_click {
                        self.last_click = None;
                        self.drag_anchor = None;
                        if let Some(pane) = self.state.panes.get_mut(&pane_id) {
                            pane.selection = pane
                                .visible_rows()
                                .get(row as usize)
                                .and_then(|grid_row| select_word_at(grid_row, col));
                        }
                    } else {
                        self.last_click = Some((now, role, col, row));
                        if let Some(pane) = self.state.panes.get_mut(&pane_id) {
                            pane.selection = None;
                            self.drag_anchor = pane
                                .visible_rows()
                                .get(row as usize)
                                .map(|grid_row| (role, pane_id, CellPos::new(col, grid_row.abs_row)));
                        }
                    }
                }
                self.request_redraw();
            }
            HitTarget::None => {}
        }
    }

    fn handle_cursor_moved(&mut self, x: f32, y: f32) {
        self.cursor_pos = (x, y);

        let Some((drag_role, drag_pane_id, anchor)) = self.drag_anchor else {
            return;
        };
        let Some(renderer) = self.renderer.as_ref() else {
            return;
        };
        let (cell_w, cell_h) = renderer.cell_size();
        let Some(layout) = self.compute_current_layout() else {
            return;
        };
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
            && let Some(grid_row) = pane.visible_rows().get(row as usize)
        {
            let head = CellPos::new(col, grid_row.abs_row);
            if head != anchor || pane.selection.is_some() {
                pane.selection = Some(Selection::new(anchor, head));
                self.request_redraw();
            }
        }
    }

    fn handle_mouse_wheel(&mut self, delta: MouseScrollDelta) {
        if self.state.picker.open {
            return;
        }
        let scroll_lines = match delta {
            MouseScrollDelta::LineDelta(_, y) => {
                if y > 0.0 {
                    WHEEL_SCROLL_LINES * (y.ceil() as isize).max(1)
                } else if y < 0.0 {
                    -WHEEL_SCROLL_LINES * ((-y).ceil() as isize).max(1)
                } else {
                    0
                }
            }
            MouseScrollDelta::PixelDelta(pos) => {
                if pos.y > 0.0 {
                    WHEEL_SCROLL_LINES
                } else if pos.y < 0.0 {
                    -WHEEL_SCROLL_LINES
                } else {
                    0
                }
            }
        };
        if scroll_lines == 0 {
            return;
        }

        let Some(window) = self.window.as_ref() else {
            return;
        };
        let Some(renderer) = self.renderer.as_ref() else {
            return;
        };
        let scale_factor = window.scale_factor() as f32;
        let (cell_w, cell_h) = renderer.cell_size();
        let Some(layout) = self.compute_current_layout() else {
            return;
        };

        let target_role = match hit_test(
            &layout,
            self.state.workspaces.len(),
            scale_factor,
            cell_w,
            cell_h,
            self.cursor_pos.0,
            self.cursor_pos.1,
        ) {
            HitTarget::PaneViewport { role, .. }
            | HitTarget::PaneHeader(role)
            | HitTarget::PaneHeaderAction(role) => Some(role),
            _ => None,
        };

        if let Some(role) = target_role {
            self.scroll_pane(role, scroll_lines);
        }
    }
}

impl ApplicationHandler<UiEvent> for DaedalusApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let attrs = Window::default_attributes()
            .with_title("Daedalus Workbench")
            .with_inner_size(LogicalSize::new(INITIAL_WINDOW_WIDTH, INITIAL_WINDOW_HEIGHT));

        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(err) => {
                error!(error = %err, "failed to create window");
                event_loop.exit();
                return;
            }
        };

        let renderer = match GpuRenderer::new(Arc::clone(&window)) {
            Ok(r) => r,
            Err(err) => {
                error!(error = %err, "failed to initialize GPU renderer");
                event_loop.exit();
                return;
            }
        };

        let sink = Arc::new(EventLoopProxySink(self.proxy.clone()));
        let bridge = DaemonBridge::start(self.config.clone(), sink);

        window.request_redraw();
        self.window = Some(window);
        self.renderer = Some(renderer);
        self.bridge = Some(bridge);
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UiEvent) {
        match event {
            UiEvent::Connected => {
                self.state.connected = true;
                self.schedule_resize_flush();
            }
            UiEvent::Disconnected => {
                self.state.connected = false;
                self.state.last_sent_sizes.clear();
                self.schedule_resize_flush();
            }
            UiEvent::WorkspacesLoaded(workspaces) => {
                self.state.set_workspaces(workspaces);
                self.schedule_resize_flush();
            }
            UiEvent::WorkspaceCreated(ws) => {
                let ws_id = ws.id;
                self.state.upsert_workspace(ws);
                self.state.active_workspace_id = Some(ws_id);
                self.schedule_resize_flush();
            }
            UiEvent::PaneSpawned {
                summary,
                agent_preset,
            } => {
                self.state.register_pane(summary, agent_preset);
                self.schedule_resize_flush();
            }
            UiEvent::PaneClosed { pane_id } => {
                self.state.panes.remove(&pane_id);
                self.state.last_sent_sizes.remove(&pane_id);
                for ws in &mut self.state.workspaces {
                    ws.panes.retain(|p| p.id != pane_id);
                }
            }
            UiEvent::Snapshot(snapshot) => {
                if let Some(pane) = self.state.panes.get_mut(&snapshot.pane_id) {
                    pane.apply_snapshot(snapshot);
                }
            }
            UiEvent::ScrollbackLoaded {
                pane_id,
                total_scrollback,
                rows,
            } => {
                if let Some(pane) = self.state.panes.get_mut(&pane_id) {
                    pane.apply_scrollback(total_scrollback, rows);
                }
            }
            UiEvent::ServerEvent(server_event) => {
                if let Some(resync_pane_id) = self.state.apply_server_event(server_event) {
                    self.send_command(UiCommand::RequestSnapshot {
                        pane_id: resync_pane_id,
                    });
                }
            }
            UiEvent::Toast { message, is_error } => {
                let had_banner = self.has_top_banner();
                self.state.show_toast(message, is_error);
                if !had_banner {
                    self.schedule_resize_flush();
                }
            }
        }

        self.flush_due_resizes(false);
        self.request_redraw();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                // Shut down only the client bridge; leave `daedalusd` and running PTYs alive.
                if let Some(bridge) = self.bridge.take() {
                    bridge.shutdown();
                }
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                if size.width > 0 && size.height > 0 {
                    if let (Some(window), Some(renderer)) =
                        (self.window.as_ref(), self.renderer.as_mut())
                    {
                        renderer.resize(size.width, size.height, window.scale_factor() as f32);
                    }
                    self.schedule_resize_flush();
                    self.request_redraw();
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                let mut resized = false;
                if let (Some(window), Some(renderer)) =
                    (self.window.as_ref(), self.renderer.as_mut())
                {
                    let size = window.inner_size();
                    if size.width > 0 && size.height > 0 {
                        renderer.resize(size.width, size.height, scale_factor as f32);
                        resized = true;
                    }
                }
                if resized {
                    self.schedule_resize_flush();
                    self.request_redraw();
                }
            }
            WindowEvent::ModifiersChanged(new_mods) => {
                self.modifiers = new_mods.state();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed {
                    let action = translate_key(
                        &event.logical_key,
                        event.text.as_deref(),
                        self.modifiers,
                        self.state.picker.open,
                    );
                    self.handle_ui_action(action);
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.handle_cursor_moved(position.x as f32, position.y as f32);
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => match state {
                ElementState::Pressed => self.handle_left_mouse_down(),
                ElementState::Released => {
                    self.drag_anchor = None;
                }
            },
            WindowEvent::MouseWheel { delta, .. } => {
                self.handle_mouse_wheel(delta);
            }
            WindowEvent::RedrawRequested => {
                self.expire_toast_if_due(Instant::now());
                self.flush_due_resizes(false);

                let Some(layout) = self.compute_current_layout() else {
                    return;
                };
                let Some(renderer) = self.renderer.as_mut() else {
                    return;
                };

                match renderer.render(&mut self.state, &layout) {
                    Ok(()) => {}
                    Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                        if let Some(window) = self.window.as_ref() {
                            let size = window.inner_size();
                            if size.width > 0 && size.height > 0 {
                                renderer.resize(
                                    size.width,
                                    size.height,
                                    window.scale_factor() as f32,
                                );
                                window.request_redraw();
                            }
                        }
                    }
                    Err(wgpu::SurfaceError::Timeout) => {
                        warn!("wgpu surface frame timed out");
                    }
                    Err(wgpu::SurfaceError::OutOfMemory) => {
                        error!("wgpu surface out of memory; exiting");
                        if let Some(bridge) = self.bridge.take() {
                            bridge.shutdown();
                        }
                        event_loop.exit();
                    }
                    Err(wgpu::SurfaceError::Other) => {
                        warn!("wgpu surface error: other");
                    }
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        self.expire_toast_if_due(now);
        self.flush_due_resizes(false);

        let toast_deadline = self
            .state
            .toast
            .as_ref()
            .map(|(_, _, created_at)| *created_at + TOAST_TTL);

        let next_deadline = match (self.pending_resize_flush_at, toast_deadline) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };

        if let Some(deadline) = next_deadline {
            event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }
}

fn workspace_name_from_dir(dir: &Path) -> String {
    dir.file_name()
        .and_then(|name| name.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("workspace")
        .to_string()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    let socket_path = args.socket.unwrap_or_else(default_socket_path);
    let initial_dir = args
        .dir
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let config = BridgeConfig {
        socket_path,
        initial_dir,
        default_agent: args.agent,
        auto_spawn_daemon: !args.no_auto_spawn,
    };

    let event_loop: EventLoop<UiEvent> = EventLoop::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let mut app = DaedalusApp::new(config, proxy);
    event_loop.run_app(&mut app)?;
    Ok(())
}
