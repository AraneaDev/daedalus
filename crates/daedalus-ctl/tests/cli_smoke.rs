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

    let server = DaemonServer::bind_with_supervisor(
        std::path::PathBuf::from("tcp://127.0.0.1:0"),
        runtime,
        test_supervisor(),
    )
    .await
    .unwrap()
    .with_auth_token(Some("cli-secret-token".to_string()));
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
