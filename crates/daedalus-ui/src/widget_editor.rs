use daedalus_proto::workbench::{
    WidgetDefinition, WidgetInstance, WidgetLibrary, WidgetResult, WorkbenchState,
    WorkspaceDocument, WorkspaceImportPreview,
};
use daedalus_proto::{ClientMessage, WorkspaceId};

mod draft;
mod elements;
mod fields;
mod import_review;

pub(crate) use draft::AppearanceInputError;
pub use draft::WidgetDraft;
use import_review::ImportReview;

#[cfg(test)]
mod tests;

#[derive(Clone, Debug, Default)]
enum WidgetSelection {
    #[default]
    First,
    Selected(String),
    Cleared,
}

#[derive(Clone, Debug, Default)]
pub struct WidgetEditor {
    pub state: Option<WorkbenchState>,
    pub library: WidgetLibrary,
    selection: WidgetSelection,
    pub draft: Option<WidgetDraft>,
    pub preview: Option<WidgetResult>,
    pub sharing: bool,
    pub showing_details: bool,
    pub action_output: bool,
    pub detail_page: usize,
    pub(crate) detail_action_page: usize,
    pub advanced: bool,
    pub definition_json: String,
    pub path: String,
    pub new_root: String,
    pub creation: Option<daedalus_proto::workbench::WorkspaceCreationPreview>,
    pub import: Option<WorkspaceImportPreview>,
    pub resolutions: std::collections::BTreeMap<String, daedalus_proto::workbench::ConflictChoice>,
    widget_test: Option<(uuid::Uuid, String, WidgetDefinition)>,
    detail_session: Option<uuid::Uuid>,
    import_review: ImportReview,
}

impl WidgetEditor {
    pub fn open_details(&mut self, index: usize) -> bool {
        let Some(widget) = self
            .state
            .as_ref()
            .and_then(|state| state.document.widgets.get(index))
        else {
            return false;
        };
        self.selection = WidgetSelection::Selected(widget.id.clone());
        self.showing_details = true;
        self.action_output = false;
        self.detail_page = 0;
        self.detail_action_page = 0;
        self.detail_session = Some(uuid::Uuid::new_v4());
        true
    }

    pub(crate) fn close_details(&mut self) {
        self.showing_details = false;
        self.detail_session = None;
    }

    pub(crate) fn detail_session(&self) -> Option<uuid::Uuid> {
        self.detail_session
    }

    pub fn matches_detail_session(&self, widget: &str, session: uuid::Uuid) -> bool {
        self.showing_details
            && self.detail_session == Some(session)
            && self
                .selected()
                .is_some_and(|selected| selected.id == widget)
    }

    pub(crate) fn start_widget_test(
        &mut self,
        name: &str,
        definition: &WidgetDefinition,
    ) -> Option<uuid::Uuid> {
        if self
            .widget_test
            .as_ref()
            .is_some_and(|(_, pending_name, pending_definition)| {
                pending_name == name && pending_definition == definition
            })
        {
            return None;
        }
        let token = uuid::Uuid::new_v4();
        self.preview = None;
        self.widget_test = Some((token, name.into(), definition.clone()));
        Some(token)
    }

    pub fn finish_widget_test(&mut self, token: uuid::Uuid) -> bool {
        let Some((_, name, definition)) = self.widget_test.take_if(|test| test.0 == token) else {
            return false;
        };
        !self.sharing
            && self
                .draft
                .as_ref()
                .and_then(|draft| draft.build().ok())
                .is_some_and(|widget| {
                    widget.name == name && widget.definition.unwrap_or_default() == definition
                })
    }

    pub fn clear_preview(&mut self) {
        self.preview = None;
        self.widget_test = None;
    }

    pub fn interrupt_previews(&mut self) -> bool {
        let widget_test_running = self.widget_test.take().is_some();
        let import_review_running = self.import_review.token.take().is_some();
        widget_test_running || import_review_running
    }

    pub fn selected(&self) -> Option<&WidgetInstance> {
        self.state
            .as_ref()?
            .document
            .widgets
            .get(self.selected_index()?)
    }

    pub(crate) fn selected_index(&self) -> Option<usize> {
        let widgets = &self.state.as_ref()?.document.widgets;
        match &self.selection {
            WidgetSelection::First => (!widgets.is_empty()).then_some(0),
            WidgetSelection::Selected(id) => widgets.iter().position(|widget| widget.id == *id),
            WidgetSelection::Cleared => None,
        }
    }

    pub fn move_selection(&mut self, step: isize) {
        let Some(state) = &self.state else { return };
        let widgets = &state.document.widgets;
        let count = widgets.len() as isize;
        if count == 0 {
            return;
        }
        let index = self.selected_index().map_or_else(
            || if step < 0 { count - 1 } else { 0 },
            |index| (index as isize + step).rem_euclid(count),
        );
        self.selection = WidgetSelection::Selected(widgets[index as usize].id.clone());
    }

    pub fn edit(&mut self) {
        let Some(state) = &self.state else { return };
        let Some(widget) = self.selected() else {
            return;
        };
        let definition = state
            .document
            .widget_definition(widget)
            .cloned()
            .unwrap_or_default();
        let (revision, widget) = (state.revision, widget.clone());
        self.clear_preview();
        self.draft = Some(WidgetDraft::new(revision, widget, definition, true));
    }

    pub fn create(&mut self) {
        let Some(revision) = self.state.as_ref().map(|state| state.revision) else {
            return;
        };
        self.clear_preview();
        let widget = WidgetInstance {
            id: uuid::Uuid::new_v4().to_string(),
            name: String::new(),
            pinned: true,
            template: None,
            definition: None,
        };
        self.draft = Some(WidgetDraft::new(
            revision,
            widget,
            WidgetDefinition::default(),
            false,
        ));
    }

    pub fn choose_template(&mut self) {
        let Some(draft) = &mut self.draft else { return };
        if self.library.templates.is_empty() {
            return;
        }
        let index = self
            .library
            .templates
            .iter()
            .position(|template| Some(template.id.as_str()) == draft.widget.template.as_deref())
            .map_or(0, |index| (index + 1) % self.library.templates.len());
        let template = self.library.templates[index].clone();
        draft.set_template(&template);
        self.clear_preview();
    }

    pub fn update(
        &self,
        id: WorkspaceId,
        edit: impl FnOnce(&mut WorkspaceDocument),
    ) -> Option<ClientMessage> {
        let state = self.state.as_ref()?;
        let mut document = state.document.clone();
        edit(&mut document);
        Some(ClientMessage::UpdateWorkbench {
            workspace_id: id,
            revision: state.revision,
            document,
            layout_only: false,
            edit: Some(daedalus_proto::workbench::WorkbenchEdit::Widgets),
        })
    }

    pub(crate) fn update_selected(
        &self,
        id: WorkspaceId,
        edit: impl FnOnce(&mut Vec<WidgetInstance>, usize),
    ) -> Option<ClientMessage> {
        let index = self.selected_index()?;
        self.update(id, |document| edit(&mut document.widgets, index))
    }

    pub fn accept_snapshot(&mut self, snapshot: WorkbenchState) {
        if let Some(previous) = &self.state
            && let Some(draft) = &mut self.draft
            && draft.revision == previous.revision
        {
            let before = previous
                .document
                .widgets
                .iter()
                .find(|w| w.id == draft.widget.id);
            let after = snapshot
                .document
                .widgets
                .iter()
                .find(|w| w.id == draft.widget.id);
            if before == after
                && before.and_then(|w| previous.document.widget_definition(w))
                    == after.and_then(|w| snapshot.document.widget_definition(w))
            {
                draft.revision = snapshot.revision;
            }
        }
        if self.showing_details
            && self.selected().is_some_and(|selected| {
                let previous = self
                    .state
                    .as_ref()
                    .and_then(|state| state.document.widget_definition(selected));
                let next = snapshot
                    .document
                    .widgets
                    .iter()
                    .find(|widget| widget.id == selected.id)
                    .and_then(|widget| snapshot.document.widget_definition(widget));
                previous != next
            })
        {
            self.detail_page = 0;
            self.detail_action_page = 0;
        }
        let widgets = &snapshot.document.widgets;
        match &self.selection {
            WidgetSelection::First => {
                if let Some(widget) = widgets.first() {
                    self.selection = WidgetSelection::Selected(widget.id.clone());
                }
            }
            WidgetSelection::Selected(id) if !widgets.iter().any(|widget| widget.id == *id) => {
                self.selection = WidgetSelection::Cleared;
                self.close_details();
                if self.draft.is_none() {
                    self.clear_preview();
                }
            }
            _ => {}
        }
        self.state = Some(snapshot);
    }

    pub fn draft_matches_saved_widget(&self) -> bool {
        self.draft
            .as_ref()
            .zip(self.state.as_ref())
            .is_some_and(|(draft, state)| {
                draft
                    .build_for_document(&state.document)
                    .is_ok_and(|widget| state.document.widgets.contains(&widget))
            })
    }

    pub fn save(&self, id: WorkspaceId) -> Result<ClientMessage, String> {
        let draft = self.draft.as_ref().ok_or("Open a widget to edit it")?;
        let state = self
            .state
            .as_ref()
            .ok_or("Workspace settings are still loading")?;
        if state.revision != draft.revision {
            return Err("The workspace changed while editing. Reopen this widget to review its current settings.".into());
        }
        let widget = draft.build_for_document(&state.document)?;
        self.update(id, |document| {
            if let Some(template) = draft.template.as_ref()
                && !document
                    .templates
                    .iter()
                    .any(|candidate| candidate.id == template.id)
            {
                document.templates.push(template.clone());
            }
            if let Some(existing) = document
                .widgets
                .iter_mut()
                .find(|existing| existing.id == widget.id)
            {
                *existing = widget;
            } else {
                document.widgets.push(widget);
            }
        })
        .ok_or_else(|| "Workspace settings are still loading".into())
    }
}
