use super::{TerminalControls, bounds, named};
use accesskit::{Action, ActionRequest, Node, NodeId, Role, TreeId};
use daedalus_ui::input::UiAction;
use daedalus_ui::layout::{Rect, overlays::search_controls};
use daedalus_ui::state::AppState;

pub(super) fn append(
    state: &AppState,
    search_node: &mut Node,
    nodes: &mut Vec<(NodeId, Node)>,
    bar: Option<Rect>,
    controls: &mut TerminalControls,
) {
    let Some(search) = &state.search else {
        return;
    };
    if let Some(bar) = bar {
        search_node.set_bounds(bounds(bar));
    }
    let base = controls.register_search(search.session());
    let rectangles = bar.map(search_controls);
    for (index, (label, shortcut)) in [
        ("Previous match", "Shift+Enter"),
        ("Next match", "Enter"),
        ("Match case", "F4"),
    ]
    .into_iter()
    .enumerate()
    {
        let is_case = index == 2;
        let mut control = named(
            if is_case {
                Role::CheckBox
            } else {
                Role::Button
            },
            label,
        );
        control.set_keyboard_shortcut(shortcut);
        if let Some(rectangles) = rectangles {
            control.set_bounds(bounds(rectangles[index]));
        }
        if is_case {
            control.set_toggled(search.case_sensitive.into());
        }
        if is_case || !search.matches.is_empty() {
            control.add_action(Action::Click);
        } else {
            control.set_disabled();
        }
        let id = NodeId(base.0 + index as u64);
        search_node.push_child(id);
        nodes.push((id, control));
    }
}

pub(super) fn action(
    state: &AppState,
    request: &ActionRequest,
    controls: &TerminalControls,
) -> Option<UiAction> {
    if request.target_tree != TreeId::ROOT || request.action != Action::Click {
        return None;
    }
    let search = state.search.as_ref()?;
    let base = controls.search_base(search.session())?;
    match request.target_node.0.checked_sub(base.0)? {
        0 if !search.matches.is_empty() => Some(UiAction::SearchPrev),
        1 if !search.matches.is_empty() => Some(UiAction::SearchNext),
        2 => Some(UiAction::SearchToggleCase),
        _ => None,
    }
}
