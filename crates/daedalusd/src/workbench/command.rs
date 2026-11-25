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

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::workbench::test_workspace;

    #[tokio::test]
    async fn output_reader_accepts_exact_limit_and_rejects_one_more_byte() {
        let mut input = vec![b'x'; OUTPUT_LIMIT];
        assert_eq!(
            read_output(input.as_slice()).await.unwrap().len(),
            OUTPUT_LIMIT
        );
        input.push(b'x');
        assert_eq!(
            read_output(input.as_slice()).await.unwrap_err().to_string(),
            "Command output exceeded 64 KiB"
        );
    }

    #[cfg(unix)]
    fn shell(script: &str) -> WorkspaceCommand {
        WorkspaceCommand {
            argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
            env: Vec::new(),
            timeout_seconds: Some(5),
        }
    }

    #[cfg(unix)]
    async fn output_text(
        workspace: &WorkspaceInfo,
        text: &str,
        output: &CommandOutput,
    ) -> SourceFields {
        let mut spec = shell(r#"printf '%s' "$1""#);
        spec.argv.extend(["widget-fixture".into(), text.into()]);
        run(workspace, &spec, output).await
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn text_command_observes_workspace_and_reports_failed_exit_with_stderr() {
        let root = tempfile::tempdir().unwrap();
        let mut workspace = test_workspace(root.path().into());
        workspace.git_branch = Some("feature/test".into());
        workspace
            .injected_env
            .insert("WIDGET_COLOR".into(), "blue".into());
        std::fs::write(root.path().join("status"), "up").unwrap();

        let mut spec = shell(
            r#"printf 'ignored\nstatus: %s / %s / %s\n' "$(cat status)" "$WIDGET_COLOR" "$DAEDALUS_GIT_BRANCH"
               test -f "$DAEDALUS_WORKSPACE_DIR/status" || exit 99
               printf 'runner failed\n' >&2
               exit 7"#,
        );
        spec.env.push("WIDGET_COLOR".into());
        let fields = run(
            &workspace,
            &spec,
            &CommandOutput::Text {
                contains: Some("status:".into()),
            },
        )
        .await;
        assert_eq!(fields["label"], "status: up / blue / feature/test");
        assert_eq!(fields["text"], fields["label"]);
        assert_eq!(
            fields["detail.Output"],
            format!("ignored\n{}", fields["label"])
        );
        assert_eq!(fields["detail.stderr"], "runner failed\n");
        assert_eq!(fields["exit_code"], "7");
        assert_eq!(fields["status"], "Failed");
        assert_eq!(fields["level"], "error");
        assert!(fields["elapsed_ms"].parse::<u64>().is_ok());

        let spec = shell("echo hidden output; echo hidden warning >&2");
        let fields = run(&workspace, &spec, &CommandOutput::ExitStatus).await;
        assert_eq!(fields["label"], "Passed");
        assert_eq!(fields["exit_code"], "0");
        assert!(!fields.contains_key("detail.stderr"));
        assert!(!fields.contains_key("detail.Output"));

        let fields = run(
            &workspace,
            &spec,
            &CommandOutput::Text {
                contains: Some("absent match".into()),
            },
        )
        .await;
        assert_eq!(fields["label"], "Passed");
        assert_eq!(fields["detail.Output"], "hidden output");
        assert!(!fields.contains_key("text"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_reports_configuration_spawn_and_stream_limit_errors() {
        let root = tempfile::tempdir().unwrap();
        let workspace = test_workspace(root.path().into());
        let mut spec = shell(":");
        let absent = format!("DAEDALUS_TEST_UNSET_{}", workspace.id.0.simple());
        spec.env.push(absent.clone());
        let fields = run(&workspace, &spec, &CommandOutput::ExitStatus).await;
        assert_eq!(
            fields["error"],
            format!("Environment variable '{absent}' is missing")
        );

        spec.env.clear();
        spec.argv = vec![
            root.path()
                .join("not-installed")
                .to_string_lossy()
                .into_owned(),
        ];
        let fields = run(&workspace, &spec, &CommandOutput::ExitStatus).await;
        assert!(fields["error"].contains("Could not start"));
        assert!(fields["error"].contains("not-installed"));

        spec.argv.clear();
        let fields = run(&workspace, &spec, &CommandOutput::ExitStatus).await;
        assert_eq!(fields["error"], "The command has no executable");

        for script in ["head -c 65537 /dev/zero", "head -c 65537 /dev/zero >&2"] {
            let fields = run(&workspace, &shell(script), &CommandOutput::ExitStatus).await;
            assert_eq!(fields["error"], "Command output exceeded 64 KiB");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_command_that_never_exits_hits_its_timeout() {
        let root = tempfile::tempdir().unwrap();
        let workspace = test_workspace(root.path().into());
        let mut spec = shell("exec sleep 30");
        spec.timeout_seconds = Some(1);
        let fields = tokio::time::timeout(
            Duration::from_secs(10),
            run(&workspace, &spec, &CommandOutput::ExitStatus),
        )
        .await
        .expect("command did not return after its configured timeout");
        assert_eq!(fields["error"], "Command exceeded 1 seconds");
        assert!(!fields.contains_key("exit_code"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_decodes_nested_json_scalar_values_and_structured_widget_details() {
        let root = tempfile::tempdir().unwrap();
        let workspace = test_workspace(root.path().into());

        let fields = output_text(
            &workspace,
            r#"{"suite":{"counts":[4,null,2],"ready":true,"name":"Smoke"}}"#,
            &CommandOutput::Json,
        )
        .await;
        assert_eq!(fields["status"], "Passed");
        assert_eq!(fields["json.suite.counts.0"], "4");
        assert!(!fields.contains_key("json.suite.counts.1"));
        assert_eq!(fields["json.suite.counts.2"], "2");
        assert_eq!(fields["json.suite.ready"], "true");
        assert_eq!(fields["json.suite.name"], "Smoke");
        let scalar = output_text(&workspace, "42", &CommandOutput::Json).await;
        assert_eq!(scalar["json"], "42");

        for level in ["info", "ok", "warn", "error"] {
            let report = format!(
                r#"{{"label":"Integration report","level":"{level}","details":[["Tests","8 passed"]]}}"#
            );
            let fields = output_text(&workspace, &report, &CommandOutput::WidgetResult).await;
            assert_eq!(fields["label"], "Integration report");
            assert_eq!(fields["level"], level);
            assert_eq!(fields["detail.Tests"], "8 passed");
            assert_eq!(fields["exit_code"], "0");
        }

        for (output, prefix) in [
            (CommandOutput::Json, "Invalid command JSON:"),
            (CommandOutput::WidgetResult, "Invalid widget result:"),
        ] {
            let fields = output_text(&workspace, "not JSON", &output).await;
            assert_eq!(fields.len(), 1);
            assert!(fields["error"].starts_with(prefix));
        }
    }
}
