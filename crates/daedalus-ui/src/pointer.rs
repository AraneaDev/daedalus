use super::DaedalusApp;
use daedalus_proto::{PaneId, PaneRole};
use daedalus_ui::input::UiAction;
use daedalus_ui::layout::{HitTarget, Rect, hit_test};
use daedalus_ui::renderer::{
    action_palette_layout, close_modal_cancel_rect, close_modal_compose_rect,
    close_modal_confirm_rect, close_modal_force_rect, close_modal_worktree_rect,
    close_workspace_modal_rect, picker_item_rect, preset_picker_layout, search_bar_rect,
    worktree_button_rects, worktree_modal_rect,
};
use daedalus_ui::selection::{CellPos, Selection, select_word_at};
use daedalus_ui::settings::SettingsAction;
use daedalus_ui::state::{AgentPreset, AppState, PaneViewState};
use daedalus_ui::workspace_form::WorkspaceFormAction;
use std::time::{Duration, Instant};
use winit::keyboard::ModifiersState;

const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(400);

#[derive(Default)]
pub(super) struct PointerState {
    pub(super) position: (f32, f32),
    drag_anchor: Option<(PaneRole, PaneId, CellPos)>,
    last_click: Option<(Instant, PaneRole, u16, u16)>,
}

impl PointerState {
    pub(super) fn end_drag(&mut self) {
        self.drag_anchor = None;
    }

    pub(super) fn cancel_gestures(&mut self) {
        self.end_drag();
        self.last_click = None;
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
    let effect = if let Some(choices) = &mut state.failure_recipients {
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
    } else if state.workspace_form.open {
        OverlayClick::WorkspaceForm(state.workspace_form.click(x, y, width, height, scale))
    } else if state.settings.open {
        OverlayClick::Settings(state.settings.click(x, y, width, height, scale))
    } else if state.action_palette.open {
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
    } else if state.picker.open {
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
    } else if state.worktree_modal.open {
        let modal = worktree_modal_rect(width, height, scale);
        OverlayClick::Action(worktree_modal_click(
            modal,
            scale,
            state.can_create_worktree(),
            x,
            y,
        ))
    } else if state.close_modal.open {
        let modal = close_workspace_modal_rect(width, height, scale);
        OverlayClick::Action(close_modal_click(modal, &state.close_modal, scale, x, y))
    } else {
        return None;
    };
    Some(effect)
}

impl DaedalusApp {
    pub(super) fn handle_left_mouse_down(&mut self) {
        let (Some(window), Some(renderer)) = (&self.window, &self.renderer) else {
            return;
        };
        let size = window.inner_size();
        let scale_factor = window.scale_factor() as f32;
        let cell = renderer.cell_size();
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
            Some(OverlayClick::WorkspaceForm(action)) => self.handle_workspace_form(action),
            Some(OverlayClick::Settings(action)) => self.handle_settings_action(action),
            Some(OverlayClick::ConfirmAction) => self.confirm_workspace_action(),
            Some(OverlayClick::ApplyPreset(preset)) => self.apply_agent_preset(preset, false),
            Some(OverlayClick::Action(action)) => self.handle_ui_action(action),
            None => self.handle_viewport_click(scale_factor, cell),
        }
    }

    fn handle_viewport_click(&mut self, scale_factor: f32, (cell_w, cell_h): (f32, f32)) {
        let (x, y) = self.pointer.position;
        let Some(layout) = self.compute_current_layout() else {
            return;
        };
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
            HitTarget::SidebarPageUp | HitTarget::SidebarPageDown => {
                if let Some(list) = self.sidebar_list() {
                    let page = list.visible.len() as isize;
                    self.scroll_sidebar(if target == HitTarget::SidebarPageUp {
                        -page
                    } else {
                        page
                    });
                }
            }
            HitTarget::ToggleSidebar => self.handle_ui_action(UiAction::ToggleSidebar),
            HitTarget::SidebarWorkspace(idx) => {
                if let Some(ws) = self.state.workspaces.get(idx) {
                    let ws_id = ws.id;
                    if self.state.active_workspace_id != Some(ws_id) {
                        self.state.select_workspace(ws_id);
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
            HitTarget::CloseSession(_) | HitTarget::SelectSession(_) => {
                self.activate_session(target)
            }
            HitTarget::SessionTab(role) => self.handle_ui_action(UiAction::FocusRole(role)),
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
                self.state.focus_clicked_pane(role);
                if is_link_click_modifier(self.modifiers) {
                    self.open_clicked_link(role, col, row);
                    self.request_redraw();
                    return;
                }
                if let Some(pane_id) = self.state.pane_id_for_role(role)
                    && let Some(pane) = self.state.panes.get_mut(&pane_id)
                {
                    self.pointer.select_at(pane, col, row, Instant::now());
                }
                self.request_redraw();
            }
            HitTarget::None => {}
        }
    }

    pub(super) fn handle_cursor_moved(&mut self, x: f32, y: f32) {
        self.pointer.position = (x, y);

        let Some((drag_role, drag_pane_id, anchor)) = self.pointer.drag_anchor else {
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
            agent_state: None,
        });
        pane.viewport_rows.push(GridRow {
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
            let field = daedalus_ui::renderer::worktree_input_rect(modal, dpi);
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
        use daedalus_ui::state::CloseWorkspaceModalState;
        use winit::keyboard::{Key, NamedKey};
        let mut state = CloseWorkspaceModalState {
            open: true,
            is_dirty: true,
            is_worktree: true,
            remove_worktree: true,
            ..Default::default()
        };
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
