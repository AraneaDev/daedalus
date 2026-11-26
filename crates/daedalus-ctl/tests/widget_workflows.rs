use daedalusd::server::DaemonServer;
use daedalusd::supervisor::{IsolationMode, Supervisor};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use tempfile::TempDir;
use tokio::process::Command;
use tokio::sync::oneshot;

struct Workbench {
    directory: TempDir,
    socket: PathBuf,
    shutdown: oneshot::Sender<()>,
    daemon: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Workbench {
    async fn start() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("daemon.sock");
        #[cfg(windows)]
        let isolation = IsolationMode::WindowsProcessTree;
        #[cfg(not(windows))]
        let isolation = IsolationMode::PgidFallback;
        let server = DaemonServer::bind_with_supervisor(
            socket.clone(),
            directory.path().join("runtime"),
            Supervisor::with_mode(isolation),
        )
        .await
        .unwrap();
        let (shutdown, done) = oneshot::channel();
        let daemon = tokio::spawn(server.serve(async {
            let _ = done.await;
        }));
        Self {
            directory,
            socket,
            shutdown,
            daemon,
        }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.directory.path().join(name)
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_daedalus-ctl"));
        command
            .arg("--socket")
            .arg(&self.socket)
            .args(args)
            .env_remove("DAEDALUS_AUTH_TOKEN")
            .kill_on_drop(true);
        command
    }

    async fn run(&self, args: &[&str]) -> String {
        output(self.command(args).output().await.unwrap())
    }

    async fn reject(&self, args: &[&str], expected: &str) -> String {
        let result = self.command(args).output().await.unwrap();
        assert!(!result.status.success(), "unexpected success: {args:?}");
        let error = String::from_utf8_lossy(&result.stderr);
        assert!(error.contains(expected), "{error}");
        String::from_utf8(result.stdout).unwrap()
    }

    async fn workspace(&self, root: &Path) -> String {
        self.run(&[
            "workspace",
            "create",
            "Widget suite",
            "--dir",
            root.to_str().unwrap(),
        ])
        .await
        .trim()
        .into()
    }

    async fn finish(self) {
        self.shutdown.send(()).unwrap();
        self.daemon.await.unwrap().unwrap();
    }
}

fn output(result: Output) -> String {
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap()
}

const TEMPLATE: &str = r#"
version = 1
[template]
id = "review"
name = "Review"
[[template.definition.sources]]
id = "review"
kind = "published"
key = "review"
[[template.definition.display.details]]
source = "review"
field = "detail"
[[template.definition.actions]]
id = "open"
label = "Open review"
kind = "open_url"
url = "https://example.invalid/review"
"#;

#[tokio::test]
async fn template_library_updates_leave_existing_workspaces_usable() {
    use tokio::io::AsyncWriteExt;

    let app = Workbench::start().await;
    let template = app.file("review.toml");
    std::fs::write(&template, TEMPLATE).unwrap();
    let file = template.to_str().unwrap();
    let root = app.directory.path().to_str().unwrap();
    let workspace = app.workspace(app.directory.path()).await;
    let id = workspace.as_str();

    assert!(
        app.run(&["widget", "template", "list"])
            .await
            .contains("Git Worktree")
    );
    app.run(&["widget", "template", "create", "--file", file])
        .await;
    app.reject(
        &["widget", "template", "import", "--file", file],
        "already exists",
    )
    .await;
    app.run(&[
        "widget", "template", "apply", "review", "first", "--dir", root,
    ])
    .await;
    let exported = app
        .run(&["widget", "template", "export", "review", "--file", "-"])
        .await;
    assert!(exported.contains("example.invalid/review"));
    std::fs::write(&template, TEMPLATE.replace("/review", "/updated")).unwrap();
    app.run(&["widget", "template", "edit", "--file", file])
        .await;
    let library: Value =
        serde_json::from_str(&app.run(&["widget", "template", "list", "--json"]).await).unwrap();
    assert_eq!(library["revision"], 2);
    app.run(&[
        "widget",
        "template",
        "apply",
        "review",
        "second",
        "--workspace",
        id,
    ])
    .await;
    for (widget, path) in [("first", "/review"), ("second", "/updated")] {
        let result = app
            .run(&["widget", "action", widget, "open", "--workspace", id])
            .await;
        assert!(result.contains(&format!("https://example.invalid{path}")));
    }

    app.run(&[
        "widget",
        "create",
        "blank",
        "--workspace",
        id,
        "--name",
        "Editable",
    ])
    .await;
    let shown: Value = serde_json::from_str(
        &app.run(&["widget", "show", "blank", "--workspace", id])
            .await,
    )
    .unwrap();
    assert_eq!(shown["name"], "Editable");
    app.run(&[
        "widget",
        "edit",
        "blank",
        "--file",
        file,
        "--name",
        "Override",
        "--workspace",
        id,
    ])
    .await;
    app.reject(
        &["widget", "move", "blank", "0", "--workspace", id],
        "outside the widget list",
    )
    .await;
    app.reject(
        &["widget", "remove", "missing", "--workspace", id],
        "No widget with ID",
    )
    .await;
    app.reject(
        &["widget", "template", "export", "missing"],
        "No template with ID",
    )
    .await;

    let mut input = app
        .command(&[
            "widget",
            "create",
            "stdin",
            "--file",
            "-",
            "--workspace",
            id,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    input
        .stdin
        .take()
        .unwrap()
        .write_all(TEMPLATE.as_bytes())
        .await
        .unwrap();
    assert!(output(input.wait_with_output().await.unwrap()).contains("saved at revision"));
    app.run(&[
        "status",
        "publish",
        "--workspace",
        id,
        "--source",
        "review",
        "--label",
        "Approved",
        "--detail",
        "All checks passed",
    ])
    .await;
    let preview = app
        .run(&["widget", "preview", "--file", file, "--workspace", id])
        .await;
    assert!(preview.lines().any(|line| line == "Approved"));
    assert!(preview.contains("All checks passed"));
    let results: Value = serde_json::from_str(
        &app.run(&["widget", "refresh", "stdin", "--workspace", id, "--json"])
            .await,
    )
    .unwrap();
    assert_eq!(results.as_array().unwrap().len(), 1);
    assert_eq!(results[0]["result"]["label"], "Approved");
    app.finish().await;
}

#[tokio::test]
async fn repo_updates_require_a_choice_before_replacing_concurrent_local_edits() {
    let app = Workbench::start().await;
    let project = app.directory.path();
    let root = project.to_str().unwrap();
    let id = app.workspace(project).await;
    let suffix = ["--workspace", id.as_str()];
    let config = app.file(".daedalus-workspace.toml");
    std::fs::write(&config, "version = 1\nname = 'Shared default'\n").unwrap();
    app.reject(
        &[&["workspace", "update"][..], &suffix].concat(),
        "no repo link",
    )
    .await;
    let preview = app
        .run(&["workspace", "import", "--repo", "--new", root, "--dry-run"])
        .await;
    assert!(preview.contains("Create Shared default"));
    let preview: Value = serde_json::from_str(
        &app.run(&[
            "workspace",
            "import",
            "--repo",
            "--new",
            root,
            "--dry-run",
            "--json",
        ])
        .await,
    )
    .unwrap();
    assert_eq!(preview["incoming"]["name"], "Shared default");
    app.run(&[&["workspace", "import", "--repo"][..], &suffix].concat())
        .await;

    let imports = ["workspace", "import", "--repo"];
    // Link a base with two widgets, then edit their names on both sides.
    app.run(
        &[
            &["widget", "create", "branch", "--template", "git-worktree"][..],
            &suffix,
        ]
        .concat(),
    )
    .await;
    app.run(
        &[
            &[
                "widget",
                "create",
                "other",
                "--template",
                "git-worktree",
                "--name",
                "Base other",
            ][..],
            &suffix,
        ]
        .concat(),
    )
    .await;
    app.run(
        &[
            &["workspace", "export", "--file", config.to_str().unwrap()][..],
            &suffix,
        ]
        .concat(),
    )
    .await;
    app.run(&[&imports[..], &suffix].concat()).await;
    let contents = std::fs::read_to_string(&config).unwrap();
    app.run(
        &[
            &["widget", "edit", "branch", "--name", "Local branch"][..],
            &suffix,
        ]
        .concat(),
    )
    .await;
    app.run(
        &[
            &["widget", "edit", "other", "--name", "Local other"][..],
            &suffix,
        ]
        .concat(),
    )
    .await;
    let remote = contents
        .replace("name = \"Git Worktree\"", "name = \"Incoming branch\"")
        .replace("name = \"Base other\"", "name = \"Incoming other\"");
    std::fs::write(&config, &remote).unwrap();
    let update = ["workspace", "update"];
    let dry_run = app
        .run(&[&update[..], &suffix, &["--dry-run"]].concat())
        .await;
    assert!(dry_run.contains("Conflict at /widgets/branch/name"));
    let configure = [&["workspace", "configure", "--json"][..], &suffix].concat();
    let before = app.run(&configure).await;
    let partial = [
        &update[..],
        &suffix,
        &["--resolve", "/widgets/branch/name=incoming"],
    ]
    .concat();
    let partial_preview: serde_json::Value = serde_json::from_str(
        &app.run(&[partial.as_slice(), &["--dry-run", "--json"]].concat())
            .await,
    )
    .unwrap();
    assert_eq!(
        partial_preview["proposed"]["widgets"][0]["name"],
        "Incoming branch"
    );
    assert_eq!(partial_preview["conflicts"].as_array().unwrap().len(), 1);
    assert_eq!(
        partial_preview["conflicts"][0]["path"],
        "/widgets/other/name"
    );
    let resolved = [
        partial.as_slice(),
        &["--resolve", "/widgets/other/name=local"],
    ]
    .concat();
    let resolved_preview: serde_json::Value = serde_json::from_str(
        &app.run(&[resolved.as_slice(), &["--dry-run", "--json"]].concat())
            .await,
    )
    .unwrap();
    assert_eq!(resolved_preview["conflicts"], serde_json::json!([]));
    assert_eq!(app.run(&configure).await, before);
    let blocked = app
        .reject(
            &[&update[..], &suffix, &["--json"]].concat(),
            "Resolve the listed conflicts",
        )
        .await;
    assert!(blocked.contains("Local branch") && blocked.contains("Incoming branch"));
    app.reject(
        &[&update[..], &suffix, &["--resolve", "missing"]].concat(),
        "Use /field/path",
    )
    .await;
    app.reject(
        &[
            &update[..],
            &suffix,
            &["--resolve", "/widgets/branch/name=invalid"],
        ]
        .concat(),
        "Choose local or incoming",
    )
    .await;
    app.run(&resolved).await;
    let applied: serde_json::Value = serde_json::from_str(&app.run(&configure).await).unwrap();
    assert_eq!(applied["document"], resolved_preview["proposed"]);
    std::fs::write(
        &config,
        remote.replace("name = \"Incoming branch\"", "name = \"Another incoming\""),
    )
    .unwrap();
    app.run(
        &[
            &update[..],
            &suffix,
            &["--resolve", "/widgets/branch/name=incoming"],
        ]
        .concat(),
    )
    .await;
    assert!(
        app.run(&[&["workspace", "configure", "--json"][..], &suffix].concat())
            .await
            .contains("Another incoming")
    );
    app.run(&[&["workspace", "import", "--repo", "--copy"][..], &suffix].concat())
        .await;
    app.reject(&[&update[..], &suffix].concat(), "no repo link")
        .await;
    app.finish().await;
}

#[tokio::test]
async fn linked_import_applies_the_whole_chosen_variant_and_keeps_independent_labels() {
    let app = Workbench::start().await;
    let workspace = app.workspace(app.directory.path()).await;
    let target = ["--workspace", workspace.as_str()];
    let config = app.file(".daedalus-workspace.toml");
    let base = r#"version = 1
name = "Workbench"
[[widgets]]
id = "fixture"
name = "Fixture"
pinned = true
[[widgets.definition.sources]]
id = "main"
kind = "published"
key = "old"
[[widgets.definition.actions]]
id = "open"
label = "Open"
kind = "open_url"
url = "https://example.invalid/old"
"#;
    std::fs::write(&config, base).unwrap();
    app.run(&[&["workspace", "import", "--repo"][..], &target].concat())
        .await;

    let template = app.file("local.toml");
    std::fs::write(
        &template,
        r#"version = 1
[template]
id = "fixture"
name = "Fixture"
[[template.definition.sources]]
id = "main"
kind = "git"
[[template.definition.actions]]
id = "open"
label = "Local label"
kind = "diff"
"#,
    )
    .unwrap();
    app.run(
        &[
            &[
                "widget",
                "edit",
                "fixture",
                "--file",
                template.to_str().unwrap(),
            ][..],
            &target,
        ]
        .concat(),
    )
    .await;
    std::fs::write(
        config,
        base.replace("key = \"old\"", "key = \"incoming\"")
            .replace("example.invalid/old", "example.invalid/new"),
    )
    .unwrap();

    let update = [&["workspace", "update", "--json"][..], &target].concat();
    let dry_run = [update.as_slice(), &["--dry-run"]].concat();
    let preview: Value = serde_json::from_str(&app.run(&dry_run).await).unwrap();
    let paths = preview["conflicts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|conflict| conflict["path"].as_str().unwrap())
        .collect::<Vec<_>>();
    let source = "/widgets/fixture/definition/sources/main/$variant";
    let action = "/widgets/fixture/definition/actions/open/$variant";
    assert_eq!(paths, [action, source]);

    let local_choice = format!("{action}=local");
    let source_choice = format!("{source}=incoming");
    let local_preview: Value = serde_json::from_str(
        &app.run(
            &[
                dry_run.as_slice(),
                &["--resolve", &source_choice, "--resolve", &local_choice],
            ]
            .concat(),
        )
        .await,
    )
    .unwrap();
    assert_eq!(local_preview["conflicts"], serde_json::json!([]));
    assert_eq!(
        local_preview["proposed"]["widgets"][0]["definition"]["actions"][0]["kind"],
        "diff"
    );

    let action_choice = format!("{action}=incoming");
    let apply = [
        update.as_slice(),
        &["--resolve", &source_choice, "--resolve", &action_choice],
    ]
    .concat();
    let incoming_preview: Value =
        serde_json::from_str(&app.run(&[apply.as_slice(), &["--dry-run"]].concat()).await).unwrap();
    let saved: Value = serde_json::from_str(&app.run(&apply).await).unwrap();
    assert_eq!(incoming_preview["conflicts"], serde_json::json!([]));
    assert_eq!(incoming_preview["proposed"], saved["document"]);
    let definition = &saved["document"]["widgets"][0]["definition"];
    assert_eq!(
        definition["sources"][0],
        serde_json::json!({"id": "main", "kind": "published", "key": "incoming"})
    );
    assert_eq!(
        definition["actions"][0],
        serde_json::json!({"id": "open", "kind": "open_url", "label": "Local label", "url": "https://example.invalid/new"})
    );
    app.finish().await;
}

#[cfg(unix)]
#[tokio::test]
async fn resolving_one_template_conflict_can_clear_its_dependent_conflicts() {
    use std::os::unix::fs::PermissionsExt;

    let app = Workbench::start().await;
    let workspace = app.workspace(app.directory.path()).await;
    let config = app.file(".daedalus-workspace.toml");
    let start = "version = 1\nname = 'Fixture'\n";
    let template =
        "\n[[templates]]\nid = 'fixture'\nname = 'Base template'\n[templates.definition]\n";
    std::fs::write(&config, format!("{start}{template}")).unwrap();
    app.run(&["workspace", "import", "--repo", "--workspace", &workspace])
        .await;

    let local = app.file("local.toml");
    std::fs::write(&local, start).unwrap();
    let editor = app.file("editor");
    std::fs::write(&editor, "#!/bin/sh\ncp \"$FIXTURE_LOCAL\" \"$1\"\n").unwrap();
    std::fs::set_permissions(&editor, std::fs::Permissions::from_mode(0o700)).unwrap();
    output(
        app.command(&[
            "workspace",
            "configure",
            "--edit",
            "--workspace",
            &workspace,
        ])
        .env("VISUAL", &editor)
        .env("FIXTURE_LOCAL", &local)
        .output()
        .await
        .unwrap(),
    );

    let changed = template.replace("Base template", "Incoming template");
    let widget = "\n[[widgets]]\nid = 'ref'\nname = 'Template widget'\npinned = true\ntemplate = 'fixture'\n";
    std::fs::write(&config, format!("{start}{changed}{widget}")).unwrap();
    let update = ["workspace", "update", "--workspace", &workspace, "--json"];
    let preview: Value =
        serde_json::from_str(&app.run(&[&update[..], &["--dry-run"]].concat()).await).unwrap();
    assert_eq!(preview["conflicts"].as_array().unwrap().len(), 2);

    let applied: Value = serde_json::from_str(
        &app.run(&[&update[..], &["--resolve", "/templates/fixture=incoming"]].concat())
            .await,
    )
    .unwrap();
    assert_eq!(applied["document"]["widgets"][0]["template"], "fixture");
    assert_eq!(
        applied["document"]["templates"][0]["name"],
        "Incoming template"
    );
    assert_eq!(
        applied["revision"].as_u64().unwrap(),
        preview["revision"].as_u64().unwrap() + 1
    );
    app.finish().await;
}

#[cfg(unix)]
#[tokio::test]
async fn terminal_editor_updates_a_definition_and_keeps_state_on_cancel() {
    use std::os::unix::fs::PermissionsExt;

    let app = Workbench::start().await;
    let workspace = app.workspace(app.directory.path()).await;
    let id = workspace.as_str();
    let editor = app.file("editor");
    let set_editor = |script: &str| {
        std::fs::write(&editor, script).unwrap();
        std::fs::set_permissions(&editor, std::fs::Permissions::from_mode(0o700)).unwrap();
    };
    set_editor("#!/bin/sh\nprintf 'version = 1\\nname = \"Custom workspace\"\\n' > \"$1\"\n");
    let saved = output(
        app.command(&["workspace", "configure", "--edit", "--workspace", id])
            .env("VISUAL", &editor)
            .output()
            .await
            .unwrap(),
    );
    assert!(saved.contains("Custom workspace saved"));

    app.run(&["widget", "create", "edit-me", "--workspace", id])
        .await;
    set_editor(
        "#!/bin/sh\nprintf 'version = 1\\n[template]\\nid = \"edited\"\\nname = \"Edited widget\"\\n[template.definition]\\n' > \"$1\"\n",
    );
    let saved = output(
        app.command(&["widget", "edit", "edit-me", "--workspace", id, "--json"])
            .env_remove("VISUAL")
            .env("EDITOR", &editor)
            .output()
            .await
            .unwrap(),
    );
    assert!(saved.contains("Edited widget"));
    set_editor("#!/bin/sh\nexit 1\n");
    let cancelled = app
        .command(&["widget", "edit", "edit-me", "--workspace", id])
        .env("VISUAL", &editor)
        .output()
        .await
        .unwrap();
    assert!(!cancelled.status.success());
    assert!(String::from_utf8_lossy(&cancelled.stderr).contains("editor exited"));
    assert!(
        app.run(&["widget", "show", "edit-me", "--workspace", id])
            .await
            .contains("Edited widget")
    );
    app.finish().await;
}
