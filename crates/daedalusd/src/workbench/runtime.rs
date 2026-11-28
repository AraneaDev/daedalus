use daedalus_proto::StatusLevel;
use daedalus_proto::workbench::{WidgetCondition, WidgetDefinition, WidgetField, WidgetResult};
use std::collections::BTreeMap;

pub type SourceFields = BTreeMap<String, String>;
pub type WidgetInputs = BTreeMap<String, SourceFields>;

pub fn evaluate_widget(
    definition: &WidgetDefinition,
    sources: &WidgetInputs,
    name: &str,
) -> WidgetResult {
    let read = |field: &WidgetField| {
        sources
            .get(&field.source)
            .and_then(|source| source.get(&field.field))
    };
    let summary = definition
        .display
        .summary
        .iter()
        .filter_map(|field| {
            let value = read(field)?;
            Some(match &field.label {
                Some(label) => format!("{label} {value}"),
                None => value.clone(),
            })
        })
        .collect::<Vec<_>>();
    let mut details = definition
        .display
        .details
        .iter()
        .filter_map(|field| {
            Some((
                field.label.as_ref().unwrap_or(&field.field).clone(),
                read(field)?.clone(),
            ))
        })
        .collect::<Vec<_>>();
    let reported_label = sources.values().find_map(|fields| fields.get("label"));
    let label = if summary.is_empty() {
        reported_label.cloned().unwrap_or_else(|| name.to_owned())
    } else {
        summary.join(" · ")
    };
    let mut level = if sources.values().any(|fields| fields.contains_key("error")) {
        StatusLevel::Error
    } else {
        StatusLevel::Info
    };
    if level == StatusLevel::Info {
        level = sources
            .values()
            .filter_map(|fields| match fields.get("level")?.as_str() {
                "ok" => Some(StatusLevel::Ok),
                "warn" => Some(StatusLevel::Warn),
                "error" => Some(StatusLevel::Error),
                _ => None,
            })
            .max_by_key(|level| match level {
                StatusLevel::Info => 0,
                StatusLevel::Ok => 1,
                StatusLevel::Warn => 2,
                StatusLevel::Error => 3,
            })
            .unwrap_or(StatusLevel::Info);
        if let Some(rule) = definition
            .rules
            .iter()
            .find(|rule| condition_matches(&rule.condition, read(&rule.field)))
        {
            level = rule.level;
        }
    }
    for (source, fields) in sources {
        if let Some(error) = fields.get("error") {
            details.push((source.clone(), error.clone()));
        }
        for (key, value) in fields
            .range("detail.".to_owned()..)
            .take_while(|(key, _)| key.starts_with("detail."))
        {
            details.push((key["detail.".len()..].to_owned(), value.clone()));
        }
    }
    let mut element_values = WidgetInputs::new();
    for field in definition
        .display
        .elements
        .iter()
        .filter_map(|element| element.value_field())
    {
        if let Some(value) = read(field) {
            element_values
                .entry(field.source.clone())
                .or_default()
                .insert(field.field.clone(), value.clone());
        }
    }
    WidgetResult {
        label,
        level,
        details,
        element_values,
    }
}

fn condition_matches(condition: &WidgetCondition, actual: Option<&String>) -> bool {
    match condition {
        WidgetCondition::Equals { value } => actual == Some(value),
        WidgetCondition::Contains { value } => actual.is_some_and(|actual| actual.contains(value)),
        WidgetCondition::GreaterThan { value } => actual
            .and_then(|actual| actual.parse::<i64>().ok())
            .is_some_and(|actual| actual > *value),
        WidgetCondition::Missing => actual.is_none(),
    }
}
