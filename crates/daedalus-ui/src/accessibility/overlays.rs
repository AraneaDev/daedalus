use super::{CLOSE, NEW_WORKSPACE, SETTINGS, SETUP_REVIEW, TerminalControls, WORKTREE};
use super::{close_workspace, settings, setup_review, workspace_form, worktree};
use accesskit::{Node, NodeId};
use daedalus_ui::state::AppState;

pub(super) fn append(
    state: &AppState,
    (id, mut node): (NodeId, Node),
    nodes: &mut Vec<(NodeId, Node)>,
    width: f32,
    height: f32,
    scale: f32,
    controls: &mut TerminalControls,
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
        _ => id,
    };
    nodes.push((id, node));
    focus
}
