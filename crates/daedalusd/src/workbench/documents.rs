use daedalus_proto::validate_assignments;
use daedalus_proto::workbench::{
    TemplateDocument, WORKSPACE_FORMAT_VERSION, WidgetAction, WidgetDefinition, WidgetSource,
    WidgetTemplate, WorkspaceCommand, WorkspaceDocument,
};
use std::collections::HashSet;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DocumentError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Parse(#[from] toml::de::Error),
    #[error("{0}")]
    Serialize(#[from] toml::ser::Error),
}

pub fn parse_workspace(input: &str) -> Result<WorkspaceDocument, DocumentError> {
    let value: toml::Value = toml::from_str(input)?;
    for (key, label) in [("widgets", "widget"), ("templates", "template")] {
        if let Some(items) = value.get(key).and_then(toml::Value::as_array) {
            for item in items {
                validate_refresh_keys(item, label)?;
            }
        }
    }
    let document = toml::from_str(input)?;
    validate_workspace(&document)?;
    Ok(document)
}

pub fn parse_template(input: &str) -> Result<TemplateDocument, DocumentError> {
    let value: toml::Value = toml::from_str(input)?;
    if let Some(template) = value.get("template") {
        validate_refresh_keys(template, "template")?;
    }
    let document: TemplateDocument = toml::from_str(input)?;
    validate_version(document.version)?;
    validate_template(&document.template)?;
    Ok(document)
}

// Portable TOML should diagnose spelling errors before decoding drops the field.
fn validate_refresh_keys(owner: &toml::Value, owner_kind: &str) -> Result<(), DocumentError> {
    let Some(sources) = owner
        .get("definition")
        .and_then(|definition| definition.get("sources"))
        .and_then(toml::Value::as_array)
    else {
        return Ok(());
    };
    for source in sources {
        if source.get("kind").and_then(toml::Value::as_str) != Some("command") {
            continue;
        }
        let Some(refresh) = source.get("refresh").and_then(toml::Value::as_table) else {
            continue;
        };
        if let Some(key) = refresh
            .keys()
            .find(|key| !matches!(key.as_str(), "interval_seconds" | "on_runner_completion"))
        {
            let owner_id = owner
                .get("id")
                .and_then(toml::Value::as_str)
                .unwrap_or("<missing>");
            let source_id = source
                .get("id")
                .and_then(toml::Value::as_str)
                .unwrap_or("<missing>");
            return invalid(format!(
                "{owner_kind} '{owner_id}' source '{source_id}' has unknown refresh key '{key}'"
            ));
        }
    }
    Ok(())
}

pub fn serialize_workspace(document: &WorkspaceDocument) -> Result<String, DocumentError> {
    validate_workspace(document)?;
    Ok(toml::to_string_pretty(document)?)
}

pub fn serialize_template(document: &TemplateDocument) -> Result<String, DocumentError> {
    validate_version(document.version)?;
    validate_template(&document.template)?;
    Ok(toml::to_string_pretty(document)?)
}

pub fn validate_workspace(document: &WorkspaceDocument) -> Result<(), DocumentError> {
    validate_version(document.version)?;
    validate_name(&document.name, "workspace")?;
    validate_assignments(&document.assignments).map_err(DocumentError::Invalid)?;
    for (split, proportion) in &document.layout.splits {
        if !(1..1000).contains(proportion) {
            return invalid(format!("layout split '{split}' must be between 1 and 999"));
        }
    }

    let mut templates = HashSet::new();
    for template in &document.templates {
        insert_id(&mut templates, &template.id, "template")?;
        validate_template(template)?;
    }
    let mut widgets = HashSet::new();
    for widget in &document.widgets {
        insert_id(&mut widgets, &widget.id, "widget")?;
        validate_name(&widget.name, &format!("widget '{}'", widget.id))?;
        if let Some(template) = widget.template.as_ref() {
            if !templates.contains(template.as_str()) {
                return invalid(format!(
                    "widget '{}' has unknown template '{template}'",
                    widget.id
                ));
            }
        } else if widget.definition.is_none() {
            return invalid(format!(
                "widget '{}' requires a template or a definition",
                widget.id
            ));
        }
        if let Some(definition) = widget.definition.as_ref() {
            validate_definition(definition, &format!("widget '{}'", widget.id))?;
        }
    }
    Ok(())
}

pub fn validate_template(template: &WidgetTemplate) -> Result<(), DocumentError> {
    validate_id(&template.id, "template")?;
    let path = format!("template '{}'", template.id);
    validate_name(&template.name, &path)?;
    validate_definition(&template.definition, &path)
}

pub fn validate_definition(definition: &WidgetDefinition, path: &str) -> Result<(), DocumentError> {
    let mut sources = HashSet::new();
    for named in &definition.sources {
        insert_id(&mut sources, &named.id, &format!("source in {path}"))?;
        if let WidgetSource::Command {
            command, refresh, ..
        } = &named.source
        {
            let source = format!("{path} source '{}'", named.id);
            validate_command(command, &source)?;
            if refresh.interval_seconds == Some(0) {
                return invalid(format!("{source} refresh interval must be positive"));
            }
        }
    }
    for field in definition
        .display
        .summary
        .iter()
        .chain(&definition.display.details)
        .chain(definition.rules.iter().map(|rule| &rule.field))
    {
        if !sources.contains(field.source.as_str()) {
            return invalid(format!(
                "{path} references unknown source '{}'",
                field.source
            ));
        }
        if field.field.trim().is_empty() {
            return invalid(format!(
                "{path} requires a field name for source '{}'",
                field.source
            ));
        }
    }
    let mut actions = HashSet::new();
    for action in &definition.actions {
        insert_id(&mut actions, &action.id, &format!("action in {path}"))?;
        validate_name(&action.label, &format!("{path} action '{}'", action.id))?;
        if let WidgetAction::Command { command } = &action.action {
            validate_command(command, &format!("{path} action '{}'", action.id))?;
        }
    }
    Ok(())
}

fn validate_command(command: &WorkspaceCommand, path: &str) -> Result<(), DocumentError> {
    if command.argv.first().is_none_or(|arg| arg.trim().is_empty())
        || command.argv.iter().any(|arg| arg.contains('\0'))
    {
        return invalid(format!("{path} requires an executable and valid arguments"));
    }
    if command.timeout_seconds == Some(0) {
        return invalid(format!("{path} command timeout must be positive"));
    }
    let mut names = HashSet::new();
    for name in &command.env {
        let mut bytes = name.bytes();
        if !bytes
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
            || !bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
            || !names.insert(name)
        {
            return invalid(format!(
                "{path} has an invalid or duplicate environment variable name"
            ));
        }
    }
    Ok(())
}

fn validate_version(version: u32) -> Result<(), DocumentError> {
    if version != WORKSPACE_FORMAT_VERSION {
        return invalid(format!("unsupported workbench document version {version}"));
    }
    Ok(())
}

fn validate_name(name: &str, kind: &str) -> Result<(), DocumentError> {
    if name.trim().is_empty() || name.chars().any(char::is_control) {
        return invalid(format!("{kind} requires a valid name"));
    }
    Ok(())
}

fn insert_id<'a>(ids: &mut HashSet<&'a str>, id: &'a str, kind: &str) -> Result<(), DocumentError> {
    validate_id(id, kind)?;
    if !ids.insert(id) {
        return invalid(format!("duplicate {kind} ID '{id}'"));
    }
    Ok(())
}

fn validate_id(id: &str, kind: &str) -> Result<(), DocumentError> {
    if id.is_empty()
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
    {
        return invalid(format!("invalid {kind} ID '{id}'"));
    }
    Ok(())
}

fn invalid<T>(message: String) -> Result<T, DocumentError> {
    Err(DocumentError::Invalid(message))
}
