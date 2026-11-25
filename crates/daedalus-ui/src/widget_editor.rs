use daedalus_proto::workbench::{
    CommandOutput, NamedAction, NamedSource, RefreshPolicy, WidgetAction, WidgetDefinition,
    WidgetField, WidgetInstance, WidgetLibrary, WidgetResult, WidgetSource, WidgetTemplate,
    WorkbenchState, WorkspaceCommand, WorkspaceImportPreview,
};
use daedalus_proto::{ClientMessage, WorkspaceId};

#[derive(Clone, Debug, Default)]
pub struct WidgetEditor {
    pub state: Option<WorkbenchState>,
    pub library: WidgetLibrary,
    pub selected: usize,
    pub draft: Option<WidgetDraft>,
    pub preview: Option<WidgetResult>,
    pub sharing: bool,
    pub showing_details: bool,
    pub action_output: bool,
    pub detail_page: usize,
    pub advanced: bool,
    pub definition_json: String,
    pub path: String,
    pub new_root: String,
    pub creation: Option<daedalus_proto::workbench::WorkspaceCreationPreview>,
    pub import: Option<WorkspaceImportPreview>,
    pub resolutions: std::collections::BTreeMap<String, daedalus_proto::workbench::ConflictChoice>,
    widget_test: Option<(uuid::Uuid, String, WidgetDefinition)>,
    import_review: Option<uuid::Uuid>,
    import_lines: Vec<String>,
}

impl WidgetEditor {
    pub(crate) fn start_widget_test(
        &mut self,
        name: &str,
        definition: &WidgetDefinition,
    ) -> uuid::Uuid {
        let token = uuid::Uuid::new_v4();
        self.preview = None;
        self.widget_test = Some((token, name.into(), definition.clone()));
        token
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

    pub(crate) fn start_import_review(&mut self) -> uuid::Uuid {
        let token = uuid::Uuid::new_v4();
        self.import_review = Some(token);
        token
    }

    pub fn finish_import_review(&mut self, token: uuid::Uuid) -> bool {
        if !self.sharing || self.import_review != Some(token) {
            return false;
        }
        self.import_review = None;
        true
    }

    pub(crate) fn is_reviewing_import(&self) -> bool {
        self.import_review.is_some()
    }

    pub(crate) fn import_lines(&self) -> &[String] {
        &self.import_lines
    }

    pub fn show_import_review(&mut self, result: WidgetResult) {
        self.import_lines.clear();
        for (label, value) in &result.details {
            let row = format!("{label} · {value}");
            self.import_lines.extend(wrap_review_row(&row));
        }
        self.preview = Some(result);
        self.detail_page = 0;
    }

    pub fn clear_import_review(&mut self) {
        self.import = None;
        self.creation = None;
        self.import_review = None;
        self.import_lines.clear();
        self.resolutions.clear();
        self.detail_page = 0;
        self.clear_preview();
    }

    pub fn interrupt_previews(&mut self) -> bool {
        let widget_test_running = self.widget_test.take().is_some();
        let import_review_running = self.import_review.take().is_some();
        widget_test_running || import_review_running
    }

    pub fn selected(&self) -> Option<&WidgetInstance> {
        self.state.as_ref()?.document.widgets.get(self.selected)
    }

    pub fn move_selection(&mut self, step: isize) {
        let count = self
            .state
            .as_ref()
            .map_or(0, |state| state.document.widgets.len());
        if count > 0 {
            self.selected = (self.selected as isize + step).rem_euclid(count as isize) as usize;
        }
    }

    pub fn edit(&mut self) {
        let Some(state) = &self.state else { return };
        let Some(widget) = state.document.widgets.get(self.selected) else {
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
        edit: impl FnOnce(&mut daedalus_proto::workbench::WorkspaceDocument),
    ) -> Option<ClientMessage> {
        let state = self.state.as_ref()?;
        let mut document = state.document.clone();
        edit(&mut document);
        Some(ClientMessage::UpdateWorkbench {
            workspace_id: id,
            revision: state.revision,
            document,
        })
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
        self.selected = self
            .selected
            .min(snapshot.document.widgets.len().saturating_sub(1));
        self.state = Some(snapshot);
    }

    pub fn draft_matches_saved_widget(&self) -> bool {
        self.draft
            .as_ref()
            .and_then(|draft| draft.build().ok())
            .zip(self.state.as_ref())
            .is_some_and(|(widget, state)| state.document.widgets.contains(&widget))
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
        let widget = draft.build()?;
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

fn wrap_review_row(text: &str) -> Vec<String> {
    // Fit long argument and environment values into readable settings rows.
    const MAX_COLUMNS: usize = 34;
    let mut printable = String::with_capacity(text.len());
    for character in text.chars() {
        if character.is_control() {
            printable.extend(character.escape_default());
        } else {
            printable.push(character);
        }
    }
    let mut rows = Vec::new();
    let mut row = String::new();
    let mut columns = 0;
    for character in printable.chars() {
        let width = if character.is_ascii() && !matches!(character, 'M' | 'W' | 'm' | 'w' | '@') {
            1
        } else {
            2
        };
        if columns + width > MAX_COLUMNS {
            rows.push(std::mem::take(&mut row));
            columns = 0;
        }
        row.push(character);
        columns += width;
    }
    rows.push(row);
    rows
}

#[derive(Clone, Debug)]
pub struct WidgetDraft {
    revision: u64,
    pub existing: bool,
    pub widget: WidgetInstance,
    pub template: Option<WidgetTemplate>,
    pub definition: WidgetDefinition,
    pub executable: String,
    pub arguments: String,
    pub interval: String,
    pub output: CommandOutput,
    pub publish_key: String,
    pub summary: String,
    pub details: String,
    pub url: String,
    pub change_source: bool,
}

impl WidgetDraft {
    fn new(
        revision: u64,
        widget: WidgetInstance,
        definition: WidgetDefinition,
        existing: bool,
    ) -> Self {
        let mut draft = Self {
            revision,
            existing,
            widget,
            template: None,
            definition,
            executable: String::new(),
            arguments: String::new(),
            interval: String::new(),
            output: CommandOutput::default(),
            publish_key: String::new(),
            summary: String::new(),
            details: String::new(),
            url: String::new(),
            change_source: false,
        };
        draft.load_fields();
        draft
    }

    pub fn set_template(&mut self, template: &WidgetTemplate) {
        self.template = Some(template.clone());
        self.widget.template = Some(template.id.clone());
        self.widget.name.clone_from(&template.name);
        self.definition.clone_from(&template.definition);
        self.change_source = false;
        self.load_fields();
    }

    pub fn replace_definition(&mut self, definition: WidgetDefinition) {
        self.definition = definition;
        self.change_source = false;
        self.load_fields();
    }

    fn load_fields(&mut self) {
        self.executable.clear();
        self.arguments.clear();
        self.interval.clear();
        self.publish_key.clear();
        self.output = CommandOutput::default();
        if let Some(source) = self.definition.sources.first() {
            match &source.source {
                WidgetSource::Command {
                    command,
                    refresh,
                    output,
                } => {
                    self.executable = command.argv.first().cloned().unwrap_or_default();
                    self.arguments =
                        serde_json::to_string(command.argv.get(1..).unwrap_or_default())
                            .unwrap_or_default();
                    self.interval = refresh
                        .interval_seconds
                        .map(|interval| interval.to_string())
                        .unwrap_or_default();
                    self.output.clone_from(output);
                }
                WidgetSource::Published { key } => self.publish_key.clone_from(key),
                _ => {}
            }
        }
        self.summary = format_fields(&self.definition.display.summary);
        self.details = format_fields(&self.definition.display.details);
        self.url = self
            .definition
            .actions
            .iter()
            .find_map(|action| match &action.action {
                WidgetAction::OpenUrl { url } => Some(url.clone()),
                _ => None,
            })
            .unwrap_or_default();
    }

    pub fn build(&self) -> Result<WidgetInstance, String> {
        let mut widget = self.widget.clone();
        widget.name = widget.name.trim().into();
        if widget.name.is_empty() {
            return Err("Enter a name for the widget.".into());
        }
        let mut definition = self.definition.clone();
        if self.change_source {
            let source_id = definition
                .sources
                .first()
                .map_or("main", |source| source.id.as_str())
                .to_owned();
            let source = if !self.executable.trim().is_empty() {
                let mut argv = vec![self.executable.trim().to_owned()];
                if !self.arguments.trim().is_empty() {
                    let arguments: Vec<String> = serde_json::from_str(&self.arguments).map_err(
                        |_| "Arguments use a JSON array, for example [\"test\", \"--workspace\"].",
                    )?;
                    argv.extend(arguments);
                }
                let interval = if self.interval.trim().is_empty() {
                    None
                } else {
                    Some(
                        self.interval
                            .trim()
                            .parse::<u32>()
                            .ok()
                            .filter(|value| *value > 0)
                            .ok_or("Use a positive refresh interval in seconds.")?,
                    )
                };
                let (env, timeout_seconds, on_runner_completion) =
                    match definition.sources.first().map(|source| &source.source) {
                        Some(WidgetSource::Command {
                            command, refresh, ..
                        }) => (
                            command.env.clone(),
                            command.timeout_seconds,
                            refresh.on_runner_completion,
                        ),
                        _ => (Vec::new(), None, false),
                    };
                Some(WidgetSource::Command {
                    command: WorkspaceCommand {
                        argv,
                        env,
                        timeout_seconds,
                    },
                    refresh: RefreshPolicy {
                        interval_seconds: interval,
                        on_runner_completion,
                    },
                    output: self.output.clone(),
                })
            } else if !self.publish_key.trim().is_empty() {
                Some(WidgetSource::Published {
                    key: self.publish_key.trim().into(),
                })
            } else {
                None
            };
            if let Some(source) = source {
                let named = NamedSource {
                    id: source_id,
                    source,
                };
                if let Some(first) = definition.sources.first_mut() {
                    *first = named;
                } else {
                    definition.sources.push(named);
                }
            }
        }
        if self.summary != format_fields(&definition.display.summary) {
            definition.display.summary = parse_fields(&self.summary)?;
        }
        if self.details != format_fields(&definition.display.details) {
            definition.display.details = parse_fields(&self.details)?;
        }
        if self.url
            != self
                .definition
                .actions
                .iter()
                .find_map(|action| match &action.action {
                    WidgetAction::OpenUrl { url } => Some(url.as_str()),
                    _ => None,
                })
                .unwrap_or_default()
        {
            if let Some(index) = definition
                .actions
                .iter()
                .position(|action| matches!(action.action, WidgetAction::OpenUrl { .. }))
            {
                if self.url.trim().is_empty() {
                    definition.actions.remove(index);
                } else {
                    definition.actions[index].action = WidgetAction::OpenUrl {
                        url: self.url.trim().into(),
                    };
                }
            } else if !self.url.trim().is_empty() {
                definition.actions.push(NamedAction {
                    id: uuid::Uuid::new_v4().to_string(),
                    label: "Open link".into(),
                    action: WidgetAction::OpenUrl {
                        url: self.url.trim().into(),
                    },
                });
            }
        }
        widget.definition = Some(definition);
        Ok(widget)
    }
}

fn format_fields(fields: &[WidgetField]) -> String {
    fields
        .iter()
        .map(|field| format!("{}:{}", field.source, field.field))
        .collect::<Vec<_>>()
        .join(", ")
}

fn parse_fields(text: &str) -> Result<Vec<WidgetField>, String> {
    text.split(',')
        .map(str::trim)
        .filter(|field| !field.is_empty())
        .map(|field| {
            let (source, field) = field
                .split_once(':')
                .ok_or("Use source:field, for example main:branch.")?;
            Ok(WidgetField {
                source: source.trim().into(),
                field: field.trim().into(),
                label: None,
            })
        })
        .collect()
}
