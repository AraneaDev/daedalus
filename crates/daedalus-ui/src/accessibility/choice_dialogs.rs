use super::{PICKER, bounds, named, terminals::next_control_group};
use accesskit::{Action, ActionRequest, Node, NodeId, Role, TreeId};
use daedalus_proto::{CustomAction, PaneId, PaneRole, WorkspaceId};
use daedalus_ui::input::UiAction;
use daedalus_ui::layout::overlays::{
    action_palette_layout, picker_item_rect, preset_picker_layout,
};
use daedalus_ui::state::{ActiveOverlay, AgentPreset, AppState, SessionAction};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Preset(u64, Option<WorkspaceId>, Option<PaneId>),
    Action(u64, Option<WorkspaceId>),
}

impl Scope {
    fn current(state: &AppState) -> Option<Self> {
        match state.active_overlay()? {
            ActiveOverlay::AgentPicker => Some(Self::Preset(
                state.picker.session(),
                state.active_workspace_id,
                state.pane_id_for_role(PaneRole::Agent),
            )),
            ActiveOverlay::ActionPalette => Some(Self::Action(
                state.action_palette.session(),
                state.active_workspace_id,
            )),
            _ => None,
        }
    }
}

enum Choice {
    Preset(AgentPreset),
    Action(CustomAction),
    Session(SessionAction),
}

#[derive(Default)]
pub(super) struct ChoiceControls {
    scope: Option<Scope>,
    rows: Vec<(NodeId, Choice)>,
    list: Option<NodeId>,
}

impl ChoiceControls {
    pub(super) fn append(
        &mut self,
        state: &AppState,
        id: NodeId,
        dialog: &mut Node,
        nodes: &mut Vec<(NodeId, Node)>,
        (width, height, scale): (f32, f32, f32),
        serial: &mut u64,
    ) -> NodeId {
        let scope = Scope::current(state);
        let actions = state.active_actions();
        let sessions = state.action_palette.session_actions.as_deref();
        let is_picker = id == PICKER;
        let changed = self.scope != scope
            || (!is_picker
                && (self.rows.len() != state.palette_len()
                    || match sessions {
                        Some(sessions) => self.rows.iter().zip(sessions).any(|((_, choice), action)| {
                            !matches!(choice, Choice::Session(stored) if stored == action)
                        }),
                        None => self.rows.iter().zip(actions).any(|((_, choice), action)| {
                            !matches!(choice, Choice::Action(stored) if stored == action)
                        }),
                    }));
        if self.list.is_none() || changed {
            self.scope = scope;
            self.rows.clear();
            self.list = Some(next_control_group(serial));
            if is_picker {
                self.rows.extend(
                    AgentPreset::ALL
                        .into_iter()
                        .map(|preset| (next_control_group(serial), Choice::Preset(preset))),
                );
            } else if let Some(sessions) = sessions {
                self.rows.extend(
                    sessions.iter().map(|action| {
                        (next_control_group(serial), Choice::Session(action.clone()))
                    }),
                );
            } else {
                self.rows.extend(
                    actions
                        .iter()
                        .map(|action| (next_control_group(serial), Choice::Action(action.clone()))),
                );
            }
        }
        let selected = if is_picker {
            state.picker.selected_index
        } else {
            state.action_palette.selected_index
        };
        let (modal, visible) = if is_picker {
            preset_picker_layout(width, height, scale, selected)
        } else {
            action_palette_layout(width, height, scale, state.palette_len(), selected)
        };
        dialog.set_bounds(bounds(modal));
        if self.rows.is_empty() {
            return id;
        }
        let Some(list_id) = self.list else {
            return id;
        };
        let mut list = named(
            Role::Group,
            if is_picker {
                "Agent presets"
            } else if sessions.is_some() {
                "Workspace sessions"
            } else {
                "Workspace actions"
            },
        );
        for (index, (node_id, choice)) in self.rows.iter().enumerate() {
            let label = match choice {
                Choice::Preset(preset) => preset.label().to_owned(),
                Choice::Action(action) => action.label.clone(),
                Choice::Session(action) => action.label(),
            };
            // A row executes an action when activated. An item-selection role would also
            // invoke its Click handler when a platform selection API just selects it.
            let mut node = named(Role::Button, label);
            if let Choice::Action(action) = choice {
                node.set_description(format!("{} pane", action.target));
                if let Some(shortcut) = &action.shortcut {
                    node.set_keyboard_shortcut(shortcut.as_str());
                }
            }
            node.add_action(Action::Focus);
            node.add_action(Action::Click);
            if visible.contains(&index) {
                node.set_bounds(bounds(picker_item_rect(
                    modal,
                    index - visible.start,
                    scale,
                )));
            }
            list.push_child(*node_id);
            nodes.push((*node_id, node));
        }
        dialog.push_child(list_id);
        nodes.push((list_id, list));
        self.rows.get(selected).map_or(id, |(node, _)| *node)
    }

    pub(super) fn action(&self, state: &mut AppState, request: &ActionRequest) -> Option<UiAction> {
        if request.target_tree != TreeId::ROOT
            || !matches!(request.action, Action::Focus | Action::Click)
            || Scope::current(state)? != self.scope?
        {
            return None;
        }
        let (index, (_, choice)) = self
            .rows
            .iter()
            .enumerate()
            .find(|(_, (node_id, _))| *node_id == request.target_node)?;
        let confirm = match choice {
            Choice::Preset(preset) if AgentPreset::ALL.get(index) == Some(preset) => {
                state.picker.selected_index = index;
                UiAction::PickerConfirm
            }
            Choice::Session(cached) => {
                let index = state
                    .action_palette
                    .session_actions
                    .as_ref()?
                    .iter()
                    .position(|action| action == cached)?;
                state.action_palette.selected_index = index;
                UiAction::ActionPaletteConfirm
            }
            Choice::Action(cached) if state.action_palette.session_actions.is_none() => {
                // Actions can move when a daemon refresh replaces the workspace manifest.
                let index = state
                    .active_actions()
                    .iter()
                    .position(|action| action == cached)?;
                state.action_palette.selected_index = index;
                UiAction::ActionPaletteConfirm
            }
            _ => return None,
        };
        Some(if request.action == Action::Click {
            confirm
        } else {
            UiAction::Ignore
        })
    }
}
