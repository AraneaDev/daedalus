use super::{CLOSE, TerminalControls, bounds, named};
use accesskit::{Action, ActionRequest, Node, NodeId, Role, Toggled};
use daedalus_ui::input::UiAction;
use daedalus_ui::layout::Rect;
use daedalus_ui::layout::overlays::{
    close_modal_cancel_rect, close_modal_compose_rect, close_modal_confirm_rect,
    close_modal_force_rect, close_modal_worktree_rect, close_workspace_modal_rect,
};
use daedalus_ui::state::{AppState, CloseWorkspaceModalState, ConnectionState};

#[derive(Clone, Copy)]
enum Control {
    Compose,
    Worktree,
    Force,
    Cancel,
    Confirm,
}

impl Control {
    const ALL: [Self; 5] = [
        Self::Compose,
        Self::Worktree,
        Self::Force,
        Self::Cancel,
        Self::Confirm,
    ];

    fn id(self, base: NodeId) -> NodeId {
        NodeId(base.0 + self as u64)
    }

    fn rect(self, modal: &CloseWorkspaceModalState, dialog: Rect, scale: f32) -> Option<Rect> {
        match self {
            Self::Compose => Some(close_modal_compose_rect(dialog, modal, scale)),
            Self::Worktree => close_modal_worktree_rect(dialog, modal, scale),
            Self::Force => close_modal_force_rect(dialog, modal, scale),
            Self::Cancel => Some(close_modal_cancel_rect(dialog, scale)),
            Self::Confirm => Some(close_modal_confirm_rect(dialog, scale)),
        }
    }

    fn enabled(self, state: &AppState) -> bool {
        let modal = &state.close_modal;
        !modal.submitting
            && match self {
                Self::Worktree => modal.is_worktree,
                Self::Force => modal.is_worktree && modal.is_dirty && modal.remove_worktree,
                Self::Confirm => {
                    state.connection == ConnectionState::Connected && modal.workspace_id.is_some()
                }
                Self::Compose | Self::Cancel => true,
            }
    }

    fn node(self, modal: &CloseWorkspaceModalState) -> Node {
        let (label, checked) = match self {
            Self::Compose => ("Stop Docker Compose", Some(modal.compose_down)),
            Self::Worktree => ("Remove Git worktree", Some(modal.remove_worktree)),
            Self::Force => ("Force removal", Some(modal.force_remove_worktree)),
            Self::Cancel => ("Cancel", None),
            Self::Confirm => ("Confirm close", None),
        };
        let Some(checked) = checked else {
            return named(Role::Button, label);
        };
        let mut node = named(Role::CheckBox, label);
        node.set_toggled(if checked {
            Toggled::True
        } else {
            Toggled::False
        });
        node
    }

    fn action(self) -> UiAction {
        match self {
            Self::Compose => UiAction::CloseModalToggleCompose,
            Self::Worktree => UiAction::CloseModalToggleWorktree,
            Self::Force => UiAction::CloseModalToggleForce,
            Self::Cancel => UiAction::CloseModalCancel,
            Self::Confirm => UiAction::CloseModalConfirm,
        }
    }
}

pub(super) fn append_fields(
    state: &AppState,
    dialog: &mut Node,
    nodes: &mut Vec<(NodeId, Node)>,
    width: f32,
    height: f32,
    scale: f32,
    controls: &mut TerminalControls,
) -> NodeId {
    let modal = &state.close_modal;
    let base = controls.register_close_modal(modal.session());
    let rect = close_workspace_modal_rect(width, height, scale);
    dialog.set_bounds(bounds(rect));
    for control in Control::ALL {
        let Some(rect) = control.rect(modal, rect, scale) else {
            continue;
        };
        let id = control.id(base);
        let mut node = control.node(modal);
        node.set_bounds(bounds(rect));
        if control.enabled(state) {
            node.add_action(Action::Click);
        } else {
            node.set_disabled();
        }
        dialog.push_child(id);
        nodes.push((id, node));
    }
    if modal.submitting {
        CLOSE
    } else {
        Control::Cancel.id(base)
    }
}

pub(super) fn action(
    state: &AppState,
    request: &ActionRequest,
    controls: &TerminalControls,
) -> Option<UiAction> {
    if request.action != Action::Click {
        return None;
    }
    let base = controls.close_modal_base(state.close_modal.session())?;
    Control::ALL.into_iter().find_map(|control| {
        (request.target_node == control.id(base) && control.enabled(state))
            .then(|| control.action())
    })
}
