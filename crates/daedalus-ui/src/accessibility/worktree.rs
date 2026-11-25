use super::{BRANCH_TEXT, VALIDATION, bounds, named, text_run, worktree_controls};
use accesskit::{Action, Invalid, Live, Node, NodeId, Role, TextPosition, TextSelection};
use daedalus_ui::layout::overlays::{
    worktree_button_rects, worktree_input_rect, worktree_modal_rect,
};
use daedalus_ui::state::AppState;

pub(super) fn append_fields(
    state: &AppState,
    node: &mut Node,
    nodes: &mut Vec<(NodeId, Node)>,
    width: f32,
    height: f32,
    scale: f32,
) -> NodeId {
    let (branch, cancel, create) = worktree_controls(state);
    let modal = worktree_modal_rect(width, height, scale);
    node.set_bounds(bounds(modal));
    let mut field = named(Role::TextInput, "Branch name");
    field.set_bounds(bounds(worktree_input_rect(modal, scale)));
    let run = text_run(&state.worktree_modal.branch_input);
    let caret = TextPosition {
        node: BRANCH_TEXT,
        character_index: run.character_lengths().len(),
    };
    field.push_child(BRANCH_TEXT);
    field.set_text_selection(TextSelection {
        anchor: caret,
        focus: caret,
    });
    nodes.push((BRANCH_TEXT, run));
    field.set_required();
    field.add_action(Action::SetValue);
    if state.worktree_modal.missing_branch() {
        field.set_invalid(Invalid::True);
        field.set_error_message(VALIDATION);
    }
    node.push_child(branch);
    nodes.push((branch, field));
    let (cancel_rect, create_rect) = worktree_button_rects(modal, scale);
    for (button_id, name, enabled, rect) in [
        (cancel, "Cancel", true, cancel_rect),
        (create, "Create", state.can_create_worktree(), create_rect),
    ] {
        let mut button = named(Role::Button, name);
        button.set_bounds(bounds(rect));
        if enabled {
            button.add_action(Action::Click);
        } else {
            button.set_disabled();
        }
        node.push_child(button_id);
        nodes.push((button_id, button));
    }
    if state.worktree_modal.missing_branch() {
        let mut alert = named(Role::Alert, "Enter a branch name");
        alert.set_live(Live::Assertive);
        node.push_child(VALIDATION);
        nodes.push((VALIDATION, alert));
    }
    branch
}
