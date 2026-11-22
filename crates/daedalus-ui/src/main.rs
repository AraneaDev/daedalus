use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser;
use daedalus_proto::{PaneId, PaneRole, WorkspaceId, default_socket_path, wrap_bracketed_paste};
use daedalus_ui::bridge::{BridgeConfig, DaemonBridge, UiCommand, UiEvent, UiEventSink};
use daedalus_ui::config::UiConfig;
use daedalus_ui::input::{UiAction, filter_modal_text, translate_key};
use daedalus_ui::layout::{HitTarget, LayoutInput, WorkspaceLayout, compute_layout, hit_test};
use daedalus_ui::links::{detect_link_in_row, detect_url_in_row, open_link};
use daedalus_ui::renderer::{
    GpuRenderer, RenderError, action_palette_layout, close_modal_cancel_rect,
    close_modal_compose_rect, close_modal_confirm_rect, close_modal_force_rect,
    close_modal_worktree_rect, close_workspace_modal_rect, picker_item_rect, picker_modal_rect,
    search_bar_rect, worktree_modal_rect,
};
use daedalus_ui::selection::{CellPos, Selection, extract_selected_text, select_word_at};
use daedalus_ui::state::{AgentPreset, AppState};
use daedalus_ui::theme::Theme;
use tracing::{error, warn};
use tracing_subscriber::EnvFilter;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent};
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
#[command(name = "daedalus-ui", about = "Daedalus native GPU terminal workbench")]
struct Args {
    /// Local daemon socket path or ssh:// URI.
    #[arg(long, env = "DAEDALUS_SOCKET")]
    socket: Option<PathBuf>,

    /// Bearer token for an authenticated daemon.
    #[arg(long, env = "DAEDALUS_AUTH_TOKEN", hide_env_values = true)]
    auth_token: Option<String>,

    /// Initial workspace directory on the daemon host (defaults to its working directory for SSH).
    #[arg(long)]
    dir: Option<PathBuf>,

    /// Default Agent pane preset (overrides `config.toml` `default_agent` when set).
    #[arg(long, value_enum)]
    agent: Option<AgentPreset>,

    /// Optional path to UI `config.toml` (defaults to `$DAEDALUS_CONFIG` or `~/.config/daedalus/config.toml`).
    #[arg(long, env = "DAEDALUS_CONFIG")]
    config: Option<PathBuf>,

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
    ui_config: UiConfig,
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
    fn new(config: BridgeConfig, ui_config: UiConfig, proxy: EventLoopProxy<UiEvent>) -> Self {
        Self {
            config,
            ui_config,
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
            let cb =
                arboard::Clipboard::new().map_err(|err| format!("Clipboard unavailable: {err}"))?;
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
        self.state.open_picker_with(current);
        self.request_redraw();
    }

    fn open_worktree_modal(&mut self) {
        self.state.open_worktree_modal();
        self.request_redraw();
    }

    fn open_close_modal_for_workspace(&mut self, workspace_id: WorkspaceId) {
        if self.state.open_close_modal_for_workspace(workspace_id) {
            self.request_redraw();
        }
    }

    fn confirm_close_workspace(&mut self) {
        let Some(id) = self.state.close_modal.workspace_id else {
            self.state.close_modal.close();
            self.request_redraw();
            return;
        };
        let compose_down = self.state.close_modal.compose_down;
        let remove_worktree = self.state.close_modal.remove_worktree;
        let force_remove_worktree = self.state.close_modal.force_remove_worktree;
        self.state.close_modal.close();
        self.send_command(UiCommand::CloseWorkspace {
            id,
            compose_down,
            remove_worktree,
            force_remove_worktree,
        });
        self.request_redraw();
    }

    fn switch_workspace_by(&mut self, delta: isize) {
        let prev_id = self.state.active_workspace_id;
        if let Some(next_id) = self.state.cycle_workspace(delta) {
            self.ensure_active_workspace_panes();
            if Some(next_id) != prev_id {
                self.schedule_resize_flush();
            }
            self.request_redraw();
        }
    }

    fn submit_worktree_modal(&mut self) {
        let branch = self.state.worktree_modal.branch_input.trim().to_string();
        if branch.is_empty() {
            self.show_toast("Branch name cannot be empty", true);
            return;
        }
        self.state.worktree_modal.close();
        let repo_dir = self
            .state
            .active_workspace()
            .map(|ws| ws.root_dir.clone())
            .unwrap_or_else(|| self.config.initial_dir.clone());
        self.send_command(UiCommand::CreateWorktreeWorkspace {
            repo_dir,
            branch,
            default_agent: self.config.default_agent,
        });
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

    fn ensure_active_workspace_panes(&mut self) {
        let Some(ws) = self.state.active_dormant_workspace() else {
            return;
        };
        let workspace_id = ws.id;
        let manifest = ws.manifest.clone();
        if self.state.mark_workspace_spawning(workspace_id) {
            self.send_command(UiCommand::EnsureWorkspacePanes {
                workspace_id,
                default_agent: self.config.default_agent,
                manifest,
            });
        }
    }

    fn run_pane_search(&self) {
        let Some(ref search) = self.state.search else {
            return;
        };
        if search.query.is_empty() {
            return;
        }
        self.send_command(UiCommand::SearchPane {
            pane_id: search.pane_id,
            query: search.query.clone(),
            case_sensitive: search.case_sensitive,
            generation: search.generation,
        });
    }

    fn scroll_to_active_search_match(&mut self) {
        let Some(ref search) = self.state.search else {
            return;
        };
        let Some(active) = search.active_match() else {
            return;
        };
        let pane_id = search.pane_id;
        let request = {
            let Some(pane) = self.state.panes.get_mut(&pane_id) else {
                return;
            };
            pane.scrollback_len = search.total_scrollback;
            if pane
                .visible_rows()
                .iter()
                .any(|r| r.abs_row == active.abs_row)
            {
                None
            } else {
                let viewport_rows = (pane.rows as usize).max(1);
                let live_start = pane
                    .viewport_rows
                    .last()
                    .and_then(|r| (r.abs_row + 1).checked_sub(viewport_rows as u64))
                    .max(Some(search.total_scrollback as u64));
                let target_offset = search
                    .target_scroll_offset_with_live_start(viewport_rows, live_start)
                    .unwrap_or(0);
                if target_offset == 0 {
                    pane.reset_scroll();
                    None
                } else {
                    pane.scroll_offset = target_offset;
                    Some((target_offset, viewport_rows))
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
    }

    fn apply_font_size_delta(&mut self, delta: f32) {
        let current = self
            .renderer
            .as_ref()
            .map_or(self.ui_config.font_size, GpuRenderer::font_size);
        self.set_font_size(current + delta);
    }

    fn set_font_size(&mut self, size: f32) {
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        let prev = renderer.font_size();
        let metrics_changed = renderer.set_font_size(size);
        let next = renderer.font_size();
        if (next - prev).abs() > 1e-4 {
            for pane in self.state.panes.values_mut() {
                pane.mark_all_visible_dirty();
            }
            if metrics_changed {
                self.schedule_resize_flush();
            }
            self.request_redraw();
        }
    }

    fn open_clicked_link(&mut self, role: PaneRole, col: u16, row: u16) {
        let link = {
            let Some(pane) = self.state.pane_for_role(role) else {
                return;
            };
            let Some(grid_row) = pane.visible_rows().get(row as usize) else {
                return;
            };
            let cwd = pane.summary.cwd.as_deref();
            let root_dir = self
                .state
                .active_workspace()
                .map(|ws| ws.root_dir.as_path());
            if self.state.remote_host.is_some() {
                detect_url_in_row(grid_row, col as usize)
            } else {
                detect_link_in_row(grid_row, col as usize, cwd, root_dir)
            }
        };

        let Some(link) = link else {
            return;
        };

        if let Err(err) = open_link(&link, self.ui_config.editor.as_deref()) {
            warn!(error = %err, link = ?link, "failed to open clicked link");
            self.show_toast(format!("Failed to open link: {err}"), true);
        }
    }

    fn confirm_workspace_action(&mut self) {
        let selected = self.state.action_palette.selected_index;
        if let (Some(workspace_id), Some(action)) = (
            self.state.active_workspace_id,
            self.state.active_actions().get(selected),
        ) {
            self.send_command(UiCommand::RunWorkspaceAction {
                workspace_id,
                action_id: action.id.clone(),
            });
        }
        self.state.action_palette.close();
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
            UiAction::ToggleActionPalette => {
                if self.state.action_palette.open {
                    self.state.action_palette.close();
                } else {
                    self.drag_anchor = None;
                    self.state.open_action_palette();
                }
                self.request_redraw();
            }
            UiAction::ActionPaletteUp | UiAction::ActionPaletteDown => {
                let count = self.state.active_actions().len();
                if action == UiAction::ActionPaletteUp {
                    self.state.action_palette.move_up(count);
                } else {
                    self.state.action_palette.move_down(count);
                }
                self.request_redraw();
            }
            UiAction::ActionPaletteConfirm => self.confirm_workspace_action(),
            UiAction::ActionPaletteCancel => {
                self.state.action_palette.close();
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
            UiAction::ToggleWorktreeModal => {
                self.state.toggle_worktree_modal();
                self.request_redraw();
            }
            UiAction::OpenCloseWorkspaceModal => {
                if let Some(ws_id) = self.state.active_workspace_id {
                    self.open_close_modal_for_workspace(ws_id);
                } else {
                    self.show_toast("No active workspace to close", true);
                }
            }
            UiAction::ToggleSearch => {
                if let Some(pane_id) = self.state.focused_pane_id() {
                    self.state.toggle_search(pane_id);
                    self.request_redraw();
                }
            }
            UiAction::NewWorkspace => self.create_new_workspace(),
            UiAction::SwitchWorkspaceNext => self.switch_workspace_by(1),
            UiAction::SwitchWorkspacePrev => self.switch_workspace_by(-1),
            UiAction::CopySelection => self.copy_focused_selection(),
            UiAction::PasteClipboard => self.paste_clipboard_to_focused(),
            UiAction::ForwardFailureToAgent => self.forward_runner_failure_to_agent(),
            UiAction::FontZoomIn => self.apply_font_size_delta(1.0),
            UiAction::FontZoomOut => self.apply_font_size_delta(-1.0),
            UiAction::FontZoomReset => self.set_font_size(self.ui_config.font_size),
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
            UiAction::WorktreeModalText(text) => {
                self.state.worktree_modal.push_text(&text);
                self.request_redraw();
            }
            UiAction::WorktreeModalBackspace => {
                self.state.worktree_modal.pop_char();
                self.request_redraw();
            }
            UiAction::WorktreeModalConfirm => self.submit_worktree_modal(),
            UiAction::WorktreeModalCancel => {
                self.state.worktree_modal.close();
                self.request_redraw();
            }
            UiAction::CloseModalToggleCompose => {
                self.state.close_modal.toggle_compose();
                self.request_redraw();
            }
            UiAction::CloseModalToggleWorktree => {
                self.state.close_modal.toggle_worktree();
                self.request_redraw();
            }
            UiAction::CloseModalToggleForce => {
                self.state.close_modal.toggle_force();
                self.request_redraw();
            }
            UiAction::CloseModalConfirm => self.confirm_close_workspace(),
            UiAction::CloseModalCancel => {
                self.state.close_modal.close();
                self.request_redraw();
            }
            UiAction::SearchText(text) => {
                if let Some(ref mut search) = self.state.search {
                    search.push_text(&text);
                    self.run_pane_search();
                    self.request_redraw();
                }
            }
            UiAction::SearchBackspace => {
                if let Some(ref mut search) = self.state.search
                    && search.pop_char()
                {
                    self.run_pane_search();
                    self.request_redraw();
                }
            }
            UiAction::SearchNext => {
                if let Some(ref mut search) = self.state.search {
                    search.step_next();
                }
                self.scroll_to_active_search_match();
                self.request_redraw();
            }
            UiAction::SearchPrev => {
                if let Some(ref mut search) = self.state.search {
                    search.step_prev();
                }
                self.scroll_to_active_search_match();
                self.request_redraw();
            }
            UiAction::SearchCancel => {
                self.state.close_search();
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

        if self.state.action_palette.open {
            let (modal, visible) = action_palette_layout(
                size.width as f32,
                size.height as f32,
                scale_factor,
                self.state.active_actions().len(),
                self.state.action_palette.selected_index,
            );
            if !modal.contains(x, y) {
                self.state.action_palette.close();
                self.request_redraw();
            } else if let Some((_, index)) = visible
                .enumerate()
                .find(|(row, _)| picker_item_rect(modal, *row, scale_factor).contains(x, y))
            {
                self.state.action_palette.selected_index = index;
                self.confirm_workspace_action();
            }
            return;
        }

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

        if self.state.worktree_modal.open {
            let modal = worktree_modal_rect(size.width as f32, size.height as f32, scale_factor);
            if !modal.contains(x, y) {
                self.state.worktree_modal.close();
                self.request_redraw();
            }
            return;
        }

        if self.state.close_modal.open {
            let modal =
                close_workspace_modal_rect(size.width as f32, size.height as f32, scale_factor);
            if !modal.contains(x, y) {
                self.state.close_modal.close();
                self.request_redraw();
                return;
            }
            if close_modal_compose_rect(modal, &self.state.close_modal, scale_factor).contains(x, y)
            {
                self.state.close_modal.toggle_compose();
                self.request_redraw();
                return;
            }
            if close_modal_worktree_rect(modal, &self.state.close_modal, scale_factor)
                .is_some_and(|r| r.contains(x, y))
            {
                self.state.close_modal.toggle_worktree();
                self.request_redraw();
                return;
            }
            if close_modal_force_rect(modal, &self.state.close_modal, scale_factor)
                .is_some_and(|r| r.contains(x, y))
            {
                self.state.close_modal.toggle_force();
                self.request_redraw();
                return;
            }
            if close_modal_cancel_rect(modal, scale_factor).contains(x, y) {
                self.state.close_modal.close();
                self.request_redraw();
                return;
            }
            if close_modal_confirm_rect(modal, scale_factor).contains(x, y) {
                self.confirm_close_workspace();
                return;
            }
            return;
        }

        let Some(layout) = self.compute_current_layout() else {
            return;
        };
        if let Some(search) = self.state.search.as_ref()
            && let Some(role) = self
                .state
                .active_workspace()
                .and_then(|ws| ws.panes.iter().find(|p| p.id == search.pane_id))
                .map(|p| p.role)
            && let Some(pane_rect) = layout.pane(role)
            && search_bar_rect(pane_rect, scale_factor).contains(x, y)
        {
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

        match target {
            HitTarget::SidebarWorkspace(idx) => {
                if let Some(ws) = self.state.workspaces.get(idx) {
                    let ws_id = ws.id;
                    if self.state.active_workspace_id != Some(ws_id) {
                        self.state.active_workspace_id = Some(ws_id);
                        self.state.close_search();
                        self.ensure_active_workspace_panes();
                        self.schedule_resize_flush();
                        self.request_redraw();
                    }
                }
            }
            HitTarget::SidebarWorkspaceClose(idx) => {
                if let Some(ws_id) = self.state.workspaces.get(idx).map(|ws| ws.id) {
                    self.open_close_modal_for_workspace(ws_id);
                }
            }
            HitTarget::SidebarNewWorkspace => self.create_new_workspace(),
            HitTarget::SidebarNewWorktree => self.open_worktree_modal(),
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
                if is_link_click_modifier(self.modifiers) {
                    self.open_clicked_link(role, col, row);
                    self.request_redraw();
                    return;
                }
                let now = Instant::now();
                let is_double_click =
                    self.last_click
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
                            self.drag_anchor =
                                pane.visible_rows().get(row as usize).map(|grid_row| {
                                    (role, pane_id, CellPos::new(col, grid_row.abs_row))
                                });
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
        if self.state.picker.open || self.state.worktree_modal.open || self.state.close_modal.open {
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

        if self.state.action_palette.open {
            self.handle_ui_action(if scroll_lines > 0 {
                UiAction::ActionPaletteUp
            } else {
                UiAction::ActionPaletteDown
            });
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
            .with_inner_size(LogicalSize::new(
                INITIAL_WINDOW_WIDTH,
                INITIAL_WINDOW_HEIGHT,
            ));

        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(err) => {
                error!(error = %err, "failed to create window");
                event_loop.exit();
                return;
            }
        };
        window.set_ime_allowed(true);

        let theme = Theme::from_preset(self.ui_config.theme);
        let renderer = match GpuRenderer::new(
            Arc::clone(&window),
            theme,
            self.ui_config.font_size,
            self.ui_config.font_family.as_deref(),
        ) {
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
                self.state.spawning_workspaces.clear();
                self.schedule_resize_flush();
            }
            UiEvent::WorkspaceCloseFailed(workspaces) => {
                self.state.set_workspaces(workspaces);
                self.schedule_resize_flush();
            }
            UiEvent::WorkspacesLoaded(workspaces) => {
                self.state.set_workspaces(workspaces);
                self.ensure_active_workspace_panes();
                self.schedule_resize_flush();
            }
            UiEvent::WorkspaceCreated(ws) => {
                let ws_id = ws.id;
                self.state.upsert_workspace(ws);
                self.state.active_workspace_id = Some(ws_id);
                self.state.close_search();
                self.schedule_resize_flush();
            }
            UiEvent::WorkspacePanesEnsured { workspace_id } => {
                self.state.clear_workspace_spawning(workspace_id);
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
                if self
                    .state
                    .search
                    .as_ref()
                    .is_some_and(|search| search.pane_id == pane_id)
                {
                    self.state.close_search();
                }
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
            UiEvent::SearchResults {
                pane_id,
                generation,
                total_scrollback,
                matches,
            } => {
                if self
                    .state
                    .apply_search_results(pane_id, generation, total_scrollback, matches)
                {
                    self.scroll_to_active_search_match();
                }
            }
            UiEvent::AgentStateChanged {
                workspace_id,
                pane_id,
                state,
            } => {
                self.state.apply_agent_state(workspace_id, pane_id, state);
            }
            UiEvent::WorkspaceRuntimeUpdated {
                workspace_id,
                bound_ports,
                containers,
                git_status,
                git_branch,
            } => {
                self.state.apply_workspace_runtime(
                    workspace_id,
                    bound_ports,
                    containers,
                    git_status,
                    git_branch,
                );
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
                        self.state.worktree_modal.open,
                        self.state.close_modal.open,
                        self.state.search.is_some(),
                        self.state.action_palette.open,
                    );
                    self.handle_ui_action(action);
                }
            }
            WindowEvent::Ime(Ime::Commit(text)) => {
                if self.state.picker.open
                    || self.state.close_modal.open
                    || self.state.action_palette.open
                {
                    // Selection and confirmation modals swallow IME text.
                } else if self.state.worktree_modal.open {
                    if let Some(cleaned) = filter_modal_text(&text, false) {
                        self.handle_ui_action(UiAction::WorktreeModalText(cleaned));
                    }
                } else if self.state.search.is_some() {
                    if let Some(cleaned) = filter_modal_text(&text, true) {
                        self.handle_ui_action(UiAction::SearchText(cleaned));
                    }
                } else if !text.is_empty() && !text.chars().all(char::is_control) {
                    self.handle_ui_action(UiAction::SendBytes(text.into_bytes()));
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
                    Err(RenderError::Reconfigure) => {
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
                    Err(RenderError::Timeout) => {
                        warn!("wgpu surface frame timed out");
                    }
                    Err(RenderError::Validation) => {
                        warn!("wgpu surface validation error");
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
    let ui_config = UiConfig::load(args.config.as_deref());
    let default_agent = args.agent.unwrap_or(ui_config.default_agent);
    let socket_path = args.socket.unwrap_or_else(default_socket_path);
    let remote_host = socket_path
        .to_str()
        .filter(|path| path.starts_with("ssh://"))
        .map(daedalus_proto::remote::SshTarget::parse)
        .transpose()?
        .map(|target| target.host);
    let initial_dir = args.dir.unwrap_or_else(|| {
        if remote_host.is_some() {
            PathBuf::from(".")
        } else {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        }
    });

    let config = BridgeConfig {
        socket_path,
        auth_token: args.auth_token,
        initial_dir,
        default_agent,
        auto_spawn_daemon: !args.no_auto_spawn,
    };

    let event_loop: EventLoop<UiEvent> = EventLoop::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let mut app = DaedalusApp::new(config, ui_config, proxy);
    app.state.remote_host = remote_host;
    event_loop.run_app(&mut app)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
