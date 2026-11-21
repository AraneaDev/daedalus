use daedalus_proto::{
    ClientMessage, PaneRole, Request, ResponsePayload, ServerEvent, ServerMessage, read_frame,
    write_frame,
};
use daedalusd::server::DaemonServer;
use daedalusd::supervisor::{IsolationMode, Supervisor};
use std::collections::HashMap;
use std::time::Duration;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::sync::oneshot;

async fn send_rpc(stream: &mut UnixStream, id: u64, payload: ClientMessage) -> ResponsePayload {
    write_frame(stream, &Request { id, payload }).await.unwrap();
    loop {
        match read_frame::<_, ServerMessage>(stream)
            .await
            .unwrap()
            .unwrap()
        {
            ServerMessage::Response { id: rid, result } if rid == id => return result.unwrap(),
            ServerMessage::Event(_) => continue,
            other => panic!("unexpected server message: {other:?}"),
        }
    }
}

async fn wait_for_command_finished(
    stream: &mut UnixStream,
    expected_code: i32,
) -> (Option<String>, i32, bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(ServerMessage::Event(ServerEvent::CommandFinished {
                command,
                exit_code,
                has_failure_trace,
                ..
            })) = read_frame::<_, ServerMessage>(stream).await.unwrap()
                && exit_code == expected_code
            {
                return (command, exit_code, has_failure_trace);
            }
        }
    })
    .await
    .expect("timed out waiting for CommandFinished")
}

#[tokio::test]
async fn bash_osc133_captures_failure_trace_and_survives_client_reconnect() {
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

    // Client 1: create workspace, spawn bash runner pane, subscribe
    let mut c1 = UnixStream::connect(&sock).await.unwrap();
    let ws = match send_rpc(
        &mut c1,
        1,
        ClientMessage::CreateWorkspace {
            name: "demo".to_string(),
            root_dir: dir.path().to_path_buf(),
            env: HashMap::new(),
        },
    )
    .await
    {
        ResponsePayload::WorkspaceCreated(ws) => ws,
        other => panic!("unexpected: {other:?}"),
    };

    let pane = match send_rpc(
        &mut c1,
        2,
        ClientMessage::SpawnPane {
            workspace_id: ws.id,
            role: PaneRole::Runner,
            command: None,
            shell: Some("/bin/bash".to_string()),
            cols: 120,
            rows: 30,
            env: HashMap::new(),
        },
    )
    .await
    {
        ResponsePayload::PaneSpawned(p) => p,
        other => panic!("unexpected: {other:?}"),
    };

    send_rpc(
        &mut c1,
        3,
        ClientMessage::Subscribe {
            pane_ids: Some(vec![pane.id]),
        },
    )
    .await;

    // Wait briefly for initial bash prompt
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Run passing command
    send_rpc(
        &mut c1,
        4,
        ClientMessage::SendInput {
            pane_id: pane.id,
            data: b"echo hello_ok\n".to_vec(),
        },
    )
    .await;
    let (_, code, has_trace) = wait_for_command_finished(&mut c1, 0).await;
    assert_eq!(code, 0);
    assert!(!has_trace);

    // Run failing command with ANSI red text
    send_rpc(
        &mut c1,
        5,
        ClientMessage::SendInput {
            pane_id: pane.id,
            data: b"sh -c 'printf \"\\033[31mFAIL_MARKER\\033[0m\\n\"; exit 42'\n".to_vec(),
        },
    )
    .await;
    let (_, code, has_trace) = wait_for_command_finished(&mut c1, 42).await;
    assert_eq!(code, 42);
    assert!(has_trace);

    // Disconnect client 1 and reconnect with client 2
    drop(c1);
    let mut c2 = UnixStream::connect(&sock).await.unwrap();

    let trace = match send_rpc(
        &mut c2,
        10,
        ClientMessage::GetFailureTrace { pane_id: pane.id },
    )
    .await
    {
        ResponsePayload::FailureTrace(Some(t)) => t,
        other => panic!("expected failure trace, got: {other:?}"),
    };
    assert_eq!(trace.exit_code, 42);
    assert!(
        trace.output.contains("FAIL_MARKER"),
        "trace output missing FAIL_MARKER: {:?}",
        trace.output
    );
    assert!(!trace.output.contains("\x1b[31m"));

    // Clean close
    send_rpc(
        &mut c2,
        11,
        ClientMessage::CloseWorkspace {
            id: ws.id,
            compose_down: false,
        },
    )
    .await;
    let _ = shutdown_tx.send(());
    srv_handle.await.unwrap();
}

#[tokio::test]
async fn zsh_osc133_captures_failure_trace_when_zsh_installed() {
    let zsh_path = match std::process::Command::new("which").arg("zsh").output() {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        _ => return, // zsh not installed on host; skip gracefully
    };

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

    let mut client = UnixStream::connect(&sock).await.unwrap();
    let ws = match send_rpc(
        &mut client,
        1,
        ClientMessage::CreateWorkspace {
            name: "zsh-demo".to_string(),
            root_dir: dir.path().to_path_buf(),
            env: HashMap::new(),
        },
    )
    .await
    {
        ResponsePayload::WorkspaceCreated(ws) => ws,
        other => panic!("unexpected: {other:?}"),
    };

    let pane = match send_rpc(
        &mut client,
        2,
        ClientMessage::SpawnPane {
            workspace_id: ws.id,
            role: PaneRole::Runner,
            command: None,
            shell: Some(zsh_path),
            cols: 120,
            rows: 30,
            env: HashMap::new(),
        },
    )
    .await
    {
        ResponsePayload::PaneSpawned(p) => p,
        other => panic!("unexpected: {other:?}"),
    };

    send_rpc(
        &mut client,
        3,
        ClientMessage::Subscribe {
            pane_ids: Some(vec![pane.id]),
        },
    )
    .await;

    tokio::time::sleep(Duration::from_millis(300)).await;

    send_rpc(
        &mut client,
        4,
        ClientMessage::SendInput {
            pane_id: pane.id,
            data: b"sh -c 'echo ZSH_FAIL_MARKER; exit 17'\n".to_vec(),
        },
    )
    .await;
    let (_, code, has_trace) = wait_for_command_finished(&mut client, 17).await;
    assert_eq!(code, 17);
    assert!(has_trace);

    let trace = match send_rpc(
        &mut client,
        5,
        ClientMessage::GetFailureTrace { pane_id: pane.id },
    )
    .await
    {
        ResponsePayload::FailureTrace(Some(t)) => t,
        other => panic!("expected failure trace, got: {other:?}"),
    };
    assert_eq!(trace.exit_code, 17);
    assert!(trace.output.contains("ZSH_FAIL_MARKER"));

    send_rpc(
        &mut client,
        6,
        ClientMessage::CloseWorkspace {
            id: ws.id,
            compose_down: false,
        },
    )
    .await;
    let _ = shutdown_tx.send(());
    srv_handle.await.unwrap();
}

#[tokio::test]
async fn get_scrollback_returns_lines_scrolled_above_viewport() {
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

    let mut client = UnixStream::connect(&sock).await.unwrap();
    let ws = match send_rpc(
        &mut client,
        1,
        ClientMessage::CreateWorkspace {
            name: "scrollback-demo".to_string(),
            root_dir: dir.path().to_path_buf(),
            env: HashMap::new(),
        },
    )
    .await
    {
        ResponsePayload::WorkspaceCreated(ws) => ws,
        other => panic!("unexpected: {other:?}"),
    };

    let pane = match send_rpc(
        &mut client,
        2,
        ClientMessage::SpawnPane {
            workspace_id: ws.id,
            role: PaneRole::Runner,
            command: Some(vec![
                "sh".to_string(),
                "-c".to_string(),
                "for i in 1 2 3 4 5 6 7 8 9 10; do echo LINE_$i; done; sleep 2".to_string(),
            ]),
            shell: None,
            cols: 80,
            rows: 4,
            env: HashMap::new(),
        },
    )
    .await
    {
        ResponsePayload::PaneSpawned(p) => p,
        other => panic!("unexpected: {other:?}"),
    };

    let rows_to_text = |rows: &[daedalus_proto::GridRow]| -> Vec<String> {
        rows.iter()
            .map(|r| {
                r.cells
                    .iter()
                    .map(|c| c.text.as_str())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    };

    let snap = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snap = match send_rpc(
                &mut client,
                3,
                ClientMessage::GetSnapshot { pane_id: pane.id },
            )
            .await
            {
                ResponsePayload::Snapshot(s) => s,
                other => panic!("unexpected: {other:?}"),
            };
            let text = rows_to_text(&snap.rows_data).join("\n");
            if text.contains("LINE_10") {
                return snap;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("timed out waiting for LINE_10 in snapshot");

    assert!(
        snap.scrollback_len >= 6,
        "expected scrollback_len >= 6, got {}",
        snap.scrollback_len
    );

    // Query with offset_from_bottom: 6, count: 4 (because echo LINE_10 emits a trailing newline
    // that leaves row 3 blank, total_scrollback is 7 and offset 6 covers LINE_2..LINE_5;
    // querying at offset_from_bottom = total_scrollback covers LINE_1..LINE_4).
    let (total_scrollback, rows_at_6) = match send_rpc(
        &mut client,
        4,
        ClientMessage::GetScrollback {
            pane_id: pane.id,
            offset_from_bottom: 6,
            count: 4,
        },
    )
    .await
    {
        ResponsePayload::ScrollbackRows {
            pane_id,
            total_scrollback,
            rows,
        } => {
            assert_eq!(pane_id, pane.id);
            (total_scrollback, rows)
        }
        other => panic!("unexpected: {other:?}"),
    };
    assert_eq!(total_scrollback, 7);
    let text_at_6 = rows_to_text(&rows_at_6);
    assert_eq!(text_at_6, vec!["LINE_2", "LINE_3", "LINE_4", "LINE_5"]);

    let (_, top_rows) = match send_rpc(
        &mut client,
        5,
        ClientMessage::GetScrollback {
            pane_id: pane.id,
            offset_from_bottom: total_scrollback,
            count: 4,
        },
    )
    .await
    {
        ResponsePayload::ScrollbackRows {
            pane_id,
            total_scrollback,
            rows,
        } => {
            assert_eq!(pane_id, pane.id);
            (total_scrollback, rows)
        }
        other => panic!("unexpected: {other:?}"),
    };
    let top_text = rows_to_text(&top_rows);
    assert_eq!(top_text, vec!["LINE_1", "LINE_2", "LINE_3", "LINE_4"]);

    send_rpc(
        &mut client,
        6,
        ClientMessage::CloseWorkspace {
            id: ws.id,
            compose_down: false,
        },
    )
    .await;
    let _ = shutdown_tx.send(());
    srv_handle.await.unwrap();
}
