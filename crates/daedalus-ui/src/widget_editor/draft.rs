use super::fields::{edit_fields, format_fields};
use daedalus_proto::workbench::{
    CommandOutput, NamedAction, NamedSource, RefreshPolicy, WidgetAction, WidgetDefinition,
    WidgetInstance, WidgetSource, WidgetTemplate, WorkspaceCommand,
};

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
        self.url =
            first_url(&self.definition.actions).map_or_else(String::new, |(_, url)| url.into());
    }

    pub fn build(&self) -> Result<WidgetInstance, String> {
        let mut widget = self.widget.clone();
        widget.name = widget.name.trim().into();
        if widget.name.is_empty() {
            return Err("Enter a name for the widget.".into());
        }
        let mut definition = self.definition.clone();
        self.update_source(&mut definition)?;
        self.update_display(&mut definition)?;
        self.update_url(&mut definition.actions);
        widget.definition = Some(definition);
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
        definition.display.summary = edit_fields(&self.summary, &definition.display.summary)?;
        definition.display.details = edit_fields(&self.details, &definition.display.details)?;
        Ok(())
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
