use super::SourceFields;
use serde_json::Value;
use std::collections::BTreeMap;

/// Canonical selectors use a JSON Pointer after "json:". Keep dotted aliases
/// for existing widgets when they refer to exactly one value.
pub(super) fn project(fields: &mut SourceFields, value: &Value) {
    let mut aliases = BTreeMap::new();
    visit(fields, &mut aliases, "", "", value);
    for (alias, value) in aliases {
        if let Some(value) = value {
            fields.insert(alias, value);
        } else {
            fields.insert(
                format!("detail.Ambiguous JSON field {alias}"),
                "Use a JSON Pointer selector beginning with json:/.".into(),
            );
        }
    }
}

fn visit(
    fields: &mut SourceFields,
    aliases: &mut BTreeMap<String, Option<String>>,
    pointer: &str,
    dotted: &str,
    value: &Value,
) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                let segment = key.replace('~', "~0").replace('/', "~1");
                let pointer = format!("{pointer}/{segment}");
                let dotted = if dotted.is_empty() {
                    format!("json.{key}")
                } else {
                    format!("{dotted}.{key}")
                };
                visit(fields, aliases, &pointer, &dotted, value);
            }
        }
        Value::Array(array) => {
            for (index, value) in array.iter().enumerate() {
                visit(
                    fields,
                    aliases,
                    &format!("{pointer}/{index}"),
                    &format!("{dotted}.{index}"),
                    value,
                );
            }
        }
        Value::Null => {}
        value => {
            let value = value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_owned);
            fields.insert(format!("json:{pointer}"), value.clone());
            let alias = if dotted.is_empty() { "json" } else { dotted };
            aliases
                .entry(alias.into())
                .and_modify(|value| *value = None)
                .or_insert(Some(value));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbench::runtime::evaluate_widget;
    use daedalus_proto::StatusLevel;
    use daedalus_proto::workbench::{WidgetCondition, WidgetDefinition, WidgetField, WidgetRule};
    use serde_json::json;

    #[test]
    fn canonical_paths_distinguish_collisions_and_drive_rules() {
        let mut fields = SourceFields::new();
        project(
            &mut fields,
            &json!({
                "latency": { "p95": 120 },
                "latency.p95": 900,
                "list": [{"a~/b": "ready"}, null]
            }),
        );
        assert!(!fields.contains_key("json.latency.p95"));
        assert!(fields.contains_key("detail.Ambiguous JSON field json.latency.p95"));
        assert_eq!(fields["json:/latency/p95"], "120");
        assert_eq!(fields["json:/latency.p95"], "900");
        assert_eq!(fields["json:/list/0/a~0~1b"], "ready");
        assert_eq!(fields["json.list.0.a~/b"], "ready");
        assert!(!fields.contains_key("json:/list/1"));
        let definition = WidgetDefinition {
            rules: vec![WidgetRule {
                field: WidgetField {
                    source: "main".into(),
                    field: "json:/latency/p95".into(),
                    label: None,
                },
                condition: WidgetCondition::GreaterThan { value: 100 },
                level: StatusLevel::Warn,
            }],
            ..WidgetDefinition::default()
        };
        let result = evaluate_widget(
            &definition,
            &BTreeMap::from([("main".into(), fields)]),
            "Latency",
        );
        assert_eq!(result.level, StatusLevel::Warn);
    }

    #[test]
    fn arrays_keep_root_dotted_aliases() {
        let mut fields = SourceFields::new();
        project(&mut fields, &json!(["first"]));
        assert_eq!(fields[".0"], "first");
        assert_eq!(fields["json:/0"], "first");
    }
}
