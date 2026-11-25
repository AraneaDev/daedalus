use super::{SETUP_REVIEW, bounds, editable_field, named};
use accesskit::{Action, ActionData, ActionRequest, Live, Node, NodeId, Role, TreeId};
use daedalus_ui::layout::Rect;
use daedalus_ui::setup_review::{Control, SetupAction, SetupReview, role_label};
use daedalus_ui::state::{ActiveOverlay, AppState};

fn id(review: &SetupReview, c: Control) -> NodeId {
    let offset = match c {
        Control::Command(i) => i as u64,
        Control::Launch(i) => 3 + i as u64,
        Control::Agent => 6,
        Control::Tool => 7,
        Control::Install => 8,
        Control::Guide => 9,
        Control::Dependencies => 10,
        Control::Back => 11,
        Control::Cancel => 12,
        Control::Check => 13,
        Control::Open => 14,
    };
    let generation = if matches!(c, Control::Tool | Control::Install | Control::Guide) {
        review.tool_revision() & 0xfffff
    } else {
        0
    };
    NodeId((1 << 57) | ((review.revision() & 0x3fffffff) << 26) | (generation << 6) | offset)
}

pub(super) fn fields(
    review: &SetupReview,
    dialog: &mut Node,
    nodes: &mut Vec<(NodeId, Node)>,
    modal: Rect,
) -> NodeId {
    dialog.set_bounds(bounds(modal));
    for (c, rect) in review.controls(modal) {
        let node_id = id(review, c);
        let mut node = match c {
            Control::Command(i) => {
                let mut n = editable_field(
                    &format!("{} command", role_label(i)),
                    review.command(i),
                    NodeId(node_id.0 + 32),
                    review.selection(i),
                    nodes,
                );
                n.clear_required();
                n.set_description(review.status(i));
                n
            }
            _ => {
                let label = if let Control::Launch(i) = c {
                    format!("{} · {}", role_label(i), review.label(c))
                } else {
                    review.label(c)
                };
                let mut n = named(Role::Button, label);
                n.add_action(Action::Click);
                n
            }
        };
        if review.enabled(c) {
            node.add_action(Action::Focus);
        } else {
            node.set_disabled();
            node.remove_action(Action::Click);
            node.remove_action(Action::SetValue);
        }
        node.set_bounds(bounds(rect));
        dialog.push_child(node_id);
        nodes.push((node_id, node));
    }
    if !review.feedback.is_empty() {
        let status_id = NodeId(id(review, Control::Open).0 + 40);
        let mut status = named(Role::Status, &review.feedback);
        status.set_live(Live::Polite);
        dialog.push_child(status_id);
        nodes.push((status_id, status));
    }
    if review.enabled(review.focus) {
        id(review, review.focus)
    } else {
        SETUP_REVIEW
    }
}

pub(crate) fn setup_action(state: &mut AppState, request: &ActionRequest) -> Option<SetupAction> {
    if request.target_tree != TreeId::ROOT
        || state.active_overlay() != Some(ActiveOverlay::SetupReview)
    {
        return None;
    }
    let review = &mut state.setup_review;
    let (control, _) = review
        .controls(Rect::new(0., 0., 1., 1.))
        .into_iter()
        .find(|(c, _)| id(review, *c) == request.target_node && review.enabled(*c))?;
    match request.action {
        Action::Focus => review.set_focus(control),
        Action::Click if !matches!(control, Control::Command(_)) => {
            return Some(review.activate(control));
        }
        Action::SetValue => {
            if let (Control::Command(i), Some(ActionData::Value(value))) = (control, &request.data)
            {
                review.set_value(i, value);
            } else {
                return None;
            }
        }
        _ => return None,
    }
    Some(SetupAction::None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use daedalus_proto::{PaneRole, SetupInspection};

    #[test]
    fn screen_reader_can_edit_role_commands_and_stale_install_requests_are_ignored() {
        let mut state = AppState::new();
        state.setup_review.begin(
            "App".into(),
            "/app".into(),
            SetupInspection {
                commands: vec![(PaneRole::Agent, vec!["codex".into()])],
                available: vec!["npm".into()],
                host: "daemon".into(),
                platform: "linux".into(),
                dependency_install: None,
            },
        );
        let mut dialog = named(Role::Dialog, "Review environment");
        let mut nodes = Vec::new();
        let focus = fields(
            &state.setup_review,
            &mut dialog,
            &mut nodes,
            Rect::new(0., 0., 640., 500.),
        );
        assert_eq!(focus, id(&state.setup_review, Control::Open));
        let field = id(&state.setup_review, Control::Command(0));
        assert!(nodes.iter().any(|(id, n)| *id == field
            && n.label() == Some("Coding agent command")
            && n.supports_action(Action::SetValue)));
        let install = ActionRequest {
            target_tree: TreeId::ROOT,
            target_node: id(&state.setup_review, Control::Install),
            action: Action::Click,
            data: None,
        };
        assert!(matches!(
            setup_action(&mut state, &install),
            Some(SetupAction::Install(_))
        ));
        let edit = ActionRequest {
            target_tree: TreeId::ROOT,
            target_node: field,
            action: Action::SetValue,
            data: Some(ActionData::Value("claude".into())),
        };
        assert_eq!(setup_action(&mut state, &edit), Some(SetupAction::None));
        assert_eq!(state.setup_review.command(0), "claude");
        assert_eq!(setup_action(&mut state, &install), None);
    }
}
