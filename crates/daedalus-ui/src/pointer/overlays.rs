use super::{DaedalusApp, ViewportMetrics};
use daedalus_ui::input::UiAction;
use daedalus_ui::layout::Rect;
use daedalus_ui::layout::overlays::{
    action_palette_layout, close_modal_cancel_rect, close_modal_compose_rect,
    close_modal_confirm_rect, close_modal_force_rect, close_modal_worktree_rect,
    close_workspace_modal_rect, picker_item_rect, preset_picker_layout, search_controls,
    worktree_button_rects, worktree_modal_rect,
};
use daedalus_ui::settings::SettingsAction;
use daedalus_ui::state::{ActiveOverlay, AgentPreset, AppState};
use daedalus_ui::workspace_form::WorkspaceFormAction;

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
            state.palette_len(),
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

pub(super) fn search_click(bar: Rect, x: f32, y: f32) -> Option<UiAction> {
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

impl DaedalusApp {
    pub(super) fn dispatch_overlay_click(&mut self, metrics: ViewportMetrics) -> bool {
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
            None => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daedalus_ui::{input::translate_key, state::ConnectionState};
    use winit::keyboard::ModifiersState;

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
}
