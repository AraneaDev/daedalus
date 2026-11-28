use super::elements::ElementDraft;
use super::fields::{edit_fields, format_fields};
use daedalus_proto::workbench::{
    CommandOutput, MAX_WIDGET_WIDTH, MIN_WIDGET_WIDTH, NamedAction, NamedSource, RefreshPolicy,
    WidgetAction, WidgetAppearance, WidgetDefinition, WidgetInstance, WidgetSource, WidgetTemplate,
    WorkspaceCommand, WorkspaceDocument,
};

pub(crate) enum AppearanceInputError {
    Accent,
    Width,
}

impl std::fmt::Display for AppearanceInputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Accent => {
                f.write_str("Use #RRGGBB for Accent, or clear it to use the theme color.")
            }
            Self::Width => write!(
                f,
                "Enter a whole width from {MIN_WIDGET_WIDTH}–{MAX_WIDGET_WIDTH} px, or clear it for automatic sizing."
            ),
        }
    }
}

#[derive(Clone, Debug)]
pub struct WidgetDraft {
    pub(super) revision: u64,
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
    pub(crate) accent: String,
    pub(crate) width: String,
    pub(crate) icon: String,
    pub(crate) editing_appearance: bool,
    pub(crate) elements: Vec<ElementDraft>,
    pub(crate) editing_element: Option<usize>,
    new_url_action_id: String,
}

impl WidgetDraft {
    pub(super) fn new(
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
            accent: String::new(),
            width: String::new(),
            icon: String::new(),
            editing_appearance: false,
            elements: Vec::new(),
            editing_element: None,
            new_url_action_id: uuid::Uuid::new_v4().to_string(),
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
        let display = &self.definition.display;
        self.accent = display.appearance.accent.clone().unwrap_or_default();
        self.width = display
            .appearance
            .width
            .map(|width| width.to_string())
            .unwrap_or_default();
        self.icon = display.icon.clone().unwrap_or_default();
        self.editing_appearance = false;
        self.elements = self
            .definition
            .display
            .elements
            .iter()
            .cloned()
            .map(ElementDraft::new)
            .collect();
        self.editing_element = None;
        self.url =
            first_url(&self.definition.actions).map_or_else(String::new, |(_, url)| url.into());
    }

    pub fn build(&self) -> Result<WidgetInstance, String> {
        let mut widget = self.widget.clone();
        widget.name = widget.name.trim().into();
        if widget.name.is_empty() {
            return Err("Enter a name for the widget.".into());
        }
        widget.definition = Some(self.build_definition()?);
        Ok(widget)
    }

    pub(crate) fn build_definition(&self) -> Result<WidgetDefinition, String> {
        let mut definition = self.definition.clone();
        self.update_source(&mut definition)?;
        self.update_display(&mut definition)?;
        definition.actions = self.available_actions();
        if let Some(id) = definition.display.primary_action.as_deref()
            && (id == self.new_url_action_id || self.definition.primary_action().is_some())
            && definition.primary_action().is_none()
        {
            definition.display.primary_action = None;
        }
        Ok(definition)
    }

    pub(crate) fn available_actions(&self) -> Vec<NamedAction> {
        let mut actions = self.definition.actions.clone();
        self.update_url(&mut actions);
        actions
    }

    pub(crate) fn primary_action_label(&self) -> Option<String> {
        let id = self.definition.display.primary_action.as_deref()?;
        self.available_actions()
            .into_iter()
            .find(|action| action.id == id)
            .map(|action| action.label)
    }

    pub(crate) fn cycle_primary_action(&mut self) {
        let actions = self.available_actions();
        let current = self.definition.display.primary_action.as_deref();
        let next = actions
            .iter()
            .position(|action| Some(action.id.as_str()) == current)
            .map_or(0, |index| index + 1);
        self.definition.display.primary_action = actions.get(next).map(|action| action.id.clone());
    }

    pub(super) fn build_for_document(
        &self,
        document: &WorkspaceDocument,
    ) -> Result<WidgetInstance, String> {
        let mut widget = self.build()?;
        // Existing inline definitions stay explicit unless the user chooses a template.
        if (self.widget.definition.is_none() || self.template.is_some())
            && let Some(id) = widget.template.as_deref()
            && let Some(template) = document
                .templates
                .iter()
                .find(|candidate| candidate.id == id)
                .or(self.template.as_ref())
            && widget.definition.as_ref() == Some(&template.definition)
        {
            widget.definition = None;
        }
        Ok(widget)
    }

    fn update_source(&self, definition: &mut WidgetDefinition) -> Result<(), String> {
        if !self.change_source {
            return Ok(());
        }
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
        Ok(())
    }

    fn update_display(&self, definition: &mut WidgetDefinition) -> Result<(), String> {
        definition.display.appearance = self
            .validated_appearance()
            .map_err(|error| error.to_string())?;
        if self.icon != definition.display.icon.as_deref().unwrap_or_default() {
            let icon = self.icon.trim();
            definition.display.icon = (!icon.is_empty()).then(|| icon.into());
        }
        definition.display.summary = edit_fields(&self.summary, &definition.display.summary)?;
        definition.display.details = edit_fields(&self.details, &definition.display.details)?;
        definition.display.elements = self
            .elements
            .iter()
            .map(ElementDraft::build)
            .collect::<Result<_, _>>()?;
        Ok(())
    }

    pub(crate) fn validated_appearance(&self) -> Result<WidgetAppearance, AppearanceInputError> {
        let accent = self.accent.trim();
        let mut appearance = WidgetAppearance {
            accent: (!accent.is_empty()).then(|| accent.into()),
            ..self.definition.display.appearance.clone()
        };
        if appearance.accent.is_some() && appearance.accent_rgb().is_none() {
            return Err(AppearanceInputError::Accent);
        }
        let width = self.width.trim();
        appearance.width = if width.is_empty() {
            None
        } else {
            Some(
                width
                    .parse::<u16>()
                    .ok()
                    .filter(|width| (MIN_WIDGET_WIDTH..=MAX_WIDGET_WIDTH).contains(width))
                    .ok_or(AppearanceInputError::Width)?,
            )
        };
        Ok(appearance)
    }

    fn update_url(&self, actions: &mut Vec<NamedAction>) {
        let previous = first_url(actions);
        if self.url == previous.map_or("", |(_, url)| url) {
            return;
        }
        let url = self.url.trim();
        match (previous.map(|(index, _)| index), url.is_empty()) {
            (Some(index), true) => {
                actions.remove(index);
            }
            (Some(index), false) => {
                actions[index].action = WidgetAction::OpenUrl { url: url.into() }
            }
            (None, false) => actions.push(NamedAction {
                id: self.new_url_action_id.clone(),
                label: "Open link".into(),
                action: WidgetAction::OpenUrl { url: url.into() },
            }),
            (None, true) => {}
        }
    }
}

fn first_url(actions: &[NamedAction]) -> Option<(usize, &str)> {
    actions
        .iter()
        .enumerate()
        .find_map(|(index, action)| match &action.action {
            WidgetAction::OpenUrl { url } => Some((index, url.as_str())),
            _ => None,
        })
}
