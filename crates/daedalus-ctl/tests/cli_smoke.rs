#[cfg(unix)]
use daedalus_proto::transport::IpcStream;
#[cfg(unix)]
use daedalus_proto::{
    AgentState, ClientMessage, FailureTrace, Request, ResponsePayload, ServerMessage, WorkspaceId,
    WorkspaceInfo, read_frame, write_frame,
};
use daedalusd::server::DaemonServer;
use daedalusd::supervisor::{IsolationMode, Supervisor};
use tempfile::TempDir;
use tokio::process::Command;
use tokio::sync::oneshot;

fn test_supervisor() -> Supervisor {
    #[cfg(windows)]
    {
        Supervisor::with_mode(IsolationMode::WindowsProcessTree)
    }
    #[cfg(not(windows))]
    {
        Supervisor::with_mode(IsolationMode::PgidFallback)
    }
}

#[tokio::test]
async fn cli_widget_crud_and_workspace_round_trip() {
    let dir = TempDir::new().unwrap();
    let checkout = dir.path().join("checkout");
    let other = dir.path().join("other");
    std::fs::create_dir_all(&checkout).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    let sock = dir.path().join("test.sock");
    let server = DaemonServer::bind_with_supervisor(
        sock.clone(),
        dir.path().join("runtime"),
        test_supervisor(),
    )
    .await
    .unwrap();
    let (shutdown, done) = oneshot::channel();
    let server = tokio::spawn(server.serve(async {
        let _ = done.await;
    }));
    async fn cli(socket: &std::path::Path, args: &[&str]) -> String {
        let output = Command::new(env!("CARGO_BIN_EXE_daedalus-ctl"))
            .arg("--socket")
            .arg(socket)
            .args(args)
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
    let base = cli(
        &sock,
        &[
            "workspace",
            "create",
            "API",
            "--dir",
            checkout.to_str().unwrap(),
        ],
    )
    .await;
    let target = cli(
        &sock,
        &[
            "workspace",
            "create",
            "Backup",
            "--dir",
            other.to_str().unwrap(),
        ],
    )
    .await;
    let base = base.trim();
    let target = target.trim();
    let listed: serde_json::Value =
        serde_json::from_str(&cli(&sock, &["workspace", "list", "--json"]).await).unwrap();
    let listed = listed.as_array().unwrap();
    assert_eq!(listed.len(), 2);
    assert!(listed.iter().any(|ws| {
        ws["id"] == base
            && ws["name"] == "API"
            && ws["pane_ids"] == serde_json::json!([])
            && ws.get("injected_env").is_none()
    }));
    assert!(listed.iter().any(|ws| ws["id"] == target));

    let template = dir.path().join("git-widget.toml");
    cli(
        &sock,
        &[
            "widget",
            "template",
            "export",
            "git-worktree",
            "--file",
            template.to_str().unwrap(),
        ],
    )
    .await;
    cli(
        &sock,
        &[
            "widget",
            "template",
            "validate",
            "--file",
            template.to_str().unwrap(),
        ],
    )
    .await;
    cli(
        &sock,
        &[
            "widget",
            "create",
            "branch",
            "--template",
            "git-worktree",
            "--workspace",
            base,
        ],
    )
    .await;
    cli(
        &sock,
        &[
            "widget",
            "duplicate",
            "branch",
            "review",
            "--workspace",
            base,
        ],
    )
    .await;
    cli(
        &sock,
        &["widget", "move", "review", "1", "--workspace", base],
    )
    .await;
    cli(
        &sock,
        &["widget", "pin", "review", "--unpin", "--workspace", base],
    )
    .await;
    let portable = checkout.join(".daedalus-workspace.toml");
    cli(
        &sock,
        &[
            "workspace",
            "export",
            "--workspace",
            base,
            "--file",
            portable.to_str().unwrap(),
        ],
    )
    .await;
    cli(
        &sock,
        &["workspace", "import", "--repo", "--workspace", base],
    )
    .await;
    cli(
        &sock,
        &[
            "workspace",
            "import",
            "--file",
            portable.to_str().unwrap(),
            "--workspace",
            target,
            "--dry-run",
            "--json",
        ],
    )
    .await;
    assert_eq!(
        cli(&sock, &["widget", "list", "--workspace", target, "--json"])
            .await
            .trim(),
        "[]"
    );
    cli(
        &sock,
        &[
            "workspace",
            "import",
            "--file",
            portable.to_str().unwrap(),
            "--workspace",
            target,
        ],
    )
    .await;
    let original = cli(&sock, &["workspace", "export", "--workspace", base]).await;
    assert_eq!(
        cli(&sock, &["workspace", "export", "--workspace", target]).await,
        original
    );
    cli(
        &sock,
        &[
            "widget",
            "edit",
            "branch",
            "--name",
            "Local branch",
            "--workspace",
            base,
        ],
    )
    .await;
    std::fs::write(
        &portable,
        original.replace("name = \"API\"", "name = \"Shared API\""),
    )
    .unwrap();
    let applied = cli(
        &sock,
        &["workspace", "update", "--workspace", base, "--json"],
    )
    .await;
    assert!(applied.contains("Shared API"));
    assert!(applied.contains("Local branch"));
    let fresh = dir.path().join("fresh");
    std::fs::create_dir(&fresh).unwrap();
    let imported = cli(
        &sock,
        &[
            "workspace",
            "import",
            "--new",
            fresh.to_str().unwrap(),
            "--file",
            portable.to_str().unwrap(),
        ],
    )
    .await;
    let imported_widgets = cli(&sock, &["widget", "list", "--workspace", imported.trim()]).await;
    assert!(imported_widgets.contains("Git Worktree"));

    #[cfg(unix)]
    {
        let command = dir.path().join("command-widget.toml");
        std::fs::write(
            &command,
            r#"
version = 1
[template]
id = "cwd"
name = "Checkout location"
[[template.definition.sources]]
id = "main"
kind = "command"
[template.definition.sources.command]
argv = ["sh", "-c", "pwd"]
[template.definition.sources.refresh]
interval_seconds = 1
[template.definition.sources.output]
kind = "text"
[[template.definition.display.summary]]
source = "main"
field = "text"
"#,
        )
        .unwrap();
        let path = command.to_str().unwrap();
        let preview = cli(
            &sock,
            &[
                "widget",
                "preview",
                "--file",
                path,
                "--workspace",
                imported.trim(),
            ],
        )
        .await;
        assert!(preview.contains(fresh.to_str().unwrap()));
        cli(
            &sock,
            &[
                "widget",
                "create",
                "cwd",
                "--file",
                path,
                "--workspace",
                imported.trim(),
            ],
        )
        .await;
        tokio::time::timeout(std::time::Duration::from_secs(8), async {
            loop {
                let results = cli(
                    &sock,
                    &["widget", "results", "--workspace", imported.trim()],
                )
                .await;
                if results.contains(fresh.to_str().unwrap()) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("timer refreshes the command in the imported workspace");
    }
    shutdown.send(()).unwrap();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn cli_smoke_ping_workspace_and_pane_lifecycle() {
    let dir = TempDir::new().unwrap();
    let sock = dir.path().join("daedalusd.sock");
    let runtime = dir.path().join("runtime");

    let server = DaemonServer::bind_with_supervisor(sock.clone(), runtime, test_supervisor())
        .await
        .unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let srv_handle = tokio::spawn(async move {
        let _ = server
            .serve(async {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    let bin = env!("CARGO_BIN_EXE_daedalus-ctl");

    let ping = Command::new(bin)
        .args(["--socket", sock.to_str().unwrap(), "ping"])
        .output()
        .await
        .unwrap();
    assert!(ping.status.success());
    let ping_out = String::from_utf8_lossy(&ping.stdout);
    assert!(ping_out.contains("daedalusd v"));

    let create = Command::new(bin)
        .args([
            "--socket",
            sock.to_str().unwrap(),
            "workspace",
            "create",
            "smoke",
            "--dir",
            dir.path().to_str().unwrap(),
        ])
        .output()
        .await
        .unwrap();
    assert!(create.status.success());
    let ws_id = String::from_utf8_lossy(&create.stdout).trim().to_string();

    #[cfg(unix)]
    let shell_bin = "/bin/bash";
    #[cfg(windows)]
    let shell_bin = "cmd.exe";

    let spawn = Command::new(bin)
        .args([
            "--socket",
            sock.to_str().unwrap(),
            "pane",
            "spawn",
            &ws_id,
            "--role",
            "agent",
            "--shell",
            shell_bin,
        ])
        .output()
        .await
        .unwrap();
    assert!(spawn.status.success());
    let pane_id = String::from_utf8_lossy(&spawn.stdout).trim().to_string();

    #[cfg(unix)]
    {
        let send = Command::new(bin)
            .args([
                "--socket",
                sock.to_str().unwrap(),
                "pane",
                "send",
                &pane_id,
                "echo SMOKE_OK",
                "--newline",
            ])
            .output()
            .await
            .unwrap();
        assert!(send.status.success());

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }

    let snap = Command::new(bin)
        .args([
            "--socket",
            sock.to_str().unwrap(),
            "pane",
            "snapshot",
            &pane_id,
        ])
        .output()
        .await
        .unwrap();
    assert!(snap.status.success());
    #[cfg(unix)]
    assert!(String::from_utf8_lossy(&snap.stdout).contains("SMOKE_OK"));

    let status_pub = Command::new(bin)
        .args([
            "--socket",
            sock.to_str().unwrap(),
            "status",
            "publish",
            "--source",
            "phrourion",
            "--dir",
            dir.path().to_str().unwrap(),
            "--level",
            "warn",
            "--label",
            "2 dirty",
        ])
        .output()
        .await
        .unwrap();
    assert!(
        status_pub.status.success(),
        "status publish failed: {}",
        String::from_utf8_lossy(&status_pub.stderr)
    );

    let ws_list = Command::new(bin)
        .args(["--socket", sock.to_str().unwrap(), "workspace", "list"])
        .output()
        .await
        .unwrap();
    assert!(ws_list.status.success());
    let ws_list_out = String::from_utf8_lossy(&ws_list.stdout);
    assert!(ws_list_out.contains(&ws_id));
    assert!(ws_list_out.contains("smoke"));
    assert!(ws_list_out.contains("phrourion:2 dirty"));

    let wt_list = Command::new(bin)
        .args([
            "--socket",
            sock.to_str().unwrap(),
            "worktree",
            "list",
            "--dir",
            dir.path().to_str().unwrap(),
        ])
        .output()
        .await
        .unwrap();
    assert!(wt_list.status.success());

    let run_pane = |args: Vec<&str>| {
        let mut cmd = Command::new(bin);
        cmd.arg("--socket").arg(&sock).arg("pane").args(args);
        async move { cmd.output().await.unwrap() }
    };
    let resized = run_pane(vec!["resize", &pane_id, "50", "7"]).await;
    assert!(resized.status.success());
    let snapshot = run_pane(vec!["snapshot", &pane_id]).await;
    assert!(snapshot.status.success());
    assert_eq!(String::from_utf8_lossy(&snapshot.stdout).lines().count(), 7);
    let trace = run_pane(vec!["trace", &pane_id]).await;
    assert!(trace.status.success());
    assert_eq!(
        String::from_utf8_lossy(&trace.stdout).trim(),
        "no failure trace"
    );
    let closed = run_pane(vec!["close", &pane_id]).await;
    assert!(closed.status.success());
    assert_eq!(
        String::from_utf8_lossy(&closed.stdout).trim(),
        format!("closed {pane_id}")
    );
    assert!(!run_pane(vec!["snapshot", &pane_id]).await.status.success());

    let close = Command::new(bin)
        .args([
            "--socket",
            sock.to_str().unwrap(),
            "workspace",
            "close",
            &ws_id,
        ])
        .output()
        .await
        .unwrap();
    assert!(close.status.success());

    let _ = shutdown_tx.send(());
    srv_handle.await.unwrap();
}

async fn run_git(repo: &std::path::Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[tokio::test]
async fn cli_smoke_worktree_create_list_and_compose_down() {
    let dir = TempDir::new().unwrap();
    let sock = dir.path().join("daedalusd.sock");
    let runtime = dir.path().join("runtime");
    let repo = dir.path().join("repo");
    let worktrees_dir = dir.path().join("worktrees");
    tokio::fs::create_dir_all(&repo).await.unwrap();

    run_git(&repo, &["init"]).await;
    run_git(&repo, &["config", "user.name", "Smoke Test"]).await;
    run_git(&repo, &["config", "user.email", "smoke@example.com"]).await;
    tokio::fs::write(repo.join("README.md"), b"smoke\n")
        .await
        .unwrap();
    run_git(&repo, &["add", "README.md"]).await;
    run_git(&repo, &["commit", "-m", "initial"]).await;

    let server = DaemonServer::bind_with_supervisor(sock.clone(), runtime, test_supervisor())
        .await
        .unwrap();
    server.set_worktrees_dir(worktrees_dir);

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let srv_handle = tokio::spawn(async move {
        let _ = server
            .serve(async {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    let bin = env!("CARGO_BIN_EXE_daedalus-ctl");

    let wt_list_initial = Command::new(bin)
        .args([
            "--socket",
            sock.to_str().unwrap(),
            "worktree",
            "list",
            "--dir",
            repo.to_str().unwrap(),
        ])
        .output()
        .await
        .unwrap();
    assert!(wt_list_initial.status.success());
    let initial_out = String::from_utf8_lossy(&wt_list_initial.stdout);
    assert!(initial_out.contains("[primary]"));

    let wt_create = Command::new(bin)
        .args([
            "--socket",
            sock.to_str().unwrap(),
            "worktree",
            "create",
            "feat/smoke",
            "--dir",
            repo.to_str().unwrap(),
            "--env",
            "CUSTOM_VAR=abc",
        ])
        .output()
        .await
        .unwrap();
    assert!(
        wt_create.status.success(),
        "worktree create failed: {}",
        String::from_utf8_lossy(&wt_create.stderr)
    );
    let wt_ws_id = String::from_utf8_lossy(&wt_create.stdout)
        .trim()
        .to_string();
    assert!(!wt_ws_id.is_empty());

    let wt_list_after = Command::new(bin)
        .args([
            "--socket",
            sock.to_str().unwrap(),
            "worktree",
            "list",
            "--dir",
            repo.to_str().unwrap(),
        ])
        .output()
        .await
        .unwrap();
    assert!(wt_list_after.status.success());
    assert!(String::from_utf8_lossy(&wt_list_after.stdout).contains("feat/smoke"));

    let status_pub = Command::new(bin)
        .args([
            "--socket",
            sock.to_str().unwrap(),
            "status",
            "publish",
            "--workspace",
            &wt_ws_id,
            "--source",
            "cassandra",
            "--level",
            "error",
            "--label",
            "retry 1/3",
            "--detail",
            "rate limit",
        ])
        .output()
        .await
        .unwrap();
    assert!(status_pub.status.success());

    let ws_list = Command::new(bin)
        .args(["--socket", sock.to_str().unwrap(), "workspace", "list"])
        .output()
        .await
        .unwrap();
    assert!(ws_list.status.success());
    let ws_list_out = String::from_utf8_lossy(&ws_list.stdout);
    assert!(ws_list_out.contains(&wt_ws_id));
    assert!(ws_list_out.contains("branch=feat/smoke"));
    assert!(ws_list_out.contains("cassandra:retry 1/3"));

    let close = Command::new(bin)
        .args([
            "--socket",
            sock.to_str().unwrap(),
            "workspace",
            "close",
            &wt_ws_id,
            "--compose-down",
        ])
        .output()
        .await
        .unwrap();
    assert!(close.status.success());

    let _ = shutdown_tx.send(());
    srv_handle.await.unwrap();
}

#[cfg(unix)]
async fn list_workspaces_rpc(sock: &std::path::Path) -> Vec<WorkspaceInfo> {
    let mut stream = IpcStream::connect(sock).await.unwrap();
    let req = Request {
        id: 1,
        payload: ClientMessage::ListWorkspaces,
    };
    write_frame(&mut stream, &req).await.unwrap();
    loop {
        let msg: ServerMessage = read_frame(&mut stream).await.unwrap().unwrap();
        if let ServerMessage::Response { id: 1, result } = msg {
            match result.unwrap() {
                ResponsePayload::Workspaces(list) => return list,
                other => panic!("unexpected response: {other:?}"),
            }
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn cli_smoke_v0_0_3_agent_state_failure_trace_status_presets_and_worktree_cleanup() {
    let dir = TempDir::new().unwrap();
    let sock = dir.path().join("daedalusd.sock");
    let runtime = dir.path().join("runtime");
    let repo = dir.path().join("repo");
    let worktrees_dir = dir.path().join("worktrees");
    tokio::fs::create_dir_all(&repo).await.unwrap();

    run_git(&repo, &["init", "-b", "main"]).await;
    run_git(&repo, &["config", "user.name", "Smoke Test"]).await;
    run_git(&repo, &["config", "user.email", "smoke@example.com"]).await;
    tokio::fs::write(repo.join("README.md"), b"smoke\n")
        .await
        .unwrap();
    run_git(&repo, &["add", "README.md"]).await;
    run_git(&repo, &["commit", "-m", "initial"]).await;

    let server = DaemonServer::bind_with_supervisor(sock.clone(), runtime, test_supervisor())
        .await
        .unwrap();
    server.set_worktrees_dir(worktrees_dir.clone());

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let srv_handle = tokio::spawn(async move {
        let _ = server
            .serve(async {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    let bin = env!("CARGO_BIN_EXE_daedalus-ctl");
    let sock_str = sock.to_str().unwrap();

    // Create clean secondary worktree workspace
    let wt_create = Command::new(bin)
        .args([
            "--socket",
            sock_str,
            "worktree",
            "create",
            "feat/cleanup",
            "--dir",
            repo.to_str().unwrap(),
        ])
        .output()
        .await
        .unwrap();
    assert!(
        wt_create.status.success(),
        "worktree create failed: {}",
        String::from_utf8_lossy(&wt_create.stderr)
    );
    let wt_ws_id = String::from_utf8_lossy(&wt_create.stdout)
        .trim()
        .to_string();
    let wt_ws_uuid: uuid::Uuid = wt_ws_id.parse().unwrap();

    let workspaces = list_workspaces_rpc(&sock).await;
    let wt_ws = workspaces
        .iter()
        .find(|ws| ws.id == WorkspaceId(wt_ws_uuid))
        .expect("created worktree workspace should exist");
    let wt_path = wt_ws.root_dir.clone();
    assert!(wt_path.exists());

    // Spawn an agent pane and exercise `agent-state set` via --pane, --workspace, and default cwd
    let spawn_agent = Command::new(bin)
        .args([
            "--socket",
            sock_str,
            "pane",
            "spawn",
            &wt_ws_id,
            "--role",
            "agent",
            "--shell",
            "/bin/bash",
        ])
        .output()
        .await
        .unwrap();
    assert!(spawn_agent.status.success());
    let agent_pane_id = String::from_utf8_lossy(&spawn_agent.stdout)
        .trim()
        .to_string();

    let set_waiting = Command::new(bin)
        .args([
            "--socket",
            sock_str,
            "agent-state",
            "set",
            "waiting",
            "--pane",
            &agent_pane_id,
        ])
        .output()
        .await
        .unwrap();
    assert!(
        set_waiting.status.success(),
        "agent-state set waiting failed: {}",
        String::from_utf8_lossy(&set_waiting.stderr)
    );
    let workspaces = list_workspaces_rpc(&sock).await;
    let agent_summary = workspaces
        .iter()
        .find(|ws| ws.id == WorkspaceId(wt_ws_uuid))
        .unwrap()
        .panes
        .iter()
        .find(|p| p.id.to_string() == agent_pane_id)
        .unwrap();
    assert_eq!(
        agent_summary.agent_state,
        Some(AgentState::WaitingForApproval)
    );

    let set_thinking = Command::new(bin)
        .args([
            "--socket",
            sock_str,
            "agent-state",
            "set",
            "thinking",
            "--workspace",
            &wt_ws_id,
        ])
        .output()
        .await
        .unwrap();
    assert!(set_thinking.status.success());
    let workspaces = list_workspaces_rpc(&sock).await;
    let agent_summary = workspaces
        .iter()
        .find(|ws| ws.id == WorkspaceId(wt_ws_uuid))
        .unwrap()
        .panes
        .iter()
        .find(|p| p.id.to_string() == agent_pane_id)
        .unwrap();
    assert_eq!(agent_summary.agent_state, Some(AgentState::Thinking));

    // Default cwd fallback when neither --pane, --workspace, nor --dir is passed
    let set_idle_cwd = Command::new(bin)
        .current_dir(&wt_path)
        .args(["--socket", sock_str, "agent-state", "set", "idle"])
        .output()
        .await
        .unwrap();
    assert!(
        set_idle_cwd.status.success(),
        "agent-state set idle from cwd failed: {}",
        String::from_utf8_lossy(&set_idle_cwd.stderr)
    );
    let workspaces = list_workspaces_rpc(&sock).await;
    let agent_summary = workspaces
        .iter()
        .find(|ws| ws.id == WorkspaceId(wt_ws_uuid))
        .unwrap()
        .panes
        .iter()
        .find(|p| p.id.to_string() == agent_pane_id)
        .unwrap();
    assert_eq!(agent_summary.agent_state, Some(AgentState::Idle));

    // Exercise `status publish --preset phrourion` and `status clear`
    let pub_phr = Command::new(bin)
        .args([
            "--socket",
            sock_str,
            "status",
            "publish",
            "--preset",
            "phrourion",
            "--dir",
            wt_path.to_str().unwrap(),
            "--level",
            "warn",
            "--label",
            "1 dirty",
        ])
        .output()
        .await
        .unwrap();
    assert!(
        pub_phr.status.success(),
        "status publish --preset phrourion failed: {}",
        String::from_utf8_lossy(&pub_phr.stderr)
    );

    let ws_list = Command::new(bin)
        .args(["--socket", sock_str, "workspace", "list"])
        .output()
        .await
        .unwrap();
    assert!(ws_list.status.success());
    assert!(String::from_utf8_lossy(&ws_list.stdout).contains("phrourion:1 dirty"));

    let clear_phr = Command::new(bin)
        .args([
            "--socket",
            sock_str,
            "status",
            "clear",
            "phrourion",
            "--dir",
            wt_path.to_str().unwrap(),
        ])
        .output()
        .await
        .unwrap();
    assert!(
        clear_phr.status.success(),
        "status clear phrourion failed: {}",
        String::from_utf8_lossy(&clear_phr.stderr)
    );

    let ws_list_after_clear = Command::new(bin)
        .args(["--socket", sock_str, "workspace", "list"])
        .output()
        .await
        .unwrap();
    assert!(ws_list_after_clear.status.success());
    assert!(!String::from_utf8_lossy(&ws_list_after_clear.stdout).contains("phrourion:"));

    // Also test `status clear --preset cassandra --workspace <id>`
    let pub_cas = Command::new(bin)
        .args([
            "--socket",
            sock_str,
            "status",
            "publish",
            "--preset",
            "cassandra",
            "--workspace",
            &wt_ws_id,
            "--level",
            "info",
            "--label",
            "running",
        ])
        .output()
        .await
        .unwrap();
    assert!(pub_cas.status.success());

    let clear_cas = Command::new(bin)
        .args([
            "--socket",
            sock_str,
            "status",
            "clear",
            "--preset",
            "cassandra",
            "--workspace",
            &wt_ws_id,
        ])
        .output()
        .await
        .unwrap();
    assert!(clear_cas.status.success());
    let workspaces = list_workspaces_rpc(&sock).await;
    let wt_ws = workspaces
        .iter()
        .find(|ws| ws.id == WorkspaceId(wt_ws_uuid))
        .unwrap();
    assert!(wt_ws.external_statuses.is_empty());

    // Exercise `failure-trace` before and after a failing command in a runner pane
    let spawn_runner = Command::new(bin)
        .args([
            "--socket",
            sock_str,
            "pane",
            "spawn",
            &wt_ws_id,
            "--role",
            "runner",
            "--shell",
            "/bin/bash",
        ])
        .output()
        .await
        .unwrap();
    assert!(spawn_runner.status.success());
    let runner_pane_id = String::from_utf8_lossy(&spawn_runner.stdout)
        .trim()
        .to_string();

    let empty_trace_json = Command::new(bin)
        .args([
            "--socket",
            sock_str,
            "failure-trace",
            &runner_pane_id,
            "--json",
        ])
        .output()
        .await
        .unwrap();
    assert!(empty_trace_json.status.success());
    assert_eq!(
        String::from_utf8_lossy(&empty_trace_json.stdout).trim(),
        "null"
    );

    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    let send_fail = Command::new(bin)
        .args([
            "--socket",
            sock_str,
            "pane",
            "send",
            &runner_pane_id,
            "sh -c 'echo FAIL_SMOKE; exit 17'",
            "--newline",
        ])
        .output()
        .await
        .unwrap();
    assert!(send_fail.status.success());

    let mut captured_trace: Option<FailureTrace> = None;
    for _ in 0..30 {
        let out = Command::new(bin)
            .args([
                "--socket",
                sock_str,
                "failure-trace",
                &runner_pane_id,
                "--json",
            ])
            .output()
            .await
            .unwrap();
        assert!(out.status.success());
        let stdout = String::from_utf8_lossy(&out.stdout);
        if stdout.trim() != "null" {
            captured_trace = Some(serde_json::from_str(stdout.trim()).unwrap());
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let trace = captured_trace.expect("expected failure trace after exiting 17");
    assert_eq!(trace.exit_code, 17);
    assert!(trace.output.contains("FAIL_SMOKE"));
    assert_eq!(trace.git_branch.as_deref(), Some("feat/cleanup"));

    let plain_trace = Command::new(bin)
        .args(["--socket", sock_str, "failure-trace", &runner_pane_id])
        .output()
        .await
        .unwrap();
    assert!(plain_trace.status.success());
    let plain_out = String::from_utf8_lossy(&plain_trace.stdout);
    assert!(plain_out.contains("exit_code: 17"));
    assert!(plain_out.contains("git_branch: feat/cleanup"));
    assert!(plain_out.contains("FAIL_SMOKE"));

    // Exercise `workspace close --remove-worktree` on clean worktree
    let close_clean = Command::new(bin)
        .args([
            "--socket",
            sock_str,
            "workspace",
            "close",
            &wt_ws_id,
            "--remove-worktree",
        ])
        .output()
        .await
        .unwrap();
    assert!(
        close_clean.status.success(),
        "workspace close --remove-worktree failed: {}",
        String::from_utf8_lossy(&close_clean.stderr)
    );
    assert!(!wt_path.exists(), "worktree directory should be removed");
    assert!(
        !list_workspaces_rpc(&sock)
            .await
            .iter()
            .any(|ws| ws.id == WorkspaceId(wt_ws_uuid))
    );

    // Exercise dirty worktree rejection without --force and cleanup with --force
    let dirty_create = Command::new(bin)
        .args([
            "--socket",
            sock_str,
            "worktree",
            "create",
            "feat/dirty",
            "--dir",
            repo.to_str().unwrap(),
        ])
        .output()
        .await
        .unwrap();
    assert!(dirty_create.status.success());
    let dirty_ws_id = String::from_utf8_lossy(&dirty_create.stdout)
        .trim()
        .to_string();
    let dirty_ws_uuid: uuid::Uuid = dirty_ws_id.parse().unwrap();
    let dirty_path = list_workspaces_rpc(&sock)
        .await
        .into_iter()
        .find(|ws| ws.id == WorkspaceId(dirty_ws_uuid))
        .unwrap()
        .root_dir;
    tokio::fs::write(dirty_path.join("untracked.txt"), b"dirty\n")
        .await
        .unwrap();

    let close_dirty_no_force = Command::new(bin)
        .args([
            "--socket",
            sock_str,
            "workspace",
            "close",
            &dirty_ws_id,
            "--remove-worktree",
        ])
        .output()
        .await
        .unwrap();
    assert!(
        !close_dirty_no_force.status.success(),
        "dirty worktree close without --force should fail"
    );
    assert!(dirty_path.exists());

    let close_dirty_force = Command::new(bin)
        .args([
            "--socket",
            sock_str,
            "workspace",
            "close",
            &dirty_ws_id,
            "--remove-worktree",
            "--force",
        ])
        .output()
        .await
        .unwrap();
    assert!(
        close_dirty_force.status.success(),
        "dirty worktree close with --force failed: {}",
        String::from_utf8_lossy(&close_dirty_force.stderr)
    );
    assert!(!dirty_path.exists());

    let _ = shutdown_tx.send(());
    srv_handle.await.unwrap();
}

#[tokio::test]
async fn cli_smoke_v0_0_6_auth_token_and_custom_actions() {
    let dir = TempDir::new().unwrap();
    let runtime = dir.path().join("runtime");
    let ws_dir = dir.path().join("ws");
    tokio::fs::create_dir_all(&ws_dir).await.unwrap();
    tokio::fs::write(
        ws_dir.join(".daedalus.toml"),
        r#"
[[actions]]
id = "runner-ok"
label = "Run Marker"
target = "runner"
command = "echo CLI_ACTION_OK"
shortcut = "Ctrl+R"
"#,
    )
    .await
    .unwrap();

    let server = DaemonServer::bind_with_auth(
        std::path::PathBuf::from("tcp://127.0.0.1:0"),
        runtime,
        None,
        None,
        test_supervisor(),
        Some("cli-secret-token".into()),
    )
    .await
    .unwrap();
    let endpoint = server.socket_path();
    let endpoint_str = endpoint.to_string_lossy().to_string();

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let srv_handle = tokio::spawn(async move {
        let _ = server
            .serve(async {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    let bin = env!("CARGO_BIN_EXE_daedalus-ctl");

    let unauth_ping = Command::new(bin)
        .env_remove("DAEDALUS_AUTH_TOKEN")
        .args(["--socket", &endpoint_str, "ping"])
        .output()
        .await
        .unwrap();
    assert!(
        !unauth_ping.status.success(),
        "ping without --auth-token should be rejected"
    );

    let bad_auth_ping = Command::new(bin)
        .args([
            "--socket",
            &endpoint_str,
            "--auth-token",
            "wrong-token",
            "ping",
        ])
        .output()
        .await
        .unwrap();
    assert!(
        !bad_auth_ping.status.success(),
        "ping with wrong --auth-token should be rejected"
    );

    let ok_ping = Command::new(bin)
        .args([
            "--socket",
            &endpoint_str,
            "--auth-token",
            "cli-secret-token",
            "ping",
        ])
        .output()
        .await
        .unwrap();
    assert!(
        ok_ping.status.success(),
        "authenticated ping failed: {}",
        String::from_utf8_lossy(&ok_ping.stderr)
    );

    let create_ws = Command::new(bin)
        .args([
            "--socket",
            &endpoint_str,
            "--auth-token",
            "cli-secret-token",
            "workspace",
            "create",
            "action-smoke",
            "--dir",
            ws_dir.to_str().unwrap(),
        ])
        .output()
        .await
        .unwrap();
    assert!(
        create_ws.status.success(),
        "workspace create failed: {}",
        String::from_utf8_lossy(&create_ws.stderr)
    );
    let ws_id = String::from_utf8_lossy(&create_ws.stdout)
        .trim()
        .to_string();

    #[cfg(unix)]
    let shell_bin = "/bin/sh";
    #[cfg(windows)]
    let shell_bin = "cmd.exe";

    let spawn = Command::new(bin)
        .args([
            "--socket",
            &endpoint_str,
            "--auth-token",
            "cli-secret-token",
            "pane",
            "spawn",
            &ws_id,
            "--role",
            "runner",
            "--shell",
            shell_bin,
        ])
        .output()
        .await
        .unwrap();
    assert!(
        spawn.status.success(),
        "pane spawn failed: {}",
        String::from_utf8_lossy(&spawn.stderr)
    );
    let pane_id = String::from_utf8_lossy(&spawn.stdout).trim().to_string();

    let list_json = Command::new(bin)
        .args([
            "--socket",
            &endpoint_str,
            "--auth-token",
            "cli-secret-token",
            "action",
            "list",
            "--dir",
            ws_dir.to_str().unwrap(),
            "--json",
        ])
        .output()
        .await
        .unwrap();
    assert!(
        list_json.status.success(),
        "action list --json failed: {}",
        String::from_utf8_lossy(&list_json.stderr)
    );
    let parsed_actions: Vec<daedalus_proto::CustomAction> =
        serde_json::from_slice(&list_json.stdout).expect("valid JSON CustomAction array");
    assert_eq!(
        parsed_actions,
        vec![daedalus_proto::CustomAction {
            id: "runner-ok".to_string(),
            label: "Run Marker".to_string(),
            target: daedalus_proto::PaneRole::Runner,
            command: "echo CLI_ACTION_OK".to_string(),
            shortcut: Some("Ctrl+R".to_string()),
        }]
    );

    let list_plain = Command::new(bin)
        .args([
            "--socket",
            &endpoint_str,
            "--auth-token",
            "cli-secret-token",
            "action",
            "list",
            "--workspace",
            &ws_id,
        ])
        .output()
        .await
        .unwrap();
    assert!(list_plain.status.success());
    let list_plain_out = String::from_utf8_lossy(&list_plain.stdout);
    assert!(list_plain_out.contains("runner-ok"));
    assert!(list_plain_out.contains("Run Marker"));

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let run_action = Command::new(bin)
        .args([
            "--socket",
            &endpoint_str,
            "--auth-token",
            "cli-secret-token",
            "action",
            "run",
            "runner-ok",
            "--workspace",
            &ws_id,
        ])
        .output()
        .await
        .unwrap();
    assert!(
        run_action.status.success(),
        "action run failed: {}",
        String::from_utf8_lossy(&run_action.stderr)
    );

    let mut found_marker = false;
    for _ in 0..40 {
        let snap = Command::new(bin)
            .args([
                "--socket",
                &endpoint_str,
                "--auth-token",
                "cli-secret-token",
                "pane",
                "snapshot",
                &pane_id,
            ])
            .output()
            .await
            .unwrap();
        assert!(snap.status.success());
        if String::from_utf8_lossy(&snap.stdout).contains("CLI_ACTION_OK") {
            found_marker = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        found_marker,
        "timed out waiting for CLI_ACTION_OK in runner pane snapshot"
    );

    let close = Command::new(bin)
        .args([
            "--socket",
            &endpoint_str,
            "--auth-token",
            "cli-secret-token",
            "workspace",
            "close",
            &ws_id,
        ])
        .output()
        .await
        .unwrap();
    assert!(close.status.success());

    let _ = shutdown_tx.send(());
    srv_handle.await.unwrap();
}

#[tokio::test]
async fn service_plist_can_be_generated_offline_with_escaped_user_paths() {
    let out = Command::new(env!("CARGO_BIN_EXE_daedalus-ctl"))
        .args([
            "--socket",
            "/offline/demo&test.sock",
            "service",
            "print-launchd",
            "--label",
            "  demo<&>  ",
            "--binary",
            "/path/with spaces/daemon",
        ])
        .env("HOME", "/Users/test&demo")
        .output()
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let plist = String::from_utf8(out.stdout).unwrap();
    for value in [
        "demo&lt;&amp;&gt;",
        "/offline/demo&amp;test.sock",
        "/path/with spaces/daemon",
        "/Users/test&amp;demo/Library/Logs/daedalusd.out.log",
    ] {
        assert!(
            plist.contains(&format!("<string>{value}</string>")),
            "{plist}"
        );
    }
}

#[tokio::test]
async fn workspace_recent_lists_closed_project() {
    let dir = TempDir::new().unwrap();
    let sock = dir.path().join("daedalusd.sock");
    let runtime = dir.path().join("runtime");
    let server = DaemonServer::bind_with_supervisor(sock.clone(), runtime, test_supervisor())
        .await
        .unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let srv_handle = tokio::spawn(async move {
        let _ = server
            .serve(async {
                let _ = shutdown_rx.await;
            })
            .await;
    });
    let dir_path = dir.path().to_path_buf();
    let body = tokio::spawn(async move {
        let bin = env!("CARGO_BIN_EXE_daedalus-ctl");
        let run = |args: Vec<String>| {
            let sock = sock.clone();
            async move {
                Command::new(bin)
                    .kill_on_drop(true)
                    .arg("--socket")
                    .arg(sock)
                    .args(args)
                    .output()
                    .await
                    .unwrap()
            }
        };

        let empty = run(vec!["workspace".into(), "recent".into()]).await;
        assert!(empty.status.success());
        assert_eq!(
            String::from_utf8_lossy(&empty.stdout),
            "No recent projects.\n"
        );

        let project_dir = dir_path.join("proj");
        std::fs::create_dir(&project_dir).unwrap();
        let create = run(vec![
            "workspace".into(),
            "create".into(),
            "recent-demo".into(),
            "--dir".into(),
            project_dir.to_str().unwrap().into(),
        ])
        .await;
        assert!(create.status.success());
        let id = String::from_utf8_lossy(&create.stdout).trim().to_string();
        let close = run(vec!["workspace".into(), "close".into(), id]).await;
        assert!(close.status.success());

        let recent = run(vec!["workspace".into(), "recent".into()]).await;
        assert!(recent.status.success());
        let out = String::from_utf8_lossy(&recent.stdout);
        let fields: Vec<&str> = out.trim_end().split('\t').collect();
        assert_eq!(fields.len(), 3, "{out}");
        assert_eq!(fields[0], "recent-demo");
        assert!(fields[1].ends_with("proj"), "{out}");
        assert_eq!(fields[2].len(), "YYYY-MM-DD HH:MM".len(), "{out}");
    });
    let result = tokio::time::timeout(std::time::Duration::from_secs(30), body).await;
    let _ = shutdown_tx.send(());
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), srv_handle).await;
    result
        .expect("workspace recent e2e timed out")
        .expect("workspace recent e2e body panicked");
}

#[tokio::test]
async fn workspace_recent_reports_unsupported_daemon() {
    // The file-level protocol imports are Unix-only; this mock speaks TCP on every platform.
    use daedalus_proto::transport::IpcListener;
    use daedalus_proto::{
        ClientMessage, Request, ResponsePayload, ServerMessage, read_frame, write_frame,
    };
    let mut listener = IpcListener::bind(std::path::Path::new("tcp://127.0.0.1:0"))
        .await
        .unwrap();
    let endpoint = listener.local_endpoint();
    let mock = tokio::spawn(async move {
        let mut stream = listener.accept().await.unwrap();
        let req: Request = read_frame(&mut stream).await.unwrap().unwrap();
        assert_eq!(req.payload, ClientMessage::Ping);
        write_frame(
            &mut stream,
            &ServerMessage::Response {
                id: req.id,
                result: Ok(ResponsePayload::Pong {
                    version: "0.0.1".into(),
                    vt_backend: "test".into(),
                    isolation_mode: "test".into(),
                    journal_supported: false,
                    inspect_setup_supported: false,
                    project_setup_supported: false,
                    image_preview_supported: false,
                    paste_supported: false,
                    workbench_supported: false,
                    workspace_membership_supported: false,
                    runner_assignment_supported: false,
                    guarded_runner_assignment_supported: false,
                    resolved_import_preview_supported: false,
                    agent_launch_metadata_supported: false,
                    default_pane_fallback_supported: false,
                    persistence_health: None,
                    home_dir: None,
                    recent_projects_supported: false,
                }),
            },
        )
        .await
        .unwrap();
    });
    let output = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let output = Command::new(env!("CARGO_BIN_EXE_daedalus-ctl"))
            .kill_on_drop(true)
            .arg("--socket")
            .arg(endpoint)
            .args(["workspace", "recent"])
            .output()
            .await
            .unwrap();
        mock.await.unwrap();
        output
    })
    .await
    .expect("CLI or mock daemon timed out");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("This daemon does not keep recent projects. Upgrade daedalusd.")
    );
}

#[tokio::test]
async fn temporary_persistence_warns_on_stderr() {
    let dir = TempDir::new().unwrap();
    let sock = dir.path().join("test.sock");
    let state = dir.path().join("state.json");
    std::fs::write(&state, b"bad json").unwrap();
    let server = DaemonServer::bind_with_options(
        sock.clone(),
        dir.path().join("runtime"),
        None,
        Some(state.clone()),
        test_supervisor(),
    )
    .await
    .unwrap();
    let (shutdown, done) = oneshot::channel();
    let task = tokio::spawn(server.serve(async {
        let _ = done.await;
    }));
    let create = Command::new(env!("CARGO_BIN_EXE_daedalus-ctl"))
        .arg("--socket")
        .arg(&sock)
        .args(["workspace", "create", "temporary", "--dir"])
        .arg(dir.path())
        .output()
        .await
        .unwrap();
    assert!(create.status.success());
    let id = String::from_utf8(create.stdout).unwrap();
    uuid::Uuid::parse_str(id.trim()).unwrap();
    assert!(String::from_utf8_lossy(&create.stderr).contains("temporary"));
    let close = Command::new(env!("CARGO_BIN_EXE_daedalus-ctl"))
        .arg("--socket")
        .arg(&sock)
        .args(["workspace", "close", id.trim()])
        .output()
        .await
        .unwrap();
    assert!(close.status.success());
    assert_eq!(
        String::from_utf8(close.stdout).unwrap().trim(),
        format!("closed {}", id.trim())
    );
    assert!(String::from_utf8_lossy(&close.stderr).contains("temporary"));
    #[cfg(target_os = "linux")]
    {
        let cli = || {
            let mut cmd = Command::new(env!("CARGO_BIN_EXE_daedalus-ctl"));
            cmd.arg("--socket")
                .arg(&sock)
                .arg("workspace")
                .stdout(std::process::Stdio::piped())
                .stderr(
                    std::fs::OpenOptions::new()
                        .write(true)
                        .open("/dev/full")
                        .unwrap(),
                );
            cmd
        };
        let create = cli()
            .args(["create", "full", "--dir"])
            .arg(dir.path())
            .spawn()
            .unwrap()
            .wait_with_output()
            .await
            .unwrap();
        assert!(
            create.status.success(),
            "failed stderr write changed create status"
        );
        let id = String::from_utf8(create.stdout).unwrap();
        uuid::Uuid::parse_str(id.trim()).unwrap();
        let close = cli()
            .args(["close", id.trim()])
            .spawn()
            .unwrap()
            .wait_with_output()
            .await
            .unwrap();
        assert!(
            close.status.success(),
            "failed stderr write changed close status"
        );
        assert_eq!(
            String::from_utf8(close.stdout).unwrap().trim(),
            format!("closed {}", id.trim())
        );
    }
    assert_eq!(std::fs::read(state).unwrap(), b"bad json");
    shutdown.send(()).unwrap();
    task.await.unwrap().unwrap();
}

const TEMPORARY_PERSISTENCE_WARNING: &str =
    "Workspace changes are temporary. Repair the rejected snapshot and restart the daemon.";
const PORTABLE_WORKSPACE: &str = "version = 1\nname = 'Portable fixture'\n";

#[tokio::test]
async fn new_workspace_import_warns_when_persistence_is_temporary() {
    check_new_workspace_import_persistence(Some(b"{ invalid JSON")).await;
}

#[tokio::test]
async fn new_workspace_import_is_quiet_when_persistence_is_durable() {
    check_new_workspace_import_persistence(None).await;
}

async fn check_new_workspace_import_persistence(rejected_snapshot: Option<&[u8]>) {
    let dir = TempDir::new().unwrap();
    let sock = dir.path().join("test.sock");
    let state = dir.path().join("state.json");
    if let Some(bytes) = rejected_snapshot {
        std::fs::write(&state, bytes).unwrap();
    }
    let project = dir.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let portable = dir.path().join("workspace.toml");
    std::fs::write(&portable, PORTABLE_WORKSPACE).unwrap();

    let server = DaemonServer::bind_with_options(
        sock.clone(),
        dir.path().join("runtime"),
        None,
        Some(state.clone()),
        test_supervisor(),
    )
    .await
    .unwrap();
    let (shutdown, done) = oneshot::channel();
    let task = tokio::spawn(server.serve(async {
        let _ = done.await;
    }));
    let workspace = || {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_daedalus-ctl"));
        cmd.kill_on_drop(true)
            .arg("--socket")
            .arg(&sock)
            .arg("workspace");
        cmd
    };
    let import = || {
        let mut cmd = workspace();
        cmd.args(["import", "--new"])
            .arg(&project)
            .arg("--file")
            .arg(&portable)
            .arg("--json");
        cmd
    };

    let preview = import().arg("--dry-run").output().await.unwrap();
    let before = workspace().args(["list", "--json"]).output().await.unwrap();
    let applied = import().output().await.unwrap();
    let after = workspace().args(["list", "--json"]).output().await.unwrap();
    shutdown.send(()).unwrap();
    task.await.unwrap().unwrap();

    assert!(preview.status.success(), "{preview:?}");
    let preview_json: serde_json::Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(preview_json["incoming"]["name"], "Portable fixture");
    assert!(preview.stderr.is_empty(), "{preview:?}");
    assert!(before.status.success(), "{before:?}");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&before.stdout).unwrap(),
        serde_json::json!([])
    );
    assert_eq!(applied.status.code(), Some(0), "{applied:?}");
    let id: uuid::Uuid = serde_json::from_slice(&applied.stdout).unwrap();
    assert!(after.status.success(), "{after:?}");
    let listed: serde_json::Value = serde_json::from_slice(&after.stdout).unwrap();
    assert_eq!(listed[0]["id"], id.to_string());
    let snapshot = std::fs::read(state).unwrap();
    if let Some(bytes) = rejected_snapshot {
        assert_eq!(snapshot, bytes);
        assert_eq!(
            String::from_utf8_lossy(&applied.stderr).trim_end(),
            TEMPORARY_PERSISTENCE_WARNING,
            "successful new-workspace import should warn when persistence is temporary"
        );
    } else {
        let persisted: serde_json::Value = serde_json::from_slice(&snapshot).unwrap();
        assert_eq!(persisted["workspaces"][0]["id"], id.to_string());
        assert!(applied.stderr.is_empty(), "{applied:?}");
    }
}

mod workspace_import_peer {
    use super::{Command, PORTABLE_WORKSPACE, TempDir};
    use daedalus_proto::transport::IpcListener;
    use daedalus_proto::workbench::{
        WorkbenchState, WorkspaceCreationPreview, WorkspaceDocument, WorkspaceImportPreview,
        WorkspaceImportSource,
    };
    use daedalus_proto::{
        ClientMessage, PersistenceHealth, ProtoError, Request, ResponsePayload, ServerMessage,
        WorkspaceId, WorkspaceInfo, read_frame, write_frame,
    };
    use std::path::Path;
    use std::process::Output;
    use std::time::Duration;

    fn pong(persistence_health: Option<PersistenceHealth>) -> ResponsePayload {
        ResponsePayload::Pong {
            version: "0.0.1".into(),
            vt_backend: "test".into(),
            isolation_mode: "test".into(),
            journal_supported: false,
            inspect_setup_supported: false,
            project_setup_supported: false,
            image_preview_supported: false,
            paste_supported: false,
            workbench_supported: true,
            workspace_membership_supported: false,
            runner_assignment_supported: false,
            guarded_runner_assignment_supported: false,
            resolved_import_preview_supported: false,
            agent_launch_metadata_supported: false,
            default_pane_fallback_supported: false,
            persistence_health,
            home_dir: None,
            recent_projects_supported: false,
        }
    }

    async fn import(
        target: [&str; 2],
        exchanges: Vec<(ClientMessage, Result<ResponsePayload, ProtoError>)>,
    ) -> Output {
        let dir = TempDir::new().unwrap();
        let portable = dir.path().join("workspace.toml");
        std::fs::write(&portable, PORTABLE_WORKSPACE).unwrap();
        let mut listener = IpcListener::bind(Path::new("tcp://127.0.0.1:0"))
            .await
            .unwrap();
        let endpoint = listener.local_endpoint();
        let peer = tokio::spawn(async move {
            let mut stream = listener.accept().await.unwrap();
            for (expected, result) in exchanges {
                let request: Request = read_frame(&mut stream).await.unwrap().unwrap();
                assert_eq!(request.payload, expected);
                write_frame(
                    &mut stream,
                    &ServerMessage::Response {
                        id: request.id,
                        result,
                    },
                )
                .await
                .unwrap();
            }
            assert_eq!(read_frame::<_, Request>(&mut stream).await.unwrap(), None);
        });
        tokio::time::timeout(Duration::from_secs(10), async {
            let output = Command::new(env!("CARGO_BIN_EXE_daedalus-ctl"))
                .kill_on_drop(true)
                .env_remove("DAEDALUS_AUTH_TOKEN")
                .arg("--socket")
                .arg(endpoint)
                .args(["workspace", "import"])
                .args(target)
                .arg("--file")
                .arg(portable)
                .arg("--json")
                .output()
                .await
                .unwrap();
            peer.await.unwrap();
            output
        })
        .await
        .expect("CLI or mock daemon timed out")
    }

    fn creation_preview(root_dir: &Path) -> WorkspaceCreationPreview {
        WorkspaceCreationPreview {
            root_dir: root_dir.into(),
            incoming: WorkspaceDocument::new("Portable fixture".into()),
            linked: false,
        }
    }

    fn preview_exchange(
        preview: &WorkspaceCreationPreview,
    ) -> (ClientMessage, Result<ResponsePayload, ProtoError>) {
        (
            ClientMessage::PreviewWorkspaceCreation {
                root_dir: preview.root_dir.clone(),
                source: WorkspaceImportSource::Text(PORTABLE_WORKSPACE.into()),
                linked: false,
            },
            Ok(ResponsePayload::WorkspaceCreationPreview(preview.clone())),
        )
    }

    #[tokio::test]
    async fn new_workspace_import_accepts_legacy_pong_without_health() {
        let dir = TempDir::new().unwrap();
        let preview = creation_preview(dir.path());
        let id = WorkspaceId(uuid::Uuid::new_v4());
        let workspace = WorkspaceInfo {
            id,
            name: preview.incoming.name.clone(),
            root_dir: preview.root_dir.clone(),
            project: Default::default(),
            setup: None,
            default_panes_pending: None,
            slot_index: 0,
            injected_env: Default::default(),
            panes: Vec::new(),
            port_offset: 0,
            compose_project_name: None,
            git_branch: None,
            git_status: None,
            bound_ports: Vec::new(),
            containers: Vec::new(),
            external_statuses: Vec::new(),
            manifest: None,
        };
        let output = import(
            ["--new", dir.path().to_str().unwrap()],
            vec![
                preview_exchange(&preview),
                (ClientMessage::Ping, Ok(pong(None))),
                (
                    ClientMessage::ApplyWorkspaceCreation { preview },
                    Ok(ResponsePayload::WorkbenchCreated(workspace)),
                ),
            ],
        )
        .await;
        assert!(output.status.success(), "{output:?}");
        assert_eq!(
            serde_json::from_slice::<WorkspaceId>(&output.stdout).unwrap(),
            id
        );
        assert!(output.stderr.is_empty(), "{output:?}");
    }

    #[tokio::test]
    async fn rejected_new_workspace_import_reports_only_the_apply_error() {
        let dir = TempDir::new().unwrap();
        let preview = creation_preview(dir.path());
        let output = import(
            ["--new", dir.path().to_str().unwrap()],
            vec![
                preview_exchange(&preview),
                (
                    ClientMessage::Ping,
                    Ok(pong(Some(PersistenceHealth::TemporarySnapshotRejected))),
                ),
                (
                    ClientMessage::ApplyWorkspaceCreation { preview },
                    Err(ProtoError::InvalidRequest("creation rejected".into())),
                ),
            ],
        )
        .await;
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
        assert_eq!(
            output.stderr,
            b"Error: InvalidRequest(\"creation rejected\")\n"
        );
    }

    #[tokio::test]
    async fn new_workspace_import_stops_before_apply_if_persistence_preflight_fails() {
        let dir = TempDir::new().unwrap();
        let preview = creation_preview(dir.path());
        let output = import(
            ["--new", dir.path().to_str().unwrap()],
            vec![
                preview_exchange(&preview),
                (
                    ClientMessage::Ping,
                    Err(ProtoError::Internal("health unavailable".into())),
                ),
            ],
        )
        .await;
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
        assert_eq!(output.stderr, b"Error: Internal(\"health unavailable\")\n");
    }

    #[tokio::test]
    async fn existing_workspace_import_skips_persistence_preflight() {
        let workspace_id = WorkspaceId(uuid::Uuid::new_v4());
        let document = WorkspaceDocument::new("Portable fixture".into());
        let output = import(
            ["--workspace", &workspace_id.to_string()],
            vec![
                (
                    ClientMessage::PreviewWorkspaceImport {
                        workspace_id,
                        source: WorkspaceImportSource::Text(PORTABLE_WORKSPACE.into()),
                        linked: false,
                    },
                    Ok(ResponsePayload::WorkspaceImportPreview(
                        WorkspaceImportPreview {
                            workspace_id,
                            revision: 1,
                            incoming: document.clone(),
                            current: WorkspaceDocument::new("Original workspace".into()),
                            proposed: document.clone(),
                            linked: false,
                            conflicts: Vec::new(),
                        },
                    )),
                ),
                (
                    ClientMessage::ApplyWorkspaceImport {
                        workspace_id,
                        revision: 1,
                        incoming: document.clone(),
                        linked: false,
                        resolutions: Default::default(),
                    },
                    Ok(ResponsePayload::Workbench(WorkbenchState {
                        revision: 2,
                        document: document.clone(),
                        repo_baseline: None,
                    })),
                ),
            ],
        )
        .await;
        assert!(output.status.success(), "{output:?}");
        let state: WorkbenchState = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(state.revision, 2);
        assert_eq!(state.document, document);
        assert!(output.stderr.is_empty(), "{output:?}");
    }
}
