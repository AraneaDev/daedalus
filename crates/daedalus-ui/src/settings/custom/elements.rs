use super::{Control, SettingsDialog};
use crate::widget_editor::WidgetDraft;
use Control::*;
use daedalus_proto::workbench::{WidgetElement, WidgetField};

pub(super) fn is_text_field(control: Control) -> bool {
    matches!(
        control,
        CustomElementText
            | CustomElementSource
            | CustomElementField
            | CustomElementCaption
            | CustomElementMax
    )
}

impl SettingsDialog {
    pub(super) fn element_rows(
        &self,
        pages: Vec<Control>,
        draft: &WidgetDraft,
    ) -> Vec<Vec<Control>> {
        let mut rows = vec![
            pages,
            vec![
                CustomElementAddLabel,
                CustomElementAddStatus,
                CustomElementAddCounter,
            ],
            vec![CustomElementAddProgress, CustomElementAddButton],
        ];
        if let Some(selected) = draft.selected_element() {
            rows.push(vec![CustomElementPrev, CustomElementNext]);
            match selected.element {
                WidgetElement::Label { .. } => rows.push(vec![CustomElementText]),
                WidgetElement::Counter { .. } | WidgetElement::Progress { .. } => {
                    rows.extend([
                        vec![CustomElementSource, CustomElementField],
                        vec![CustomElementCaption],
                    ]);
                    if matches!(selected.element, WidgetElement::Progress { .. }) {
                        rows.push(vec![CustomElementMax]);
                    }
                }
                WidgetElement::Button { .. } => rows.push(vec![CustomElementAction]),
                WidgetElement::Status => {}
            }
            rows.push(vec![
                CustomElementEarlier,
                CustomElementLater,
                CustomElementRemove,
            ]);
        }
        rows.push(vec![CustomElementsBack, Cancel]);
        rows
    }

    pub(super) fn element_label(&self, control: Control) -> Option<String> {
        let draft = self.custom.draft.as_ref()?;
        Some(match control {
            CustomElements => format!("Elements · {} ›", draft.elements.len()),
            CustomElementsBack => "Back to form".into(),
            CustomElementAddLabel => "+ Label".into(),
            CustomElementAddStatus => "+ Status".into(),
            CustomElementAddCounter => "+ Counter".into(),
            CustomElementAddProgress => "+ Progress".into(),
            CustomElementAddButton if draft.available_actions().is_empty() => {
                "+ Button (needs action)".into()
            }
            CustomElementAddButton => "+ Button".into(),
            CustomElementPrev => "‹ Previous element".into(),
            CustomElementNext => "Next element ›".into(),
            CustomElementEarlier => "Move earlier".into(),
            CustomElementLater => "Move later".into(),
            CustomElementRemove => "Remove element".into(),
            CustomElementAction => {
                let WidgetElement::Button { action } = &draft.selected_element()?.element else {
                    return None;
                };
                let label = draft
                    .available_actions()
                    .into_iter()
                    .find(|candidate| candidate.id == *action)
                    .map_or_else(|| "Select an action".into(), |candidate| candidate.label);
                format!("Action · {label} ↻")
            }
            _ => return None,
        })
    }

    pub(super) fn element_enabled(&self, control: Control) -> Option<bool> {
        let draft = self.custom.draft.as_ref()?;
        let index = draft.editing_element.unwrap_or_default();
        Some(match control {
            CustomElementAddButton | CustomElementAction => !draft.available_actions().is_empty(),
            CustomElementPrev | CustomElementEarlier => index > 0,
            CustomElementNext | CustomElementLater => index + 1 < draft.elements.len(),
            CustomElementRemove => draft.selected_element().is_some(),
            _ => return None,
        })
    }

    pub(super) fn element_value(&self, control: Control) -> Option<&str> {
        let selected = self.custom.draft.as_ref()?.selected_element()?;
        let field = selected.element.value_field();
        Some(match control {
            CustomElementText => match &selected.element {
                WidgetElement::Label { text } => text,
                _ => return None,
            },
            CustomElementSource => &field?.source,
            CustomElementField => &field?.field,
            CustomElementCaption if field.is_some() => &selected.caption,
            CustomElementMax if matches!(selected.element, WidgetElement::Progress { .. }) => {
                &selected.maximum
            }
            _ => return None,
        })
    }

    pub(super) fn element_field(&mut self) -> Option<&mut String> {
        let selected = self.custom.draft.as_mut()?.selected_element_mut()?;
        Some(match self.focus {
            CustomElementText => match &mut selected.element {
                WidgetElement::Label { text } => text,
                _ => return None,
            },
            CustomElementSource => &mut selected.value_field_mut()?.source,
            CustomElementField => &mut selected.value_field_mut()?.field,
            CustomElementCaption if selected.element.value_field().is_some() => {
                &mut selected.caption
            }
            CustomElementMax if matches!(selected.element, WidgetElement::Progress { .. }) => {
                &mut selected.maximum
            }
            _ => return None,
        })
    }

    pub(super) fn apply_element_control(&mut self, control: Control) -> bool {
        let Some(draft) = &mut self.custom.draft else {
            return false;
        };
        if let Some((element, focus)) = new_element(draft, control) {
            draft.add_element(element);
            self.focus = focus;
            self.select_all = is_text_field(focus);
            self.custom.clear_preview();
            return true;
        }
        let index = draft.editing_element.unwrap_or_default();
        match control {
            CustomElements => {
                draft.editing_element = Some(0);
                self.focus = CustomElementAddLabel;
            }
            CustomElementsBack => {
                draft.editing_element = None;
                self.focus = CustomElements;
            }
            CustomElementPrev => draft.editing_element = Some(index - 1),
            CustomElementNext => draft.editing_element = Some(index + 1),
            CustomElementEarlier | CustomElementLater => {
                let next = if control == CustomElementEarlier {
                    index - 1
                } else {
                    index + 1
                };
                draft.elements.swap(index, next);
                draft.editing_element = Some(next);
                self.custom.clear_preview();
            }
            CustomElementRemove => {
                draft.elements.remove(index);
                draft.editing_element = Some(index.min(draft.elements.len().saturating_sub(1)));
                if draft.elements.is_empty() {
                    self.focus = CustomElementAddLabel;
                }
                self.custom.clear_preview();
            }
            CustomElementAction => {
                cycle_action(draft);
                self.custom.clear_preview();
            }
            control if is_text_field(control) => self.select_all = true,
            _ => return false,
        }
        true
    }
}

fn new_element(draft: &WidgetDraft, control: Control) -> Option<(WidgetElement, Control)> {
    let field = || WidgetField {
        source: draft
            .definition
            .sources
            .first()
            .map_or("main", |source| source.id.as_str())
            .into(),
        field: "count".into(),
        label: None,
    };
    Some(match control {
        CustomElementAddLabel => (
            WidgetElement::Label {
                text: String::new(),
            },
            CustomElementText,
        ),
        CustomElementAddStatus => (WidgetElement::Status, CustomElementAddStatus),
        CustomElementAddCounter => (
            WidgetElement::Counter { field: field() },
            CustomElementField,
        ),
        CustomElementAddProgress => (
            WidgetElement::Progress {
                field: field(),
                max: 100,
            },
            CustomElementField,
        ),
        CustomElementAddButton => (
            WidgetElement::Button {
                action: draft.available_actions().into_iter().next()?.id,
            },
            CustomElementAction,
        ),
        _ => return None,
    })
}

fn cycle_action(draft: &mut WidgetDraft) {
    let actions = draft.available_actions();
    let Some(selected) = draft.selected_element_mut() else {
        return;
    };
    let WidgetElement::Button { action } = &mut selected.element else {
        return;
    };
    if !actions.is_empty() {
        let next = actions
            .iter()
            .position(|candidate| candidate.id == *action)
            .map_or(0, |index| (index + 1) % actions.len());
        action.clone_from(&actions[next].id);
    }
}
