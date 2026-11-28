#![cfg_attr(windows, windows_subsystem = "windows")]

mod accessibility;
mod daemon_events;
mod dialog_actions;
mod pointer;
mod setup;
mod terminal_commands;
mod terminal_search;
mod viewport;
mod window_events;
mod workspace_settings;
mod workspace_sharing;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser;
use daedalus_proto::{PaneId, PaneRole, WorkspaceId, default_socket_path};
use daedalus_ui::bridge::{BridgeConfig, UiCommand, UiEvent, UiEventSink};
use daedalus_ui::config::UiConfig;
use daedalus_ui::input::{
    KeyRepeatGuard, UiAction, filter_modal_text, translate_overlay_key, windows_terminal_shortcut,
};
use daedalus_ui::layout::overlays::picker_row_stride;
use daedalus_ui::layout::{HitTarget, hit_test};
use daedalus_ui::renderer::GpuRenderer;
use daedalus_ui::state::{ActiveOverlay, AgentPreset, AppState, ConnectionState};
use tracing_subscriber::EnvFilter;
use viewport::ViewportMetrics;
use winit::event::MouseScrollDelta;
use winit::event_loop::{EventLoop, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::window::Window;

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

struct DaedalusApp {
    config: BridgeConfig,
    ui_config: UiConfig,
    project_setups: daedalus_ui::project_setups::ProjectSetups,
    widget_directory: PathBuf,
    watched_workbench: Option<WorkspaceId>,
    layout_saves: std::collections::HashMap<WorkspaceId, workspace_sharing::LayoutSave>,
    layout_workspace: Option<WorkspaceId>,
    seeding_workbench: std::collections::HashSet<WorkspaceId>,
    widget_refresh_at: Option<Instant>,
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

/// Sound is delegated to the desktop and respects the user's system sound settings.
fn play_notification_ping() {
    #[cfg(target_os = "linux")]
    let command = ("canberra-gtk-play", &["--id", "message-new-instant"][..]);
    #[cfg(target_os = "macos")]
    let command = ("afplay", &["/System/Library/Sounds/Ping.aiff"][..]);
    #[cfg(target_os = "windows")]
    {
        #[link(name = "user32")]
        unsafe extern "system" {
            fn MessageBeep(kind: u32) -> i32;
        }
        // SAFETY: MessageBeep takes a predefined sound identifier and no pointers.
        unsafe {
            MessageBeep(0x40);
        }
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    std::thread::spawn(move || {
        if let Err(error) = std::process::Command::new(command.0)
            .args(command.1)
            .status()
        {
            tracing::debug!(%error, "notification sound unavailable");
        }
    });
}

impl DaedalusApp {
    fn new(
        config: BridgeConfig,
        ui_config: UiConfig,
        proxy: Option<EventLoopProxy<AppEvent>>,
        profile: &std::path::Path,
    ) -> Self {
        let mut state = AppState::new();
        state.reduced_motion = ui_config.reduced_motion;
        let widget_directory = profile.with_extension("widgets");
        state.workspace_widgets = daedalus_ui::workspace_widgets::load_all(&widget_directory);
        Self {
            project_setups: Default::default(),
            widget_directory,
            watched_workbench: None,
            layout_saves: Default::default(),
            layout_workspace: None,
            seeding_workbench: Default::default(),
            widget_refresh_at: None,
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
            state,
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
        if visible
            && let Some(id) = self.state.focused_pane_id()
            && let Some(pane) = self.state.panes.get_mut(&id)
        {
            pane.unread_attention = false;
        }
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
        if let Some(next) = self.state.adjacent_workspace(delta) {
            if self.state.active_workspace_id == Some(next) {
                self.ensure_active_workspace_panes();
                self.request_redraw();
            } else {
                self.activate_workspace(next);
            }
        }
    }

    fn activate_workspace(&mut self, id: WorkspaceId) {
        if self.state.active_workspace_id == Some(id)
            || !self
                .state
                .workspaces
                .iter()
                .any(|workspace| workspace.id == id)
        {
            return;
        }
        self.state.select_workspace(id);
        self.prepare_active_workbench_layout();
        self.state.close_search();
        self.ensure_active_workspace_panes();
        self.reveal_active_workspace();
        self.schedule_resize_flush();
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
                self.prepare_active_workbench_layout();
                self.state.zoomed = !self.state.zoomed;
                self.save_workbench_layout();
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
            UiAction::RunTests => self.run_saved_tests(),
            UiAction::OpenSessions => self.open_sessions(),
            UiAction::RunCargo(preset) => self.run_cargo_preset(preset),
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
            UiAction::SelectWorkspace(id) => self.activate_workspace(id),
            UiAction::CloseWorkspaceFromSidebar(id) => self.open_close_modal_for_workspace(id),
            UiAction::PreviewImage => self.preview_image(),
            UiAction::CopySelection => self.copy_focused_selection(),
            UiAction::PasteClipboard => self.paste_clipboard_to_focused(),
            UiAction::ForwardFailureToAgent => self.forward_runner_failure_to_agent(),
            UiAction::FontZoomIn => self.apply_font_size_delta(1.0),
            UiAction::FontZoomOut => self.apply_font_size_delta(-1.0),
            UiAction::FontZoomReset => self.set_font_size(self.ui_config.font_size),
            UiAction::ToggleActionPalette
            | UiAction::ActionPaletteUp
            | UiAction::ActionPaletteDown
            | UiAction::ActionPaletteConfirm
            | UiAction::ActionPaletteCancel
            | UiAction::ToggleAgentPicker
            | UiAction::ToggleWorktreeModal
            | UiAction::OpenCloseWorkspaceModal
            | UiAction::PickerUp
            | UiAction::PickerDown
            | UiAction::PickerConfirm
            | UiAction::PickerAdd
            | UiAction::PickerCancel
            | UiAction::WorktreeModalText(_)
            | UiAction::WorktreeModalSetText(_)
            | UiAction::WorktreeModalBackspace
            | UiAction::WorktreeModalConfirm
            | UiAction::WorktreeModalCancel
            | UiAction::CloseModalToggleCompose
            | UiAction::CloseModalToggleWorktree
            | UiAction::CloseModalToggleForce
            | UiAction::CloseModalConfirm
            | UiAction::CloseModalCancel => self.handle_dialog_action(action),
            UiAction::ToggleSearch
            | UiAction::PasteSearchQuery
            | UiAction::SearchText(_)
            | UiAction::SearchBackspace
            | UiAction::SearchToggleCase
            | UiAction::SearchNext
            | UiAction::SearchPrev
            | UiAction::SearchCancel => self.handle_search_action(action),
            UiAction::CloseTerminal
            | UiAction::ScrollPageUp
            | UiAction::ScrollPageDown
            | UiAction::ScrollToBottom
            | UiAction::SendBytes(_) => self.handle_terminal_action(action),
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
        let overlay = self.state.active_overlay();
        if overlay == Some(ActiveOverlay::FailureRecipients)
            && let Some(choices) = &mut self.state.failure_recipients
        {
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
        if matches!(
            overlay,
            Some(
                ActiveOverlay::SetupReview | ActiveOverlay::WorkspaceForm | ActiveOverlay::Settings
            )
        ) {
            if self
                .key_repeats
                .form_press(physical_key, logical_key, repeat)
            {
                if overlay == Some(ActiveOverlay::SetupReview) {
                    self.setup_key(logical_key);
                } else if overlay == Some(ActiveOverlay::WorkspaceForm) {
                    self.workspace_form_key(logical_key);
                } else {
                    self.settings_key(logical_key);
                }
            }
            return;
        }
        if overlay.is_none() && self.handle_recap_key(logical_key, repeat) {
            self.key_repeats.suppress(physical_key);
            return;
        }
        let terminal_shortcut = if cfg!(target_os = "windows") && overlay.is_none() {
            windows_terminal_shortcut(logical_key, self.modifiers)
        } else {
            None
        };
        let action = terminal_shortcut.unwrap_or_else(|| {
            translate_overlay_key(
                logical_key,
                text,
                self.modifiers,
                overlay,
                cfg!(target_os = "macos"),
            )
        });
        let action = self.key_repeats.press(physical_key, repeat, action);
        self.handle_ui_action(action);
    }

    fn commit_text(&mut self, text: String) {
        match self.state.active_overlay() {
            Some(ActiveOverlay::SetupReview) => {
                self.state.setup_review.insert_text(&text);
                self.request_redraw();
            }
            Some(ActiveOverlay::WorkspaceForm) => {
                self.state.workspace_form.insert_text(&text);
                self.request_redraw();
            }
            Some(ActiveOverlay::Settings) => {
                self.state.settings.insert_text(&text);
                self.request_redraw();
            }
            Some(ActiveOverlay::Worktree) => {
                if let Some(cleaned) = filter_modal_text(&text, false) {
                    self.handle_ui_action(UiAction::WorktreeModalText(cleaned));
                }
            }
            Some(ActiveOverlay::Search) => {
                if let Some(cleaned) = filter_modal_text(&text, true) {
                    self.handle_ui_action(UiAction::SearchText(cleaned));
                }
            }
            Some(
                ActiveOverlay::FailureRecipients
                | ActiveOverlay::AgentPicker
                | ActiveOverlay::ActionPalette
                | ActiveOverlay::CloseWorkspace,
            ) => {}
            None if self.state.recap.keyboard_row.is_none()
                && !text.is_empty()
                && !text.chars().all(char::is_control) =>
            {
                self.handle_ui_action(UiAction::SendBytes(text.into_bytes()));
            }
            None => {}
        }
    }

    fn handle_recap_key(&mut self, key: &winit::keyboard::Key, repeat: bool) -> bool {
        use winit::keyboard::{Key, NamedKey};
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
            HitTarget::CustomWidget(index) => self.activate_widget(index),
            HitTarget::WidgetElementAction { widget, element } => {
                self.activate_widget_element(widget, element)
            }
            HitTarget::Widget(role) => {
                if let Some(id) = self.state.pane_id_for_role(role) {
                    if role == PaneRole::Runner && self.state.widget_diagnostic().is_some() {
                        if let Some(pane) = self.state.panes.get_mut(&id) {
                            pane.show_diagnostic = !pane.show_diagnostic;
                        }
                        self.request_redraw();
                    } else {
                        self.activate_session(HitTarget::SelectSession(id));
                    }
                }
            }
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

    fn scroll_overlay(
        &mut self,
        overlay: ActiveOverlay,
        metrics: Option<ViewportMetrics>,
        delta: MouseScrollDelta,
    ) {
        let scale = metrics.map_or(1., |metrics| metrics.scale_factor);
        match overlay {
            ActiveOverlay::FailureRecipients => {
                if let Some(choices) = &mut self.state.failure_recipients {
                    let steps = self.wheel_scroll.steps(
                        ScrollTarget::Recipients,
                        delta,
                        picker_row_stride(scale),
                    );
                    choices.advance(steps.saturating_neg());
                    self.request_redraw();
                }
            }
            ActiveOverlay::Settings => {
                let steps = self.wheel_scroll.steps(ScrollTarget::Settings, delta, 30.);
                if steps != 0 {
                    self.state.settings.scroll(steps as f32);
                    self.request_redraw();
                }
            }
            ActiveOverlay::ActionPalette | ActiveOverlay::AgentPicker if metrics.is_some() => {
                let (target, count, selected) = if overlay == ActiveOverlay::AgentPicker {
                    (
                        ScrollTarget::Presets,
                        AgentPreset::ALL.len(),
                        &mut self.state.picker.selected_index,
                    )
                } else {
                    (
                        ScrollTarget::Actions,
                        self.state.palette_len(),
                        &mut self.state.action_palette.selected_index,
                    )
                };
                let steps = self
                    .wheel_scroll
                    .steps(target, delta, picker_row_stride(scale));
                let next = selected
                    .saturating_add_signed(steps.saturating_neg())
                    .min(count.saturating_sub(1));
                if next != *selected {
                    *selected = next;
                    self.request_redraw();
                }
            }
            _ => self.wheel_scroll = WheelScroll::default(),
        }
    }

    fn scroll_at(&mut self, metrics: Option<ViewportMetrics>, delta: MouseScrollDelta) {
        if let Some(overlay) = self
            .state
            .active_overlay()
            .filter(|overlay| *overlay != ActiveOverlay::Search)
        {
            self.scroll_overlay(overlay, metrics, delta);
            return;
        }
        let Some(metrics) = metrics else {
            return;
        };
        let scale_factor = metrics.scale_factor;
        let (cell_w, cell_h) = metrics.cell;
        let layout = self.layout_for(metrics);

        if layout
            .preview_panel(&self.state, scale_factor)
            .is_some_and(|rect| rect.contains(self.pointer.position.0, self.pointer.position.1))
        {
            return;
        }
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
    fn wheel_targets_only_the_frontmost_overlay() {
        let profile = tempfile::tempdir().unwrap();
        let mut app = DaedalusApp::new(
            BridgeConfig {
                socket_path: profile.path().join("daemon.sock"),
                auth_token: None,
                initial_dir: PathBuf::new(),
                bootstrap_explicit_dir: false,
                default_agent: AgentPreset::Shell,
                auto_spawn_daemon: false,
            },
            UiConfig::default(),
            None,
            &profile.path().join("config.toml"),
        );
        let metrics = ViewportMetrics {
            size: (1200, 720).into(),
            scale_factor: 1.,
            cell: (8., 16.),
        };
        let scroll = MouseScrollDelta::LineDelta(0., -1.);
        app.state.settings.open = true;
        app.state.workspace_form.open = true;
        app.scroll_at(Some(metrics), scroll);
        assert!(app.wheel_scroll.target.is_none());

        app.state.workspace_form.open = false;
        app.state.settings.open = false;
        app.state.picker.open = true;
        app.state.action_palette.open = true;
        app.scroll_at(Some(metrics), scroll);
        assert!(app.wheel_scroll.target == Some(ScrollTarget::Actions));
        assert_eq!(app.state.picker.selected_index, 0);
    }

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
