use super::{
    ACTIONS, CLOSE, NEW_WORKSPACE, PICKER, SEARCH, SETTINGS, SETUP_REVIEW, TerminalControls,
    WORKTREE,
};
use super::{close_workspace, search, settings, setup_review, workspace_form, worktree};
use accesskit::{Node, NodeId};
use daedalus_ui::layout::Rect;
use daedalus_ui::state::AppState;

pub(super) fn append(
    state: &AppState,
    (id, mut node): (NodeId, Node),
    nodes: &mut Vec<(NodeId, Node)>,
    (width, height, scale): (f32, f32, f32),
    controls: &mut TerminalControls,
    search_bar: Option<Rect>,
) -> NodeId {
    let focus = match id {
        SETUP_REVIEW => setup_review::fields(
            &state.setup_review,
            &mut node,
            nodes,
            daedalus_ui::setup_review::modal_rect(width, height, scale),
        ),
        SETTINGS => settings::fields(
            &state.settings,
            &mut node,
            nodes,
            state.settings.modal_rect(width, height, scale),
        ),
        NEW_WORKSPACE => workspace_form::workspace_fields(
            &state.workspace_form,
            &mut node,
            nodes,
            state.workspace_form.modal_rect(width, height, scale),
        ),
        WORKTREE => worktree::append_fields(state, &mut node, nodes, width, height, scale),
        CLOSE => {
            close_workspace::append_fields(state, &mut node, nodes, width, height, scale, controls)
        }
        PICKER | ACTIONS => {
            controls.append_choices(state, id, &mut node, nodes, (width, height, scale))
        }
        SEARCH => {
            search::append(state, &mut node, nodes, search_bar, controls);
            id
        }
        _ => id,
    };
    nodes.push((id, node));
    focus
}
