use super::{bounds, editable_field, named};
use accesskit::{Action, ActionData, ActionRequest, Invalid, Live, Node, NodeId, Role, TreeId};
use daedalus_ui::layout::Rect;
use daedalus_ui::state::{ActiveOverlay, AppState};
use daedalus_ui::workspace_form::{Control, WorkspaceForm, WorkspaceFormAction};

// The high bit keeps form control IDs separate from the worktree dialog's IDs.
pub(super) fn form_node(form: &WorkspaceForm, offset: u64) -> NodeId {
    NodeId((1 << 63) | form.session().wrapping_mul(8) | offset)
}

pub(super) fn control_node(form: &WorkspaceForm, control: Control) -> NodeId {
    match control {
        Control::Name => form_node(form, 0),
        Control::Folder => form_node(form, 1),
        Control::Cancel => form_node(form, 2),
        Control::Save => form_node(form, 3),
        Control::Recent(i) => recent_node(form, i as u64),
    }
}

// Bit 62 next to the form's bit keeps recent rows apart from the form's other nodes.
fn recent_node(form: &WorkspaceForm, offset: u64) -> NodeId {
    NodeId((0b11 << 62) | ((form.session() & ((1 << 56) - 1)) << 6) | offset)
}

fn recent_list_node(form: &WorkspaceForm) -> NodeId {
    recent_node(form, 63)
}

pub(crate) fn workspace_action(
    state: &mut AppState,
    request: &ActionRequest,
) -> Option<WorkspaceFormAction> {
    if state.active_overlay() != Some(ActiveOverlay::WorkspaceForm)
        || request.target_tree != TreeId::ROOT
    {
        return None;
    }
    let form = &mut state.workspace_form;
    let (control, _) = form
        .controls(Rect::new(0., 0., 1., 1.))
        .into_iter()
        .find(|(c, _)| form.enabled(*c) && control_node(form, *c) == request.target_node)?;
    match request.action {
        Action::Focus => {
            if control != form.focus {
                form.set_focus(control);
            }
        }
        Action::Click if form.value(control).is_none() => return Some(form.activate(control)),
        Action::SetValue if form.value(control).is_some() => {
            let Some(ActionData::Value(text)) = &request.data else {
                return None;
            };
            form.set_value(control, text);
        }
        _ => return None,
    }
    Some(WorkspaceFormAction::None)
}

pub(super) fn workspace_fields(
    form: &WorkspaceForm,
    dialog: &mut Node,
    nodes: &mut Vec<(NodeId, Node)>,
    modal: Rect,
) -> NodeId {
    dialog.set_bounds(bounds(modal));
    let error_id = form_node(form, 6);
    if let Some(message) = &form.error {
        let mut alert = named(Role::Alert, message);
        alert.set_live(Live::Assertive);
        dialog.push_child(error_id);
        nodes.push((error_id, alert));
    }
    let list_id = recent_list_node(form);
    let mut list = form.recent_heading(modal).map(|heading| {
        dialog.push_child(list_id);
        let mut list = named(Role::List, "Recent projects");
        let rows = form.controls(modal);
        let last = rows
            .iter()
            .filter(|(c, _)| matches!(c, Control::Recent(_)))
            .map(|(_, rect)| rect.bottom())
            .fold(heading.bottom(), f32::max);
        list.set_bounds(bounds(Rect::new(
            heading.x,
            heading.y,
            heading.width,
            last - heading.y,
        )));
        list
    });
    for (control, rect) in form.controls(modal) {
        let id = control_node(form, control);
        let label = match control {
            Control::Name if form.name_is_fixed() => "Workspace name",
            Control::Name => "Name (optional)",
            Control::Folder => "Folder",
            Control::Cancel if form.submitting() => "Dismiss",
            Control::Cancel => "Cancel",
            Control::Save => "Review environment",
            Control::Recent(_) => "",
        };
        let mut node = if let Control::Recent(i) = control {
            let project = &form.recent()[i];
            let mut node = named(
                Role::ListItem,
                format!("{}, {}", project.name, project.root_dir.display()),
            );
            node.add_action(Action::Click);
            node
        } else if let Some(value) = form.value(control) {
            let offset = if control == Control::Name { 0 } else { 1 };
            let run_id = form_node(form, offset + 4);
            let mut node = editable_field(label, value, run_id, form.selection(control), nodes);
            if control == Control::Name {
                node.clear_required();
            }
            if control == Control::Folder
                && form.error.as_deref().is_some_and(|message| {
                    message == "Enter the workspace folder."
                        || message.contains("root_dir does not exist")
                })
            {
                node.set_invalid(Invalid::True);
                node.set_error_message(error_id);
            }
            node
        } else {
            let mut node = named(Role::Button, label);
            node.add_action(Action::Click);
            node
        };
        node.set_bounds(bounds(rect));
        if form.enabled(control) {
            node.add_action(Action::Focus);
        } else {
            node.set_disabled();
            node.remove_action(Action::Click);
            node.remove_action(Action::SetValue);
        }
        match (&mut list, control) {
            (Some(list), Control::Recent(_)) => list.push_child(id),
            _ => dialog.push_child(id),
        }
        nodes.push((id, node));
    }
    if let Some(list) = list {
        nodes.push((list_id, list));
    }
    control_node(form, form.focus)
}
