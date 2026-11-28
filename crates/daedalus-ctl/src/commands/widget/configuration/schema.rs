use crate::client::ClientError;
use serde_json::Value;

/// Check patch keys before merging so even misspelled null removals fail.
/// Serde checks values and required fields on the resulting definition.
pub(super) fn validate_definition(value: &Value) -> Result<(), ClientError> {
    check(value, "definition", Shape::Definition)
}

#[derive(Clone, Copy)]
enum Shape {
    Definition,
    Display,
    Appearance,
    Source,
    Rule,
    Field,
    Condition,
    Action,
    Element,
    Command,
    Refresh,
    Output,
    PaneConfig,
}

impl Shape {
    fn accepts(self, key: &str, kind: &str) -> bool {
        match self {
            Self::Definition => matches!(key, "sources" | "display" | "rules" | "actions"),
            Self::Display => matches!(
                key,
                "icon"
                    | "style"
                    | "primary_action"
                    | "summary"
                    | "details"
                    | "elements"
                    | "appearance"
            ),
            Self::Appearance => {
                matches!(key, "accent" | "width" | "density" | "surface" | "status")
            }
            Self::Rule => matches!(key, "field" | "condition" | "level"),
            Self::Field => matches!(key, "source" | "field" | "label"),
            Self::Command => matches!(key, "argv" | "env" | "timeout_seconds"),
            Self::Refresh => matches!(key, "interval_seconds" | "on_runner_completion"),
            Self::PaneConfig => {
                matches!(key, "preset" | "label" | "show_duration" | "show_warnings")
            }
            Self::Output => key == "kind" || (kind == "text" && key == "contains"),
            Self::Condition => {
                key == "kind"
                    || (matches!(kind, "equals" | "contains" | "greater_than") && key == "value")
            }
            Self::Source => {
                matches!(key, "id" | "kind")
                    || match kind {
                        "port" => matches!(key, "port" | "service"),
                        "container" => key == "name",
                        "runner" => key == "assignment",
                        "published" => key == "key",
                        "command" => matches!(key, "command" | "refresh" | "output"),
                        "pane" => matches!(key, "role" | "config"),
                        _ => false,
                    }
            }
            Self::Action => {
                matches!(key, "id" | "label" | "kind")
                    || match kind {
                        "terminal" | "logs" => key == "role",
                        "open_url" => key == "url",
                        "command" => matches!(key, "command" | "run_in_runner"),
                        _ => false,
                    }
            }
            Self::Element => {
                key == "kind"
                    || match kind {
                        "label" => key == "text",
                        "counter" => Self::Field.accepts(key, ""),
                        "progress" => key == "max" || Self::Field.accepts(key, ""),
                        "button" => key == "action",
                        _ => false,
                    }
            }
        }
    }

    fn child(self, key: &str) -> Option<(Self, bool)> {
        Some(match (self, key) {
            (Self::Definition, "display") => (Self::Display, false),
            (Self::Definition, "sources") => (Self::Source, true),
            (Self::Definition, "rules") => (Self::Rule, true),
            (Self::Definition, "actions") => (Self::Action, true),
            (Self::Display, "appearance") => (Self::Appearance, false),
            (Self::Display, "summary" | "details") => (Self::Field, true),
            (Self::Display, "elements") => (Self::Element, true),
            (Self::Rule, "field") => (Self::Field, false),
            (Self::Rule, "condition") => (Self::Condition, false),
            (Self::Source | Self::Action, "command") => (Self::Command, false),
            (Self::Source, "refresh") => (Self::Refresh, false),
            (Self::Source, "output") => (Self::Output, false),
            (Self::Source, "config") => (Self::PaneConfig, false),
            _ => return None,
        })
    }
}

fn check(value: &Value, path: &str, shape: Shape) -> Result<(), ClientError> {
    let Some(object) = value.as_object() else {
        return Ok(());
    };
    let kind = object
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default();
    for (key, value) in object {
        let path = format!("{path}.{key}");
        if !shape.accepts(key, kind) {
            return Err(format!("Unknown widget patch field '{path}'.").into());
        }
        if let Some((child, array)) = shape.child(key) {
            if array {
                for (index, item) in value.as_array().into_iter().flatten().enumerate() {
                    check(item, &format!("{path}[{index}]"), child)?;
                }
            } else {
                check(value, &path, child)?;
            }
        }
    }
    Ok(())
}
