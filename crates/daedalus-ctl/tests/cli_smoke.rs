use daedalusd::server::DaemonServer;
use daedalusd::supervisor::{IsolationMode, Supervisor};
use tempfile::TempDir;
use tokio::process::Command;
use tokio::sync::oneshot;

#[tokio::test]
async fn cli_smoke_ping_workspace_and_pane_lifecycle() {
    let dir = TempDir::new().unwrap();
    let sock = dir.path().join("daedalusd.sock");
    let runtime = dir.path().join("runtime");

    let server = DaemonServer::bind_with_supervisor(
        sock.clone(),
        runtime,
        Supervisor::with_mode(IsolationMode::PgidFallback),
    )
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
            "/bin/bash",
        ])
        .output()
        .await
        .unwrap();
    assert!(spawn.status.success());
    let pane_id = String::from_utf8_lossy(&spawn.stdout).trim().to_string();

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

    let snap = Command::new(bin)
        .args(["--socket", sock.to_str().unwrap(), "pane", "snapshot", &pane_id])
        .output()
        .await
        .unwrap();
    assert!(snap.status.success());
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
        .args(["--socket", sock.to_str().unwrap(), "workspace", "close", &ws_id])
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

    let server = DaemonServer::bind_with_supervisor(
        sock.clone(),
        runtime,
        Supervisor::with_mode(IsolationMode::PgidFallback),
    )
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
