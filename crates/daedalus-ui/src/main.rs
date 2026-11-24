#![cfg_attr(windows, windows_subsystem = "windows")]

mod accessibility;
mod pointer;
mod setup;
mod workspace_settings;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser;
use daedalus_proto::{PaneId, PaneRole, WorkspaceId, default_socket_path, wrap_bracketed_paste};
use daedalus_ui::bridge::{BridgeConfig, DaemonBridge, UiCommand, UiEvent, UiEventSink};
use daedalus_ui::config::UiConfig;
use daedalus_ui::input::{KeyRepeatGuard, UiAction, filter_modal_text, translate_key};
use daedalus_ui::layout::overlays::picker_row_stride;
use daedalus_ui::layout::{HitTarget, LayoutInput, WorkspaceLayout, compute_layout, hit_test};
use daedalus_ui::links::{detect_link_in_row, detect_url_in_row, open_link};
use daedalus_ui::renderer::{GpuRenderer, RenderError};
use daedalus_ui::selection::extract_selected_text;
use daedalus_ui::state::{AgentPreset, AppState, ConnectionState};
use daedalus_ui::theme::Theme;
use tracing::{error, warn};
use tracing_subscriber::EnvFilter;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

const INITIAL_WINDOW_WIDTH: f64 = 1440.0;
const INITIAL_WINDOW_HEIGHT: f64 = 900.0;
const RESIZE_DEBOUNCE: Duration = Duration::from_millis(32);
const TOAST_TTL: Duration = Duration::from_secs(5);
const WHEEL_SCROLL_LINES: isize = 3;
const FALLBACK_PANE_COLS: u16 = 120;
const FALLBACK_PANE_ROWS: u16 = 36;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ScrollTarget {
    Sidebar,
    Presets,
    Settings,
    Recipients,
    Actions,
    Pane(PaneId),
}

#[derive(Default)]
struct WheelScroll {
    target: Option<ScrollTarget>,
    remainder: f64,
}

impl WheelScroll {
    fn steps(
        &mut self,
        target: ScrollTarget,
        delta: MouseScrollDelta,
        physical_step_height: f32,
    ) -> isize {
        if self.target != Some(target) {
            self.target = Some(target);
            self.remainder = 0.0;
        }
        self.remainder += match delta {
            MouseScrollDelta::LineDelta(_, y) => match target {
                ScrollTarget::Pane(_) => f64::from(y) * WHEEL_SCROLL_LINES as f64,
                _ => f64::from(y),
            },
            MouseScrollDelta::PixelDelta(pos) => pos.y / f64::from(physical_step_height.max(1.0)),
        };
        let whole = self.remainder.trunc();
        self.remainder -= whole;
        whole as isize
    }
}

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

#[allow(clippy::large_enum_variant)]
enum AppEvent {
    Daemon(UiEvent),
    Accessibility(accesskit_winit::Event),
}

impl From<accesskit_winit::Event> for AppEvent {
    fn from(event: accesskit_winit::Event) -> Self {
        Self::Accessibility(event)
    }
}

struct EventLoopProxySink(EventLoopProxy<AppEvent>);

impl UiEventSink for EventLoopProxySink {
    fn emit(&self, event: UiEvent) {
        let _ = self.0.send_event(AppEvent::Daemon(event));
    }
}

#[derive(Clone, Copy)]
struct ViewportMetrics {
    size: winit::dpi::PhysicalSize<u32>,
    scale_factor: f32,
    cell: (f32, f32),
}

struct DaedalusApp {
    config: BridgeConfig,
    ui_config: UiConfig,
    project_setups: daedalus_ui::project_setups::ProjectSetups,
    daemon_home: Option<PathBuf>,
    setup: setup::SetupFlow,
    history: daedalus_ui::activity_history::ActivityHistory,
    focused: bool,
    occluded: bool,
    proxy: Option<EventLoopProxy<AppEvent>>,
    window: Option<Arc<Window>>,
    renderer: Option<GpuRenderer>,
    accessibility: Option<accesskit_winit::Adapter>,
    accessibility_active: bool,
    terminal_controls: accessibility::TerminalControls,
    last_accessibility_tree: Option<accesskit::TreeUpdate>,
    bridge: Option<Box<dyn Fn(UiCommand) -> bool>>,
    state: AppState,
    modifiers: ModifiersState,
    key_repeats: KeyRepeatGuard,
    pointer: pointer::PointerState,
    wheel_scroll: WheelScroll,
    pending_resize_flush_at: Option<Instant>,
    clipboard: Option<arboard::Clipboard>,
}

impl DaedalusApp {
    fn new(
        config: BridgeConfig,
        ui_config: UiConfig,
        proxy: Option<EventLoopProxy<AppEvent>>,
        profile: &std::path::Path,
    ) -> Self {
        Self {
            project_setups: Default::default(),
            daemon_home: None,
            setup: Default::default(),
            history: daedalus_ui::activity_history::ActivityHistory::new(
                profile,
                &config.socket_path,
            ),
            focused: false,
            occluded: false,
            config,
            ui_config,
            proxy,
            window: None,
            renderer: None,
            accessibility: None,
            accessibility_active: false,
            terminal_controls: accessibility::TerminalControls::default(),
            last_accessibility_tree: None,
            bridge: None,
            state: AppState::new(),
            modifiers: ModifiersState::empty(),
            key_repeats: KeyRepeatGuard::default(),
            pointer: pointer::PointerState::default(),
            wheel_scroll: WheelScroll::default(),
            pending_resize_flush_at: None,
            clipboard: None,
        }
    }

    fn sync_history_visit(&mut self) {
        let visible = self.focused
            && !self.occluded
            && !self.state.setup_review.open
            && !self.state.settings.open
            && !self.state.workspace_form.open
            && !self.state.worktree_modal.open
            && !self.state.close_modal.open
            && !self.state.picker.open
            && !self.state.action_palette.open;
        self.history
            .visit(self.state.active_workspace_id.filter(|_| visible));
    }

    fn request_redraw(&self) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn send_command(&self, cmd: UiCommand) -> bool {
        self.bridge.as_ref().is_some_and(|bridge| bridge(cmd))
    }

    fn schedule_resize_flush(&mut self) {
        self.pending_resize_flush_at = Some(Instant::now() + RESIZE_DEBOUNCE);
    }

    fn has_top_banner(&self) -> bool {
        self.state.connection != ConnectionState::Connected || self.state.toast.is_some()
    }

    fn viewport_metrics(&self) -> Option<ViewportMetrics> {
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

    fn compute_current_layout(&self) -> Option<WorkspaceLayout> {
        Some(self.layout_for(self.viewport_metrics()?))
    }

    fn layout_for(&self, metrics: ViewportMetrics) -> WorkspaceLayout {
        let ViewportMetrics {
            size,
            scale_factor,
            cell: (cell_w, cell_h),
        } = metrics;
        let mut layout = compute_layout(&LayoutInput {
            window_w: size.width as f32,
            window_h: size.height as f32,
            scale_factor,
            cell_w,
            cell_h,
            sidebar_override: self.state.sidebar_override,
            has_top_banner: self.has_top_banner(),
            zoomed: self.state.zoomed,
            focused_role: self.state.focused_role,
            runner_has_failure: self.state.runner_has_failure(),
        });
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
        if self.state.active_test_workspace().is_some() {
            layout.enable_run_tests(scale_factor);
        }
        layout
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
        if self.state.connection != ConnectionState::Connected {
            return;
        }

        let Some(layout) = self.compute_current_layout() else {
            return;
        };

        self.resize_panes(&layout);
    }

    fn resize_panes(&mut self, layout: &WorkspaceLayout) {
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

    fn clipboard_text(&mut self) -> Option<String> {
        match self.ensure_clipboard().and_then(|cb| {
            cb.get_text()
                .map_err(|err| format!("Failed to read clipboard: {err}"))
        }) {
            Ok(text) => (!text.is_empty()).then_some(text),
            Err(err) => {
                self.show_toast(err, true);
                None
            }
        }
    }

    fn paste_clipboard_to_focused(&mut self) {
        let Some(pane_id) = self.state.focused_pane_id() else {
            return;
        };
        let Some(text) = self.clipboard_text() else {
            return;
        };
        if let Some(pane) = self.state.panes.get_mut(&pane_id) {
            pane.reset_scroll();
            pane.selection = None;
        }
        let data = wrap_bracketed_paste(text.as_bytes());
        self.send_command(UiCommand::SendInput { pane_id, data });
        self.request_redraw();
    }

    /// Where a new project starts: the `--dir` folder, else the daemon host's home, else nothing.
    /// Never this process's working directory, which means nothing on the daemon host.
    fn folder_suggestion(&self) -> PathBuf {
        if self.config.bootstrap_explicit_dir {
            self.config.initial_dir.clone()
        } else {
            self.daemon_home.clone().unwrap_or_default()
        }
    }

    /// Keeps folders that are open as a workspace out of the form's recent list.
    fn hide_open_recent_projects(&mut self) {
        let open = self
            .state
            .workspaces
            .iter()
            .map(|ws| ws.root_dir.clone())
            .collect();
        self.state.workspace_form.hide_open(open);
    }

    fn create_new_workspace(&mut self) {
        if self.setup_installer_is_running() {
            self.show_toast(
                "Finish or close the setup terminal before starting another workspace.",
                false,
            );
            return;
        }
        self.reset_setup_draft();
        let folder = self.folder_suggestion();
        self.state.workspace_form.begin(&folder);
        self.hide_open_recent_projects();
        self.send_command(UiCommand::RefreshRecentProjects);
        self.state
            .workspace_form
            .set_focus(daedalus_ui::workspace_form::Control::Folder);
        self.state.close_search();
        self.request_redraw();
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
        if self.state.close_modal.submitting {
            return;
        }
        if self.state.connection != ConnectionState::Connected {
            self.show_toast(
                "Reconnect to the daemon before closing this workspace.",
                true,
            );
            return;
        }
        let compose_down = self.state.close_modal.compose_down;
        let remove_worktree = self.state.close_modal.remove_worktree;
        let force_remove_worktree = self.state.close_modal.force_remove_worktree;
        self.state.close_modal.submitting = self.send_command(UiCommand::CloseWorkspace {
            id,
            compose_down,
            remove_worktree,
            force_remove_worktree,
        });
        self.request_redraw();
    }

    fn sidebar_list(&self) -> Option<std::sync::Arc<daedalus_ui::sidebar_content::SidebarContent>> {
        self.compute_current_layout()?.sidebar_content
    }

    fn reveal_active_workspace(&mut self) {
        use daedalus_ui::sidebar_content::SidebarRow;
        if let Some(list) = self.sidebar_list()
            && let Some(index) = list.rows.iter().position(|row| matches!(row,
                SidebarRow::Workspace(i) if Some(self.state.workspaces[*i].id) == self.state.active_workspace_id))
            && !list.visible.contains(&index)
        {
            self.state.sidebar_first = index.min(list.max_first);
        }
    }

    fn scroll_sidebar(
        &mut self,
        list: &daedalus_ui::sidebar_content::SidebarContent,
        delta: isize,
    ) {
        self.state.sidebar_first = list
            .visible
            .start
            .saturating_add_signed(delta)
            .min(list.max_first);
        self.request_redraw();
    }

    fn switch_workspace_by(&mut self, delta: isize) {
        let prev_id = self.state.active_workspace_id;
        if let Some(next_id) = self.state.cycle_workspace(delta) {
            self.ensure_active_workspace_panes();
            if Some(next_id) != prev_id {
                self.reveal_active_workspace();
                self.schedule_resize_flush();
            }
            self.request_redraw();
        }
    }

    fn submit_worktree_modal(&mut self) {
        if self.state.connection != ConnectionState::Connected {
            return;
        }
        let Some(branch) = self.state.worktree_modal.branch_for_submit() else {
            self.request_redraw();
            return;
        };
        let repo_dir = self
            .state
            .active_workspace()
            .map(|ws| ws.root_dir.clone())
            .unwrap_or_else(|| self.folder_suggestion());
        if self.send_command(UiCommand::CreateWorktreeWorkspace {
            repo_dir,
            branch,
            default_agent: self.config.default_agent,
        }) {
            self.state.worktree_modal.close();
        }
        self.request_redraw();
    }

    fn apply_agent_preset(&mut self, preset: AgentPreset, add: bool) {
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

        self.send_command(if add {
            UiCommand::AddAgent {
                workspace_id,
                preset,
                cols,
                rows,
            }
        } else {
            UiCommand::ReplaceAgentPreset {
                workspace_id,
                old_pane_id,
                preset,
                cols,
                rows,
            }
        });
        self.state.focused_role = PaneRole::Agent;
        self.request_redraw();
    }

    fn forward_runner_failure_to_agent(&mut self) {
        match self.state.failure_recipient_choices() {
            Err(message) => self.show_toast(message, true),
            Ok(choices) if choices.agents.len() == 1 => self.send_failure_to_recipient(choices),
            Ok(choices) => {
                self.state.failure_recipients = Some(choices);
                self.request_redraw();
            }
        }
    }

    fn send_failure_to_recipient(&mut self, choices: daedalus_ui::state::FailureRecipients) {
        let agent_pane_id = match self.state.validate_failure_recipient(&choices) {
            Ok(id) => id,
            Err(message) => {
                self.show_toast(message, true);
                return;
            }
        };
        self.send_command(UiCommand::ForwardRunnerFailureToAgent {
            runner_pane_id: choices.runner,
            agent_pane_id,
        });
        self.state
            .select_session(choices.workspace, PaneRole::Agent, agent_pane_id);
        if self.state.zoomed {
            self.schedule_resize_flush();
        }
        self.request_redraw();
    }

    fn request_scrollback_if_needed(&mut self, pane_id: PaneId) {
        let Some(pane) = self.state.panes.get_mut(&pane_id) else {
            return;
        };
        if let Some(generation) = pane.begin_scrollback_request() {
            let command = UiCommand::RequestScrollback {
                pane_id,
                generation,
                offset_from_bottom: pane.requested_scroll_offset,
                count: usize::from(pane.summary.rows).max(1),
            };
            self.send_command(command);
        }
    }

    fn scroll_pane(&mut self, role: PaneRole, delta: isize) {
        if delta == 0 {
            return;
        }
        let Some(pane_id) = self.state.pane_id_for_role(role) else {
            return;
        };
        let Some(pane) = self.state.panes.get_mut(&pane_id) else {
            return;
        };
        if delta > 0 {
            // Older daemons omit history length from deltas. Let the daemon clamp the request.
            pane.set_scroll_offset(pane.requested_scroll_offset.saturating_add(delta as usize));
        } else {
            pane.adjust_scroll_offset(delta);
        }
        self.request_scrollback_if_needed(pane_id);
        self.request_redraw();
    }

    fn ensure_active_workspace_panes(&mut self) {
        let Some(ws) = self.state.active_dormant_workspace() else {
            return;
        };
        let workspace_id = ws.id;
        let manifest = ws.manifest.clone();
        if self.state.connection == ConnectionState::Connected
            && self.send_command(UiCommand::EnsureWorkspacePanes {
                workspace_id,
                default_agent: self.config.default_agent,
                manifest,
            })
        {
            self.state.mark_workspace_spawning(workspace_id);
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
        {
            let Some(pane) = self.state.panes.get_mut(&pane_id) else {
                return;
            };
            pane.scrollback_len = search.total_scrollback;
            if pane
                .visible_rows()
                .iter()
                .any(|r| r.abs_row == active.abs_row)
            {
                pane.set_scroll_offset(pane.scroll_offset);
            } else {
                let viewport_rows = (pane.summary.rows as usize).max(1);
                let live_start = pane
                    .viewport_rows
                    .last()
                    .and_then(|r| (r.abs_row + 1).checked_sub(viewport_rows as u64))
                    .max(Some(search.total_scrollback as u64));
                let target_offset = search
                    .target_scroll_offset_with_live_start(viewport_rows, live_start)
                    .unwrap_or(0);
                pane.set_scroll_offset(target_offset);
            }
        }
        self.request_scrollback_if_needed(pane_id);
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
                self.state.focus_role_shortcut(role);
                if self.state.zoomed {
                    self.schedule_resize_flush();
                }
                self.request_redraw();
            }
            UiAction::FocusDir(dir) => {
                let stacked = self
                    .compute_current_layout()
                    .is_some_and(|layout| layout.stacked);
                self.state.focus_direction(dir, stacked);
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
                if let Some(layout) = self.compute_current_layout() {
                    self.state.sidebar_override = Some(layout.sidebar.is_none());
                    self.reveal_active_workspace();
                }
                self.schedule_resize_flush();
                self.request_redraw();
            }
            UiAction::ToggleActionPalette => {
                if self.state.action_palette.open {
                    self.state.action_palette.close();
                } else {
                    self.pointer.end_drag();
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
            UiAction::CloseTerminal => {
                if let Some(id) = self.state.focused_pane_id() {
                    self.send_command(UiCommand::ClosePane(id));
                }
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
            UiAction::RunTests => self.run_saved_tests(),
            UiAction::WorkspaceSettings => self.open_workspace_settings(),
            UiAction::NewWorkspace => self.create_new_workspace(),
            UiAction::OpenFailedRun(workspace) => self.open_failed_run(workspace),
            UiAction::OpenNextFailedRun => {
                if let Some(workspace) = self.state.next_failed_workspace() {
                    self.open_failed_run(workspace);
                } else {
                    self.show_toast("No failed runs in open workspaces", false);
                }
            }
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
                    .map_or(20, |p| (p.summary.rows as isize).saturating_sub(1).max(1));
                self.scroll_pane(self.state.focused_role, page);
            }
            UiAction::ScrollPageDown => {
                let page = self
                    .state
                    .pane_for_role(self.state.focused_role)
                    .map_or(20, |p| (p.summary.rows as isize).saturating_sub(1).max(1));
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
            UiAction::PickerConfirm | UiAction::PickerAdd => {
                let preset = self.state.picker.selected_preset();
                self.apply_agent_preset(preset, action == UiAction::PickerAdd);
            }
            UiAction::PickerCancel => {
                self.state.picker.close();
                self.request_redraw();
            }
            UiAction::WorktreeModalText(text) => {
                self.state.worktree_modal.push_text(&text);
                self.request_redraw();
            }
            UiAction::WorktreeModalSetText(text) => {
                self.state.worktree_modal.set_text(text);
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
                if self.state.close_modal.submitting {
                    return;
                }
                self.state.close_modal.close();
                self.request_redraw();
            }
            UiAction::PasteSearchQuery => {
                if let Some(text) = self
                    .clipboard_text()
                    .and_then(|text| filter_modal_text(&text, true))
                {
                    self.handle_ui_action(UiAction::SearchText(text));
                }
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
            UiAction::SearchToggleCase => {
                if let Some(ref mut search) = self.state.search {
                    search.toggle_case();
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

    fn key_pressed(
        &mut self,
        physical_key: winit::keyboard::PhysicalKey,
        logical_key: &winit::keyboard::Key,
        text: Option<&str>,
        repeat: bool,
    ) {
        if let Some(choices) = &mut self.state.failure_recipients {
            self.key_repeats.suppress(physical_key);
            use winit::keyboard::{Key, NamedKey};
            match logical_key {
                Key::Named(NamedKey::ArrowUp) => choices.advance(-1),
                Key::Named(NamedKey::ArrowDown) => choices.advance(1),
                Key::Named(NamedKey::Escape) => self.state.failure_recipients = None,
                Key::Named(NamedKey::Enter) if !repeat => {
                    if let Some(choices) = self.state.failure_recipients.take() {
                        self.send_failure_to_recipient(choices);
                    }
                }
                _ => {}
            }
            self.request_redraw();
            return;
        }
        if self.state.workspace_form.open
            || self.state.settings.open
            || self.state.setup_review.open
        {
            if self
                .key_repeats
                .form_press(physical_key, logical_key, repeat)
            {
                if self.state.setup_review.open {
                    self.setup_key(logical_key);
                } else if self.state.workspace_form.open {
                    self.workspace_form_key(logical_key);
                } else {
                    self.settings_key(logical_key);
                }
            }
            return;
        }
        if self.handle_recap_key(logical_key, repeat) {
            self.key_repeats.suppress(physical_key);
            return;
        }
        let action = translate_key(
            logical_key,
            text,
            self.modifiers,
            self.state.picker.open,
            self.state.worktree_modal.open,
            self.state.close_modal.open,
            self.state.search.is_some(),
            self.state.action_palette.open,
        );
        let action = self.key_repeats.press(physical_key, repeat, action);
        self.handle_ui_action(action);
    }

    fn commit_text(&mut self, text: String) {
        if self.state.failure_recipients.is_some() || self.state.recap.keyboard_row.is_some() {
            return;
        }
        if self.state.setup_review.open {
            self.state.setup_review.insert_text(&text);
            self.request_redraw();
            return;
        }
        if self.state.workspace_form.open {
            self.state.workspace_form.insert_text(&text);
            self.request_redraw();
            return;
        }
        if self.state.settings.open {
            self.state.settings.insert_text(&text);
            self.request_redraw();
            return;
        }
        if self.state.picker.open || self.state.close_modal.open || self.state.action_palette.open {
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

    fn handle_recap_key(&mut self, key: &winit::keyboard::Key, repeat: bool) -> bool {
        use winit::keyboard::{Key, NamedKey};
        if self.state.picker.open
            || self.state.worktree_modal.open
            || self.state.close_modal.open
            || self.state.search.is_some()
            || self.state.action_palette.open
        {
            return false;
        }
        let open = daedalus_ui::input::is_primary_shift_char(
            key,
            self.modifiers,
            "y",
            cfg!(target_os = "macos"),
        );
        if !open && self.state.recap.keyboard_row.is_none() {
            return false;
        }
        let targets = self
            .renderer
            .as_ref()
            .map_or(&[][..], |r| r.recap_targets());
        let selected = self
            .state
            .recap
            .keyboard_row
            .as_ref()
            .and_then(|current| targets.iter().position(|(_, row)| row == current));
        if !repeat {
            self.state.recap.keyboard_row = match key {
                _ if open => targets.first().map(|(_, row)| row.clone()),
                Key::Named(NamedKey::ArrowDown | NamedKey::ArrowUp) if !targets.is_empty() => {
                    let index = selected.unwrap_or(0);
                    let next = if matches!(key, Key::Named(NamedKey::ArrowUp)) {
                        index.checked_sub(1).unwrap_or(targets.len() - 1)
                    } else {
                        (index + 1) % targets.len()
                    };
                    Some(targets[next].1.clone())
                }
                Key::Named(NamedKey::Enter) => {
                    if let Some(index) = selected {
                        let row = targets[index].1.clone();
                        if self.state.focus_recap_row(&row) {
                            self.schedule_resize_flush();
                        }
                    }
                    None
                }
                Key::Named(NamedKey::Escape) => None,
                _ => self.state.recap.keyboard_row.take(),
            };
            self.request_redraw();
        }
        true
    }

    fn open_failed_run(&mut self, workspace: WorkspaceId) {
        let Some(pane) = self
            .state
            .workspaces
            .iter()
            .find(|ws| ws.id == workspace)
            .and_then(|ws| self.state.failed_runner(ws))
        else {
            return;
        };
        self.state.select_workspace(workspace);
        self.state.select_session(workspace, PaneRole::Runner, pane);
        self.state.recap.keyboard_row = None;
        self.reveal_active_workspace();
        self.schedule_resize_flush();
        self.request_redraw();
    }

    fn activate_session(&mut self, target: HitTarget) {
        match target {
            HitTarget::CloseSession(id) => {
                self.send_command(UiCommand::ClosePane(id));
            }
            HitTarget::SelectSession(id) => {
                if let Some(pane) = self.state.panes.get(&id) {
                    self.state
                        .select_session(pane.summary.workspace_id, pane.summary.role, id);
                    self.schedule_resize_flush();
                    self.request_redraw();
                }
            }
            _ => {}
        }
    }

    fn handle_mouse_wheel(&mut self, delta: MouseScrollDelta) {
        self.scroll_at(self.viewport_metrics(), delta);
    }

    fn scroll_at(&mut self, metrics: Option<ViewportMetrics>, delta: MouseScrollDelta) {
        let scale_factor = metrics.map_or(1., |m| m.scale_factor);
        if let Some(choices) = &mut self.state.failure_recipients {
            let steps = self.wheel_scroll.steps(
                ScrollTarget::Recipients,
                delta,
                picker_row_stride(scale_factor),
            );
            choices.advance(steps.saturating_neg());
            self.request_redraw();
            return;
        }
        if self.state.settings.open {
            let steps = self.wheel_scroll.steps(ScrollTarget::Settings, delta, 30.);
            if steps != 0 {
                self.state.settings.scroll(steps as f32);
                self.request_redraw();
            }
            return;
        }
        if self.state.workspace_form.open
            || self.state.setup_review.open
            || self.state.worktree_modal.open
            || self.state.close_modal.open
        {
            self.wheel_scroll = WheelScroll::default();
            return;
        }
        let Some(metrics) = metrics else {
            return;
        };

        if self.state.picker.open || self.state.action_palette.open {
            let (target, count, selected) = if self.state.picker.open {
                (
                    ScrollTarget::Presets,
                    AgentPreset::ALL.len(),
                    &mut self.state.picker.selected_index,
                )
            } else {
                (
                    ScrollTarget::Actions,
                    self.state.active_actions().len(),
                    &mut self.state.action_palette.selected_index,
                )
            };
            let steps = self
                .wheel_scroll
                .steps(target, delta, picker_row_stride(scale_factor));
            let next = selected
                .saturating_add_signed(steps.saturating_neg())
                .min(count.saturating_sub(1));
            if next != *selected {
                *selected = next;
                self.request_redraw();
            }
            return;
        }
        let (cell_w, cell_h) = metrics.cell;
        let layout = self.layout_for(metrics);

        if let Some(list) = layout.sidebar_content.as_ref()
            && layout.sidebar.is_some_and(|sidebar| {
                sidebar.contains(self.pointer.position.0, self.pointer.position.1)
            })
        {
            let metrics = daedalus_ui::layout::LayoutMetrics::for_scale(scale_factor);
            let steps = self.wheel_scroll.steps(
                ScrollTarget::Sidebar,
                delta,
                metrics.sidebar_row_height + metrics.sidebar_row_gap,
            );
            if steps != 0 {
                self.scroll_sidebar(list, steps.saturating_neg());
            }
            return;
        }

        let target_role = match hit_test(
            &layout,
            self.state.workspaces.len(),
            scale_factor,
            cell_w,
            cell_h,
            self.pointer.position.0,
            self.pointer.position.1,
        ) {
            HitTarget::PaneViewport { role, .. }
            | HitTarget::PaneHeader(role)
            | HitTarget::PaneHeaderAction(role) => Some(role),
            _ => None,
        };

        if let Some(role) = target_role
            && let Some(pane_id) = self.state.pane_id_for_role(role)
        {
            let steps = self
                .wheel_scroll
                .steps(ScrollTarget::Pane(pane_id), delta, cell_h);
            self.scroll_pane(role, steps);
        } else {
            self.wheel_scroll = WheelScroll::default();
        }
    }
}

impl DaedalusApp {
    fn handle_daemon_event(&mut self, event: UiEvent) {
        let previous_workspace = self.state.active_workspace_id;
        let search_generation = self.state.search.as_ref().map(|search| search.generation);
        self.state
            .recap
            .observe(&event, &self.state.workspaces, &self.state.panes);
        let changed = match &event {
            UiEvent::AgentStateChanged { workspace_id, .. }
            | UiEvent::WorkspaceRuntimeUpdated { workspace_id, .. } => Some(*workspace_id),
            UiEvent::ServerEvent(daedalus_proto::ServerEvent::ExternalStatusUpdated {
                workspace_id,
                ..
            }) => Some(*workspace_id),
            UiEvent::ServerEvent(event) => {
                let pane = match event {
                    daedalus_proto::ServerEvent::GridDelta(delta)
                        if !delta.dirty_rows.is_empty()
                            && self
                                .state
                                .panes
                                .get(&delta.pane_id)
                                .is_some_and(|pane| pane.summary.role == PaneRole::Agent) =>
                    {
                        Some(delta.pane_id)
                    }
                    daedalus_proto::ServerEvent::ResyncRequired { pane_id } => Some(*pane_id),
                    daedalus_proto::ServerEvent::CommandFinished { pane_id, .. }
                    | daedalus_proto::ServerEvent::PaneExited { pane_id, .. } => Some(*pane_id),
                    _ => None,
                };
                pane.and_then(|id| self.state.panes.get(&id))
                    .map(|pane| pane.summary.workspace_id)
            }
            _ => None,
        };
        if let Some(id) = changed {
            self.history.changed(id);
        }
        self.sync_history_visit();
        match event {
            UiEvent::JournalCapability(supported) => self.history.connection(supported),
            UiEvent::DaemonCapabilities {
                home_dir,
                project_setups,
                recent_projects,
            } => {
                self.daemon_home = home_dir;
                self.project_setups.set_daemon_backed(project_setups);
                if !recent_projects {
                    // Rows from the previous daemon would name folders this one never saw.
                    self.state.workspace_form.set_recent(Vec::new());
                }
            }
            UiEvent::RecentProjectsLoaded(projects) => {
                self.hide_open_recent_projects();
                self.state.workspace_form.set_recent(projects);
            }
            UiEvent::ProjectSetupChanged {
                root_dir,
                assignments,
            } => self.project_setup_changed(&root_dir, assignments),
            UiEvent::ProjectSetupSaveFailed {
                workspace_id,
                message,
            } => {
                self.project_setups.rejected(workspace_id);
                self.refresh_workspace_setup(workspace_id);
                self.show_toast(message, true);
            }
            UiEvent::ActivityRead { token, page } => {
                use daedalus_ui::activity_history::PageUpdate;
                match self.history.received(token, &page) {
                    Some(PageUpdate::Recap { first, gap }) => {
                        self.state
                            .recap
                            .journal(&page, first, gap, &self.state.panes)
                    }
                    Some(PageUpdate::Foreground(count)) => self
                        .state
                        .recap
                        .foreground_activity(page.workspace_id, count),
                    None => {}
                }
            }
            UiEvent::ActivityReadFailed(token) => self.history.failed(token),
            UiEvent::Connected => {
                self.state.connection = ConnectionState::Connected;
                self.schedule_resize_flush();
            }
            UiEvent::Disconnected => {
                self.setup_disconnected();
                self.history.connection(false);
                self.state.connection = ConnectionState::Reconnecting;
                self.state.workspace_list_loaded = false;
                let form = &mut self.state.workspace_form;
                form.finish_submission(
                    form.session(),
                    Some("Connection lost. Check the workspace list before retrying.".into()),
                );
                self.state.close_modal.submitting = false;
                self.state.last_sent_sizes.clear();
                self.state.spawning_workspaces.clear();
                self.schedule_resize_flush();
            }
            UiEvent::WorkspaceCloseFinished(id) => {
                if self.state.close_modal.workspace_id == Some(id) {
                    self.state.close_modal.submitting = false;
                }
            }
            UiEvent::WorkspaceCloseFailed(workspaces) => {
                self.state.set_workspaces(workspaces);
                self.schedule_resize_flush();
            }
            UiEvent::WorkspacesLoaded(workspaces) => {
                self.project_setups.absorb(&workspaces);
                for (workspace_id, assignments) in self.project_setups.migrations(&workspaces) {
                    if !self.send_command(UiCommand::SaveProjectSetup {
                        workspace_id,
                        assignments,
                    }) {
                        self.project_setups.rejected(workspace_id);
                    }
                }
                self.restore_session_names(&workspaces);
                self.state.set_workspaces(workspaces);
                self.reconcile_setup_installer();
                self.offer_first_workspace();
                self.ensure_active_workspace_panes();
                self.schedule_resize_flush();
            }
            UiEvent::SetupInspected { session, result } => self.setup_inspected(session, result),
            UiEvent::SetupInstallerStarted {
                workspace_id,
                pane_id,
            } => self.setup_installer_started(workspace_id, pane_id),
            UiEvent::SetupInstallerFinished(pane, code) => {
                self.setup_installer_finished(pane, code)
            }
            UiEvent::SetupInstallerFailed {
                workspace_id,
                message,
            } => self.setup_failed(workspace_id, message),
            UiEvent::SetupApplied {
                workspace_id,
                error,
            } => self.setup_applied(workspace_id, error),
            UiEvent::ManualWorkspaceCreated { session, workspace } => {
                self.setup_created(session, workspace);
            }
            UiEvent::ManualWorkspaceFailed { session, message } => {
                self.setup_creation_failed(session, message)
            }
            UiEvent::WorkspaceCreated(ws) => {
                let ws_id = ws.id;
                self.project_setups.absorb(std::slice::from_ref(&ws));
                self.state.upsert_workspace(ws);
                self.state.select_workspace(ws_id);
                self.state.close_search();
                self.schedule_resize_flush();
            }
            UiEvent::WorkspacePanesEnsured {
                workspace_id,
                complete,
            } => {
                self.state.finish_workspace_spawning(workspace_id, complete);
            }
            UiEvent::PaneSpawned {
                summary,
                agent_preset,
            } => {
                self.state.register_pane(summary, agent_preset);
                self.schedule_resize_flush();
            }
            UiEvent::AssignmentStarted {
                workspace_id,
                pane_id,
                assignment,
            } => self.assignment_started(workspace_id, pane_id, assignment),
            UiEvent::PaneClosed { pane_id } => {
                self.setup_installer_finished(pane_id, None);
                self.state.remove_session(pane_id);
                self.schedule_resize_flush();
            }
            UiEvent::Snapshot(snapshot) => {
                let pane_id = snapshot.pane_id;
                if let Some(pane_id) = self.state.apply_grid_snapshot(snapshot) {
                    self.send_command(UiCommand::RequestSnapshot { pane_id });
                } else {
                    self.request_scrollback_if_needed(pane_id);
                }
            }
            UiEvent::ScrollbackLoaded {
                pane_id,
                generation,
                total_scrollback,
                rows,
            } => {
                if let Some(pane) = self.state.panes.get_mut(&pane_id) {
                    pane.apply_scrollback(generation, total_scrollback, rows);
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
                let ended = match &server_event {
                    daedalus_proto::ServerEvent::PaneExited { pane_id, exit_code } => {
                        Some((*pane_id, *exit_code))
                    }
                    _ => None,
                };
                let changed_pane = match &server_event {
                    daedalus_proto::ServerEvent::GridDelta(delta)
                        if !delta.dirty_rows.is_empty()
                            && self
                                .state
                                .panes
                                .get(&delta.pane_id)
                                .is_some_and(|pane| pane.summary.role == PaneRole::Agent) =>
                    {
                        Some(delta.pane_id)
                    }
                    daedalus_proto::ServerEvent::ResyncRequired { pane_id } => Some(*pane_id),
                    _ => None,
                };
                if let Some(resync_pane_id) = self.state.apply_server_event(server_event) {
                    self.send_command(UiCommand::RequestSnapshot {
                        pane_id: resync_pane_id,
                    });
                }
                if let Some((id, code)) = ended {
                    self.setup_installer_finished(id, code);
                }
                if let Some(pane_id) = changed_pane {
                    self.request_scrollback_if_needed(pane_id);
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

        if let Some(search) = &self.state.search
            && Some(search.generation) != search_generation
        {
            self.run_pane_search();
        }
        self.state.recap.select(self.state.active_workspace_id);
        if previous_workspace != self.state.active_workspace_id {
            self.reveal_active_workspace();
        }
        self.flush_due_resizes(false);
        self.request_redraw();
    }
}

impl ApplicationHandler<AppEvent> for DaedalusApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let Some(proxy) = self.proxy.as_ref() else {
            return;
        };
        let attrs = Window::default_attributes()
            .with_visible(false)
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
        self.accessibility = Some(accesskit_winit::Adapter::with_event_loop_proxy(
            event_loop,
            &window,
            proxy.clone(),
        ));
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

        let sink = Arc::new(EventLoopProxySink(proxy.clone()));
        let bridge = DaemonBridge::start(self.config.clone(), sink);

        window.set_visible(true);
        self.focused = window.has_focus();
        self.state.recap.focus(self.focused);
        window.request_redraw();
        self.window = Some(window);
        self.renderer = Some(renderer);
        self.bridge = Some(Box::new(move |command| bridge.send(command)));
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: AppEvent) {
        let event = match event {
            AppEvent::Daemon(event) => event,
            AppEvent::Accessibility(event) => {
                if self
                    .window
                    .as_ref()
                    .is_some_and(|window| window.id() == event.window_id)
                {
                    match event.window_event {
                        accesskit_winit::WindowEvent::InitialTreeRequested => {
                            self.accessibility_active = true;
                            self.last_accessibility_tree = None;
                        }
                        accesskit_winit::WindowEvent::AccessibilityDeactivated => {
                            self.accessibility_active = false;
                            self.last_accessibility_tree = None;
                        }
                        accesskit_winit::WindowEvent::ActionRequested(request) => {
                            if self.accessibility_active {
                                if let Some(action) =
                                    accessibility::setup_action(&mut self.state, &request)
                                {
                                    self.handle_setup_action(action);
                                } else if let Some(target) =
                                    self.terminal_controls.action(&self.state, &request)
                                {
                                    self.activate_session(target);
                                } else if let Some(action) =
                                    accessibility::settings_action(&mut self.state, &request)
                                {
                                    self.handle_settings_action(action);
                                } else if let Some(action) =
                                    accessibility::workspace_action(&mut self.state, &request)
                                {
                                    self.handle_workspace_form(action);
                                } else if let Some(action) = accessibility::action(
                                    &self.state,
                                    &request,
                                    &self.terminal_controls,
                                ) {
                                    self.handle_ui_action(action);
                                }
                            }
                        }
                    }
                }
                return;
            }
        };
        self.handle_daemon_event(event);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        if let (Some(adapter), Some(window)) = (&mut self.accessibility, &self.window) {
            adapter.process_event(window, &event);
        }
        match event {
            WindowEvent::CloseRequested => {
                // Shut down only the client bridge; leave `daedalusd` and running PTYs alive.
                self.bridge.take();
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                if size.width > 0 && size.height > 0 {
                    if let (Some(window), Some(renderer)) =
                        (self.window.as_ref(), self.renderer.as_mut())
                    {
                        renderer.resize(size.width, size.height, window.scale_factor() as f32);
                    }
                    self.reveal_active_workspace();
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
                    self.reveal_active_workspace();
                    self.schedule_resize_flush();
                    self.request_redraw();
                }
            }
            WindowEvent::ModifiersChanged(new_mods) => {
                self.modifiers = new_mods.state();
            }
            WindowEvent::Occluded(hidden) => {
                self.occluded = hidden;
                self.sync_history_visit();
                self.state.recap.focus(self.focused && !hidden);
                if !hidden {
                    self.request_redraw();
                }
            }
            WindowEvent::Focused(true) => {
                self.focused = true;
                self.sync_history_visit();
                self.state.recap.focus(!self.occluded);
                self.request_redraw();
            }
            WindowEvent::Focused(false) => {
                self.focused = false;
                self.sync_history_visit();
                self.state.recap.focus(false);
                self.wheel_scroll = WheelScroll::default();
                self.modifiers = ModifiersState::empty();
                self.key_repeats = KeyRepeatGuard::default();
                self.pointer.cancel_gestures();
            }
            WindowEvent::KeyboardInput {
                event,
                is_synthetic,
                ..
            } => {
                if event.state == ElementState::Released {
                    self.key_repeats.release(event.physical_key);
                } else if is_synthetic {
                    self.key_repeats.suppress(event.physical_key);
                } else {
                    self.key_pressed(
                        event.physical_key,
                        &event.logical_key,
                        event.text.as_deref(),
                        event.repeat,
                    );
                }
            }
            WindowEvent::Ime(Ime::Commit(text)) => self.commit_text(text),
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
                    self.pointer.end_drag();
                }
            },
            WindowEvent::MouseWheel { delta, phase, .. } => {
                if phase == TouchPhase::Started {
                    self.wheel_scroll = WheelScroll::default();
                }
                if phase != TouchPhase::Cancelled {
                    self.handle_mouse_wheel(delta);
                }
                if matches!(phase, TouchPhase::Ended | TouchPhase::Cancelled) {
                    self.wheel_scroll = WheelScroll::default();
                }
            }
            WindowEvent::RedrawRequested => {
                self.sync_history_visit();
                self.expire_toast_if_due(Instant::now());
                self.flush_due_resizes(false);

                let Some(layout) = self.compute_current_layout() else {
                    return;
                };
                let Some(renderer) = self.renderer.as_mut() else {
                    return;
                };

                match renderer.render(&mut self.state, &layout) {
                    Ok(true) if renderer.recap_visible() => self.history.presented(),
                    Ok(_) => {}
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
        self.sync_history_visit();
        if let Some(cmd) = self.history.request(Instant::now())
            && let UiCommand::ReadActivity { token, .. } = cmd
            && !self.send_command(cmd)
        {
            self.history.failed(token);
        }

        if self.accessibility_active
            && let Some(adapter) = &mut self.accessibility
            && let Some(window) = &self.window
        {
            let size = window.inner_size();
            let tree = accessibility::workbench_tree(
                &self.state,
                size.width as f32,
                size.height as f32,
                window.scale_factor() as f32,
                &mut self.terminal_controls,
            );
            if self.last_accessibility_tree.as_ref() != Some(&tree) {
                adapter.update_if_active(|| {
                    self.last_accessibility_tree = Some(tree.clone());
                    tree
                });
            }
        }
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

        if let Some(deadline) = next_deadline
            .into_iter()
            .chain(self.history.deadline())
            .min()
        {
            event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }
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
    let bootstrap_explicit_dir = args.dir.is_some();
    // Without --dir the folder suggestion comes from the daemon (see `folder_suggestion`).
    let initial_dir = args.dir.unwrap_or_default();

    let config = BridgeConfig {
        bootstrap_explicit_dir,
        socket_path,
        auth_token: args.auth_token,
        initial_dir,
        default_agent,
        auto_spawn_daemon: !args.no_auto_spawn,
    };

    let event_loop: EventLoop<AppEvent> = EventLoop::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let mut app = DaedalusApp::new(
        config,
        ui_config,
        Some(proxy),
        &args
            .config
            .unwrap_or_else(daedalus_proto::default_config_file_path),
    );
    app.state.remote_host = remote_host;
    event_loop.run_app(&mut app)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precision_scrolling_moves_by_rendered_rows_and_keeps_fractional_remainders_local() {
        let pane = ScrollTarget::Pane(PaneId::new());
        let other = ScrollTarget::Pane(PaneId::new());
        let pixels = |y| MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(0.0, y));
        let mut wheel = WheelScroll::default();

        assert_eq!(wheel.steps(pane, pixels(1.0), 20.0), 0);
        assert_eq!(wheel.steps(pane, pixels(49.0), 20.0), 2);
        assert_eq!(wheel.steps(pane, pixels(-30.0), 20.0), -1);
        assert_eq!(wheel.steps(pane, pixels(19.0), 20.0), 0);
        assert_eq!(wheel.steps(other, pixels(1.0), 20.0), 0);
        assert_eq!(wheel.steps(ScrollTarget::Sidebar, pixels(60.0), 120.0), 0);
        assert_eq!(wheel.steps(ScrollTarget::Sidebar, pixels(60.0), 120.0), 1);

        let stride = picker_row_stride(2.0);
        assert_eq!(
            wheel.steps(ScrollTarget::Presets, pixels(stride as f64 / 2.0), stride),
            0
        );
        assert_eq!(
            wheel.steps(ScrollTarget::Actions, pixels(stride as f64 / 2.0), stride),
            0
        );
        assert_eq!(
            wheel.steps(ScrollTarget::Actions, pixels(stride as f64 / 2.0), stride),
            1
        );

        // These independent dialogs must each accumulate their own first complete step.
        for target in [
            ScrollTarget::Settings,
            ScrollTarget::Recipients,
            ScrollTarget::Actions,
        ] {
            assert_eq!(wheel.steps(target, pixels(stride as f64 * 0.75), stride), 0);
        }

        let lines = |y| MouseScrollDelta::LineDelta(0.0, y);
        assert_eq!(wheel.steps(pane, lines(0.25), 20.0), 0);
        assert_eq!(wheel.steps(pane, lines(0.25), 20.0), 1);
        assert_eq!(wheel.steps(other, lines(-1.0), 40.0), -3);
    }
}

#[cfg(test)]
mod controller_tests;
