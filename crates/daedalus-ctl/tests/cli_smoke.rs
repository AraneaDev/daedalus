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

    let close = Command::new(bin)
        .args(["--socket", sock.to_str().unwrap(), "workspace", "close", &ws_id])
        .output()
        .await
        .unwrap();
    assert!(close.status.success());

    let _ = shutdown_tx.send(());
    srv_handle.await.unwrap();
}
