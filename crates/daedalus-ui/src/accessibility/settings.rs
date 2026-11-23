use super::{bounds, editable_field, named};
use accesskit::{Action, ActionData, ActionRequest, Invalid, Live, Node, NodeId, Role, TreeId};
use daedalus_ui::layout::Rect;
use daedalus_ui::settings::{Control, SettingsAction, SettingsDialog, control_order, controls};
use daedalus_ui::state::AppState;

fn id(dialog: &SettingsDialog, offset: u64) -> NodeId {
    // A distinct namespace keeps callbacks from earlier forms and assignments isolated.
    NodeId((1 << 62) | ((dialog.revision() & ((1 << 57) - 1)) << 5) | offset)
}

pub(super) fn fields(
    settings: &SettingsDialog,
    dialog: &mut Node,
    nodes: &mut Vec<(NodeId, Node)>,
    modal: Rect,
) -> NodeId {
    dialog.set_bounds(bounds(modal));
    let error_id = id(settings, 31);
    if let Some(error) = &settings.error {
        let mut alert = named(Role::Alert, error);
        alert.set_live(Live::Assertive);
        dialog.push_child(error_id);
        nodes.push((error_id, alert));
    }
    for (control, rect) in controls(modal) {
        let enabled = settings.enabled(control);
        let mut node = match control {
            Control::Name | Control::Args => {
                let label = if control == Control::Name {
                    "Name"
                } else {
                    "Arguments"
                };
                let mut field = if enabled {
                    editable_field(
                        label,
                        settings.value(control).unwrap_or_default(),
                        id(settings, 16 + control as u64),
                        settings.selection(control),
                        nodes,
                    )
                } else {
                    named(Role::TextInput, label)
                };
                if control == Control::Args {
                    field.set_description("JSON array of arguments, starting with the executable.");
                }
                if settings.error.is_some() && settings.invalid_field == Some(control) {
                    field.set_invalid(Invalid::True);
                    field.set_error_message(error_id);
                }
                field
            }
            _ => {
                let label = match control {
                    Control::Prev => "Previous assignment".into(),
                    Control::Next => "Next assignment".into(),
                    Control::Add => "Add command".into(),
                    Control::Agy => "Add Agy".into(),
                    Control::Claude => "Add Claude".into(),
                    Control::Codex => "Add Codex".into(),
                    _ => settings.label(control),
                };
                let mut button = named(Role::Button, label);
                if enabled {
                    button.add_action(Action::Click);
                }
                button
            }
        };
        if enabled {
            node.add_action(Action::Focus);
        } else {
            node.set_disabled();
        }
        node.set_bounds(bounds(rect));
        let node_id = id(settings, control as u64);
        dialog.push_child(node_id);
        nodes.push((node_id, node));
    }
    id(settings, settings.focus as u64)
}

pub(crate) fn settings_action(
    state: &mut AppState,
    request: &ActionRequest,
) -> Option<SettingsAction> {
    if request.target_tree != TreeId::ROOT
        || !state.settings.open
        || state.failure_recipients.is_some()
    {
        return None;
    }
    let settings = &mut state.settings;
    let control = control_order()
        .find(|&c| id(settings, c as u64) == request.target_node && settings.enabled(c))?;
    match request.action {
        Action::Focus => settings.set_focus(control),
        Action::Click if !matches!(control, Control::Name | Control::Args) => {
            return Some(settings.activate(control));
        }
        Action::SetValue if matches!(control, Control::Name | Control::Args) => {
            let Some(ActionData::Value(value)) = &request.data else {
                return None;
            };
            settings.set_value(control, value);
        }
        _ => return None,
    }
    Some(SettingsAction::None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(
        settings: &SettingsDialog,
        control: Control,
        action: Action,
        data: Option<ActionData>,
    ) -> ActionRequest {
        ActionRequest {
            target_node: id(settings, control as u64),
            target_tree: TreeId::ROOT,
            action,
            data,
        }
    }

    fn click(state: &mut AppState, control: Control) -> Option<SettingsAction> {
        let req = request(&state.settings, control, Action::Click, None);
        settings_action(state, &req)
    }

    fn set(state: &mut AppState, control: Control, value: &str) {
        let req = request(
            &state.settings,
            control,
            Action::SetValue,
            Some(ActionData::Value(value.into())),
        );
        assert!(matches!(
            settings_action(state, &req),
            Some(SettingsAction::None)
        ));
    }

    #[test]
    fn assignment_controls_follow_visible_bounds_enabled_state_and_focus() {
        let mut state = AppState::new();
        state.settings.begin(vec![]);
        for (w, h, scale) in [(320., 480., 1.), (2400., 1600., 2.)] {
            let update = crate::accessibility::tree(&state, w, h, scale);
            assert_eq!(update.focus, id(&state.settings, Control::Add as u64));
            for (control, r) in controls(daedalus_ui::settings::modal_rect(w, h, scale)) {
                let node_id = id(&state.settings, control as u64);
                let n = &update.nodes.iter().find(|(i, _)| *i == node_id).unwrap().1;
                assert_eq!(n.bounds(), Some(bounds(r)));
                assert_eq!(n.is_disabled(), !state.settings.enabled(control));
                assert_eq!(
                    n.supports_action(Action::Focus),
                    state.settings.enabled(control)
                );
            }
        }
        assert!(click(&mut state, Control::Launch).is_none());
    }

    #[test]
    fn assistive_edit_validates_and_launches_the_named_assignment() {
        let mut state = AppState::new();
        state.settings.begin(vec![]);
        click(&mut state, Control::Add);
        set(&mut state, Control::Name, "日本語");
        assert!(matches!(
            click(&mut state, Control::Launch),
            Some(SettingsAction::None)
        ));
        let update = crate::accessibility::tree(&state, 320., 480., 1.);
        assert_eq!(update.focus, id(&state.settings, Control::Args as u64));
        let field = &update
            .nodes
            .iter()
            .find(|(i, _)| *i == update.focus)
            .unwrap()
            .1;
        assert_eq!(field.invalid(), Some(Invalid::True));
        assert!(
            update
                .nodes
                .iter()
                .any(|(i, n)| Some(*i) == field.error_message() && n.role() == Role::Alert)
        );
        let focus_name = request(&state.settings, Control::Name, Action::Focus, None);
        settings_action(&mut state, &focus_name);
        let update = crate::accessibility::tree(&state, 320., 480., 1.);
        let field = &update
            .nodes
            .iter()
            .find(|(i, _)| *i == update.focus)
            .unwrap()
            .1;
        let selection = field.text_selection().unwrap();
        assert_eq!(
            (
                selection.anchor.character_index,
                selection.focus.character_index
            ),
            (0, 3)
        );
        set(&mut state, Control::Args, r#"["printf","test"]"#);
        let Some(SettingsAction::Launch(entries, selected)) = click(&mut state, Control::Launch)
        else {
            panic!("expected launch");
        };
        assert_eq!(entries[0].id, selected);
        assert_eq!(entries[0].name, "日本語");
        assert_eq!(entries[0].argv, ["printf", "test"]);
    }

    #[test]
    fn a_previous_assignments_requests_cannot_change_the_current_draft() {
        let mut state = AppState::new();
        state.settings.begin(vec![]);
        click(&mut state, Control::Codex);
        let old_remove = request(&state.settings, Control::Remove, Action::Click, None);
        click(&mut state, Control::Claude);
        assert!(settings_action(&mut state, &old_remove).is_none());
        let old_launch = request(&state.settings, Control::Launch, Action::Click, None);
        state.settings.begin(vec![]);
        click(&mut state, Control::Codex);
        assert!(settings_action(&mut state, &old_launch).is_none());
    }
}
