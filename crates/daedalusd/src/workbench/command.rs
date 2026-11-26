mod json_fields;
mod output;
mod process_tree;

use super::runtime::SourceFields;
use daedalus_proto::WorkspaceInfo;
use daedalus_proto::workbench::{CommandOutput, WorkspaceCommand};
use process_tree::ProcessTree;
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
    let (program, mut process) = prepare_widget_command(workspace, spec)?;
    let start = Instant::now();
    let (mut child, process_tree) = ProcessTree::spawn(&mut process)
        .await
        .map_err(|error| format!("Could not start '{program}': {error}"))?;
    let stdout = child.stdout.take().ok_or("Could not read command output")?;
    let stderr = child.stderr.take().ok_or("Could not read command errors")?;
    let limit = Duration::from_secs(u64::from(spec.timeout_seconds.unwrap_or(15)));
    let execution = async {
        // Keep the process ID allocated while output is still open so cleanup
        // always addresses the original process group.
        let (stdout, stderr) = tokio::try_join!(read_output(stdout), read_output(stderr))?;
        let status = child.wait().await?;
        Ok::<_, std::io::Error>((status, stdout, stderr))
    };
    let (status, stdout, stderr) = match tokio::time::timeout(limit, execution).await {
        Ok(Ok(result)) => {
            process_tree.complete();
            result
        }
        Ok(Err(error)) => {
            drop(process_tree);
            let _ = child.kill().await;
            return Err(error.to_string());
        }
        Err(_) => {
            drop(process_tree);
            let _ = child.kill().await;
            return Err(format!("Command exceeded {} seconds", limit.as_secs()));
        }
    };
    output::project_fields(output, status, stdout, stderr, start.elapsed())
}

fn prepare_widget_command<'a>(
    workspace: &WorkspaceInfo,
    spec: &'a WorkspaceCommand,
) -> Result<(&'a str, tokio::process::Command), String> {
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
    Ok((program, process))
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

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(any(unix, windows))]
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
    async fn timed_out_command_stops_its_descendants() {
        let root = tempfile::tempdir().unwrap();
        let workspace = test_workspace(root.path().into());
        let mut spec = shell(
            "(trap '' HUP; touch ready; while [ ! -f release ]; do sleep 0.02; done; touch escaped) </dev/null >/dev/null 2>&1 & wait",
        );
        spec.timeout_seconds = Some(1);
        let fields = run(&workspace, &spec, &CommandOutput::ExitStatus).await;
        assert_eq!(fields["error"], "Command exceeded 1 seconds");
        assert_descendant_stopped(root.path()).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn output_limit_error_stops_command_descendants() {
        let root = tempfile::tempdir().unwrap();
        let workspace = test_workspace(root.path().into());
        let spec = shell(
            "(trap '' HUP; touch ready; while [ ! -f release ]; do sleep 0.02; done; touch escaped) </dev/null >/dev/null 2>&1 &
while [ ! -f ready ]; do sleep 0.02; done
head -c 65537 /dev/zero
wait",
        );
        let fields = run(&workspace, &spec, &CommandOutput::ExitStatus).await;
        assert_eq!(fields["error"], "Command output exceeded 64 KiB");
        assert_descendant_stopped(root.path()).await;
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn timed_out_command_stops_its_descendants() {
        let root = tempfile::tempdir().unwrap();
        let workspace = test_workspace(root.path().into());
        std::fs::write(
            root.path().join("descendant.ps1"),
            "New-Item ready -ItemType File
while (!(Test-Path release)) { Start-Sleep -Milliseconds 20 }
New-Item escaped -ItemType File",
        )
        .unwrap();
        let spec = WorkspaceCommand {
            argv: [
                "powershell.exe",
                "-NoProfile",
                "-Command",
                "$child = Start-Process -FilePath powershell.exe -ArgumentList '-NoProfile -ExecutionPolicy Bypass -File descendant.ps1' -NoNewWindow -PassThru; $child.WaitForExit()",
            ]
            .map(str::to_owned)
            .to_vec(),
            env: Vec::new(),
            timeout_seconds: Some(5),
        };
        let fields = run(&workspace, &spec, &CommandOutput::ExitStatus).await;
        assert_eq!(fields["error"], "Command exceeded 5 seconds");
        assert_descendant_stopped(root.path()).await;
    }

    #[cfg(any(unix, windows))]
    async fn assert_descendant_stopped(root: &std::path::Path) {
        let ready = root.join("ready").exists();
        std::fs::write(root.join("release"), "").unwrap();
        assert!(ready, "descendant never started");
        let escaped = root.join("escaped");
        let completed = tokio::time::timeout(Duration::from_millis(600), async {
            while !escaped.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(completed.is_err(), "descendant ran after the timeout");
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
