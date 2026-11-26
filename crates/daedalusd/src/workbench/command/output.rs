use super::SourceFields;
use daedalus_proto::workbench::{CommandOutput, WidgetResult};
use std::process::ExitStatus;
use std::time::Duration;

pub(super) fn project_fields(
    output: &CommandOutput,
    status: ExitStatus,
    stdout: String,
    stderr: String,
    elapsed: Duration,
) -> Result<SourceFields, String> {
    let status_label = if status.success() { "Passed" } else { "Failed" };
    let mut fields = SourceFields::from([
        ("elapsed_ms".into(), elapsed.as_millis().to_string()),
        ("status".into(), status_label.into()),
        ("label".into(), status_label.into()),
        (
            "level".into(),
            if status.success() { "ok" } else { "error" }.into(),
        ),
    ]);
    if let Some(code) = status.code() {
        fields.insert("exit_code".into(), code.to_string());
    }
    if !stderr.trim().is_empty() && !status.success() {
        fields.insert("detail.stderr".into(), stderr);
    }
    match output {
        CommandOutput::ExitStatus => {}
        CommandOutput::Text { contains } => {
            if !stdout.trim().is_empty() {
                fields.insert("detail.Output".into(), stdout.trim_end().into());
            }
            if let Some(line) = stdout.lines().find(|line| {
                contains
                    .as_ref()
                    .is_none_or(|pattern| line.contains(pattern))
            }) {
                fields.insert("text".into(), line.into());
                fields.insert("label".into(), line.into());
            }
        }
        CommandOutput::Json => {
            let value = serde_json::from_str(&stdout)
                .map_err(|error| format!("Invalid command JSON: {error}"))?;
            read_json(&mut fields, "", &value);
        }
        CommandOutput::WidgetResult => {
            let result: WidgetResult = serde_json::from_str(&stdout)
                .map_err(|error| format!("Invalid widget result: {error}"))?;
            fields.insert("label".into(), result.label);
            let level = match result.level {
                daedalus_proto::StatusLevel::Info => "info",
                daedalus_proto::StatusLevel::Ok => "ok",
                daedalus_proto::StatusLevel::Warn => "warn",
                daedalus_proto::StatusLevel::Error => "error",
            };
            fields.insert("level".into(), level.into());
            fields.extend(
                result
                    .details
                    .into_iter()
                    .map(|(key, value)| (format!("detail.{key}"), value)),
            );
        }
    }
    Ok(fields)
}

fn read_json(fields: &mut SourceFields, path: &str, value: &serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => {
            for (key, value) in object {
                let path = if path.is_empty() {
                    format!("json.{key}")
                } else {
                    format!("{path}.{key}")
                };
                read_json(fields, &path, value);
            }
        }
        serde_json::Value::Array(array) => {
            for (index, value) in array.iter().enumerate() {
                read_json(fields, &format!("{path}.{index}"), value);
            }
        }
        serde_json::Value::Null => {}
        value => {
            let path = if path.is_empty() { "json" } else { path };
            fields.insert(
                path.into(),
                value
                    .as_str()
                    .map_or_else(|| value.to_string(), str::to_owned),
            );
        }
    }
}
