use super::runtime::SourceFields;
use daedalus_proto::WorkspaceInfo;
use daedalus_proto::workbench::{CommandOutput, WidgetResult, WorkspaceCommand};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;

const OUTPUT_LIMIT: usize = 64 * 1024;

pub(crate) async fn run(
    workspace: &WorkspaceInfo,
    spec: &WorkspaceCommand,
    output: &CommandOutput,
) -> SourceFields {
    match execute(workspace, spec, output).await {
        Ok(fields) => fields,
        Err(error) => SourceFields::from([("error".into(), error)]),
    }
}

async fn execute(
    workspace: &WorkspaceInfo,
    spec: &WorkspaceCommand,
    output: &CommandOutput,
) -> Result<SourceFields, String> {
    let (program, args) = spec
        .argv
        .split_first()
        .ok_or("The command has no executable")?;
    let mut process = tokio::process::Command::from(crate::background_command(program));
    process
        .args(args)
        .current_dir(&workspace.root_dir)
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .envs(&workspace.injected_env)
        .env("DAEDALUS_WORKSPACE_DIR", &workspace.root_dir)
        .env(
            "DAEDALUS_GIT_BRANCH",
            workspace.git_branch.as_deref().unwrap_or_default(),
        );
    for name in &spec.env {
        let value = workspace
            .injected_env
            .get(name)
            .map(std::ffi::OsString::from)
            .or_else(|| std::env::var_os(name))
            .ok_or_else(|| format!("Environment variable '{name}' is missing"))?;
        process.env(name, value);
    }
    let start = Instant::now();
    let mut child = process
        .spawn()
        .map_err(|error| format!("Could not start '{program}': {error}"))?;
    let stdout = child.stdout.take().ok_or("Could not read command output")?;
    let stderr = child.stderr.take().ok_or("Could not read command errors")?;
    let limit = Duration::from_secs(u64::from(spec.timeout_seconds.unwrap_or(15)));
    let execution =
        async { tokio::try_join!(child.wait(), read_output(stdout), read_output(stderr)) };
    let (status, stdout, stderr) = match tokio::time::timeout(limit, execution).await {
        Ok(result) => result.map_err(|error| error.to_string())?,
        Err(_) => {
            let _ = child.kill().await;
            return Err(format!("Command exceeded {} seconds", limit.as_secs()));
        }
    };
    let status_label = if status.success() { "Passed" } else { "Failed" };
    let mut fields = SourceFields::from([
        ("elapsed_ms".into(), start.elapsed().as_millis().to_string()),
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

async fn read_output(input: impl tokio::io::AsyncRead + Unpin) -> std::io::Result<String> {
    let mut bytes = Vec::new();
    input
        .take((OUTPUT_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() > OUTPUT_LIMIT {
        return Err(std::io::Error::other("Command output exceeded 64 KiB"));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
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
