use super::{AppEvent, DaedalusApp, EventLoopProxySink, TOAST_TTL, WheelScroll, accessibility};
use daedalus_ui::bridge::{DaemonBridge, UiCommand};
use daedalus_ui::input::{KeyRepeatGuard, UiAction};
use daedalus_ui::renderer::{GpuRenderer, RenderError};
use daedalus_ui::state::ActiveOverlay;
use daedalus_ui::theme::Theme;
use std::sync::Arc;
use std::time::Instant;
use tracing::{error, warn};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, Ime, MouseButton, TouchPhase, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

const INITIAL_WINDOW_WIDTH: f64 = 1440.0;
const INITIAL_WINDOW_HEIGHT: f64 = 900.0;

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

        #[cfg(any(windows, target_os = "linux"))]
        let attrs = {
            let icon = winit::window::Icon::from_rgba(
                include_bytes!("../../../packaging/window-icon.rgba").to_vec(),
                64,
                64,
            )
            .expect("embedded 64px Daedalus window icon");
            #[cfg(windows)]
            {
                use winit::platform::windows::WindowAttributesExtWindows;
                attrs
                    .with_window_icon(Some(icon.clone()))
                    .with_taskbar_icon(Some(icon))
            }
            #[cfg(target_os = "linux")]
            {
                use winit::platform::wayland::WindowAttributesExtWayland;
                attrs
                    .with_name("daedalus", "daedalus")
                    .with_window_icon(Some(icon))
            }
        };

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
                                } else if let Some(action) = self
                                    .terminal_controls
                                    .choice_action(&mut self.state, &request)
                                {
                                    if action == UiAction::Ignore {
                                        self.request_redraw();
                                    } else {
                                        self.handle_ui_action(action);
                                    }
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
                self.handle_left_mouse_up();
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
            WindowEvent::CursorLeft { .. } => self.update_link_hover(None),
            WindowEvent::CursorMoved { position, .. } => {
                self.handle_cursor_moved(position.x as f32, position.y as f32);
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => match state {
                ElementState::Pressed => self.handle_left_mouse_down(),
                ElementState::Released => self.handle_left_mouse_up(),
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
        self.poll_workbench();
        self.sync_history_visit();
        if let Some(cmd) = self.history.request(Instant::now())
            && let UiCommand::ReadActivity { token, .. } = cmd
            && !self.send_command(cmd)
        {
            self.history.failed(token);
        }

        let search_bar = if self.accessibility_active
            && self.state.active_overlay() == Some(ActiveOverlay::Search)
        {
            self.search_bar_bounds()
        } else {
            None
        };
        if self.accessibility_active
            && let Some(adapter) = &mut self.accessibility
            && let Some(window) = &self.window
        {
            let size = window.inner_size();
            let tree = accessibility::workbench_tree_with_search_bar(
                &self.state,
                size.width as f32,
                size.height as f32,
                window.scale_factor() as f32,
                &mut self.terminal_controls,
                search_bar,
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
            .chain(self.widget_refresh_at)
            .min()
        {
            event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }
}
