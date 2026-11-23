use daedalus_proto::transport::IpcStream;
use daedalus_proto::{
    AgentState, ClientMessage, PaneRole, ProtoError, Request, ResponsePayload, ServerEvent,
    ServerMessage, StatusLevel, read_frame, write_frame,
};
use daedalusd::server::DaemonServer;
use daedalusd::supervisor::{IsolationMode, Supervisor};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
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

fn sleep_command() -> Vec<String> {
    #[cfg(unix)]
    {
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "sleep 10".to_string(),
        ]
    }
    #[cfg(windows)]
    {
        vec![
            "ping.exe".to_string(),
            "-n".to_string(),
            "11".to_string(),
            "127.0.0.1".to_string(),
        ]
    }
}

fn echo_worktree_env_command() -> Vec<String> {
    #[cfg(unix)]
    {
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "echo PORT=$PORT PROJ=$COMPOSE_PROJECT_NAME DB=$DATABASE_URL; sleep 2".to_string(),
        ]
    }
    #[cfg(windows)]
    {
        vec![
            "cmd.exe".to_string(),
            "/C".to_string(),
            "echo".to_string(),
            "PORT=%PORT%".to_string(),
            "PROJ=%COMPOSE_PROJECT_NAME%".to_string(),
            "DB=%DATABASE_URL%".to_string(),
            "&".to_string(),
            "ping".to_string(),
            "-n".to_string(),
            "4".to_string(),
            "127.0.0.1".to_string(),
        ]
    }
}

fn echo_restored_env_command() -> Vec<String> {
    #[cfg(unix)]
    {
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "echo RESTORED=$RESTORED_ENV; sleep 2".to_string(),
        ]
    }
    #[cfg(windows)]
    {
        vec![
            "cmd.exe".to_string(),
            "/C".to_string(),
            "echo".to_string(),
            "RESTORED=%RESTORED_ENV%".to_string(),
            "&".to_string(),
            "ping".to_string(),
            "-n".to_string(),
            "4".to_string(),
            "127.0.0.1".to_string(),
        ]
    }
}

async fn send_rpc_result(
    stream: &mut IpcStream,
    id: u64,
    payload: ClientMessage,
) -> Result<ResponsePayload, ProtoError> {
    write_frame(stream, &Request { id, payload }).await.unwrap();
    loop {
        match read_frame::<_, ServerMessage>(stream)
            .await
            .unwrap()
            .unwrap()
        {
            ServerMessage::Response { id: rid, result } if rid == id => return result,
            ServerMessage::Event(_) => continue,
            other => panic!("unexpected server message: {other:?}"),
        }
    }
}

async fn send_rpc(stream: &mut IpcStream, id: u64, payload: ClientMessage) -> ResponsePayload {
    send_rpc_result(stream, id, payload).await.unwrap()
}

#[tokio::test]
async fn interrupted_default_initialization_resumes_from_another_client() {
    use ResponsePayload::{DefaultPaneExisting, PaneSpawned, WorkspaceCreated, Workspaces};
    let dir = TempDir::new().unwrap();
    let socket = dir.path().join("daemon.sock");
    let server = DaemonServer::bind_with_supervisor(
        socket.clone(),
        dir.path().join("runtime"),
        test_supervisor(),
    )
    .await
    .unwrap();
    let (shutdown, shutdown_rx) = oneshot::channel::<()>();
    let task = tokio::spawn(server.serve(async {
        let _ = shutdown_rx.await;
    }));
    let mut first = IpcStream::connect(&socket).await.unwrap();
    let WorkspaceCreated(ws) = send_rpc(
        &mut first,
        1,
        ClientMessage::CreateWorkspace {
            name: "partial".into(),
            root_dir: dir.path().into(),
            env: HashMap::new(),
        },
    )
    .await
    else {
        panic!("expected a workspace")
    };
    let ensure = |role, command| ClientMessage::EnsureDefaultPane {
        workspace_id: ws.id,
        role,
        command: Some(command),
        cols: 80,
        rows: 24,
    };
    let PaneSpawned(agent) =
        send_rpc(&mut first, 2, ensure(PaneRole::Agent, sleep_command())).await
    else {
        panic!("expected a new agent")
    };
    drop(first);

    let mut second = IpcStream::connect(&socket).await.unwrap();
    let Workspaces(partial) = send_rpc(&mut second, 1, ClientMessage::ListWorkspaces).await else {
        panic!("expected workspace list")
    };
    assert!(partial[0].needs_default_panes());
    assert_eq!(partial[0].panes.len(), 1);
    assert!(
        matches!(send_rpc(&mut second, 2, ensure(PaneRole::Agent, sleep_command())).await, DefaultPaneExisting(Some(p)) if p.id == agent.id)
    );
    let failed = send_rpc_result(
        &mut second,
        3,
        ensure(PaneRole::Service, vec!["absent-service-82f52".into()]),
    )
    .await;
    assert!(matches!(failed, Err(ProtoError::SpawnFailed(_))));
    for (id, role) in [(4, PaneRole::Service), (5, PaneRole::Runner)] {
        assert!(matches!(
            send_rpc(&mut second, id, ensure(role, sleep_command())).await,
            PaneSpawned(_)
        ));
    }
    let Workspaces(done) = send_rpc(&mut second, 6, ClientMessage::ListWorkspaces).await else {
        panic!("expected workspace list")
    };
    assert_eq!(done[0].panes.len(), 3);
    assert!(!done[0].needs_default_panes());
    drop(second);
    shutdown.send(()).unwrap();
    task.await.unwrap().unwrap();
}

#[cfg(unix)]
async fn wait_for_command_finished(
    stream: &mut IpcStream,
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

#[cfg(unix)]
#[tokio::test]
async fn bash_osc133_captures_failure_trace_and_survives_client_reconnect() {
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

    // Client 1: create workspace, spawn bash runner pane, subscribe
    let mut c1 = IpcStream::connect(&sock).await.unwrap();
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
    let mut c2 = IpcStream::connect(&sock).await.unwrap();

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
            remove_worktree: false,
            force_remove_worktree: false,
        },
    )
    .await;
    let _ = shutdown_tx.send(());
    srv_handle.await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn zsh_osc133_captures_failure_trace_when_zsh_installed() {
    let zsh_path = match std::process::Command::new("which").arg("zsh").output() {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        _ => return, // zsh not installed on host; skip gracefully
    };

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

    let mut client = IpcStream::connect(&sock).await.unwrap();
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
            remove_worktree: false,
            force_remove_worktree: false,
        },
    )
    .await;
    let _ = shutdown_tx.send(());
    srv_handle.await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn get_scrollback_returns_lines_scrolled_above_viewport() {
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

    let mut client = IpcStream::connect(&sock).await.unwrap();
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

    match send_rpc(
        &mut client,
        6,
        ClientMessage::SearchPane {
            pane_id: pane.id,
            query: "line_".to_string(),
            case_sensitive: false,
        },
    )
    .await
    {
        ResponsePayload::SearchMatches {
            pane_id,
            total_scrollback: search_scrollback,
            matches,
        } => {
            assert_eq!(pane_id, pane.id);
            assert_eq!(search_scrollback, 7);
            assert_eq!(matches.len(), 10);
            assert_eq!(matches[0].start_col, 0);
            assert_eq!(matches[0].end_col, 5);
        }
        other => panic!("unexpected: {other:?}"),
    }

    match send_rpc(
        &mut client,
        7,
        ClientMessage::SearchPane {
            pane_id: pane.id,
            query: "line_".to_string(),
            case_sensitive: true,
        },
    )
    .await
    {
        ResponsePayload::SearchMatches { matches, .. } => {
            assert!(
                matches.is_empty(),
                "case-sensitive lowercase should not match"
            );
        }
        other => panic!("unexpected: {other:?}"),
    }

    match send_rpc(
        &mut client,
        8,
        ClientMessage::SearchPane {
            pane_id: pane.id,
            query: "LINE_2".to_string(),
            case_sensitive: true,
        },
    )
    .await
    {
        ResponsePayload::SearchMatches {
            total_scrollback: search_scrollback,
            matches,
            ..
        } => {
            assert_eq!(matches.len(), 1);
            assert!(
                matches[0].abs_row < search_scrollback as u64,
                "LINE_2 match should be in scrollback above viewport: {matches:?}"
            );
        }
        other => panic!("unexpected: {other:?}"),
    }

    send_rpc(
        &mut client,
        9,
        ClientMessage::CloseWorkspace {
            id: ws.id,
            compose_down: false,
            remove_worktree: false,
            force_remove_worktree: false,
        },
    )
    .await;
    let _ = shutdown_tx.send(());
    srv_handle.await.unwrap();
}

fn run_git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("git command should execute");
    assert!(
        output.status.success(),
        "git {:?} failed: stdout={}, stderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn create_worktree_workspace_isolates_ports_and_injects_env() {
    let dir = TempDir::new().unwrap();
    let sock = dir.path().join("daedalusd.sock");
    let runtime = dir.path().join("runtime");
    let worktrees_dir = dir.path().join("worktrees");
    let repo_dir = dir.path().join("app");
    std::fs::create_dir_all(&repo_dir).unwrap();

    run_git(&repo_dir, &["init", "-b", "main"]);
    std::fs::write(
        repo_dir.join("docker-compose.yml"),
        "services:\n  web:\n    image: node:20\n    ports:\n      - \"3000:3000\"\n",
    )
    .unwrap();
    std::fs::write(
        repo_dir.join(".env"),
        "PORT=3000\nDATABASE_URL=postgres://localhost:5432/app\n",
    )
    .unwrap();
    run_git(&repo_dir, &["add", "docker-compose.yml", ".env"]);
    run_git(
        &repo_dir,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-m",
            "initial commit",
        ],
    );

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

    let mut client = IpcStream::connect(&sock).await.unwrap();

    // Open primary workspace (slot 0, port_offset 0)
    let primary_ws = match send_rpc(
        &mut client,
        1,
        ClientMessage::CreateWorkspace {
            name: "app".to_string(),
            root_dir: repo_dir.clone(),
            env: HashMap::new(),
        },
    )
    .await
    {
        ResponsePayload::WorkspaceCreated(ws) => ws,
        other => panic!("unexpected response: {other:?}"),
    };
    assert_eq!(primary_ws.slot_index, 0);
    assert_eq!(primary_ws.port_offset, 0);
    assert_eq!(primary_ws.git_branch.as_deref(), Some("main"));

    // Create worktree workspace for branch feat/billing (slot 1, offset >= 100)
    let wt_ws = match send_rpc(
        &mut client,
        2,
        ClientMessage::CreateWorktreeWorkspace {
            repo_dir: repo_dir.clone(),
            branch: "feat/billing".to_string(),
            create_branch: true,
            env: HashMap::new(),
        },
    )
    .await
    {
        ResponsePayload::WorkspaceCreated(ws) => ws,
        other => panic!("unexpected response: {other:?}"),
    };
    assert_eq!(wt_ws.slot_index, 1);
    assert!(
        wt_ws.port_offset >= 100,
        "expected port_offset >= 100, got {}",
        wt_ws.port_offset
    );
    assert_eq!(wt_ws.git_branch.as_deref(), Some("feat/billing"));
    let expected_compose_project = wt_ws
        .compose_project_name
        .clone()
        .expect("compose_project_name should be set");
    let expected_port = 3000 + wt_ws.port_offset;
    let expected_db_port = 5432 + wt_ws.port_offset;

    let worktrees = match send_rpc(
        &mut client,
        3,
        ClientMessage::DiscoverWorktrees {
            repo_dir: repo_dir.clone(),
        },
    )
    .await
    {
        ResponsePayload::Worktrees(wts) => wts,
        other => panic!("unexpected response: {other:?}"),
    };
    assert_eq!(worktrees.len(), 2);

    let override_path = wt_ws
        .root_dir
        .join(".daedalus")
        .join("compose.override.yml");
    assert!(
        override_path.is_file(),
        "expected compose.override.yml at {}",
        override_path.display()
    );
    let override_contents = std::fs::read_to_string(&override_path).unwrap();
    assert!(
        override_contents.contains("!override"),
        "expected !override in compose.override.yml:\n{override_contents}"
    );
    assert!(
        override_contents.contains(&format!("{expected_port}:3000")),
        "expected remapped port {expected_port}:3000 in compose.override.yml:\n{override_contents}"
    );

    // Spawn a pane inside the worktree and verify it observes remapped PORT and COMPOSE_PROJECT_NAME
    let pane = match send_rpc(
        &mut client,
        4,
        ClientMessage::SpawnPane {
            workspace_id: wt_ws.id,
            role: PaneRole::Service,
            command: Some(echo_worktree_env_command()),
            shell: None,
            cols: 160,
            rows: 12,
            env: HashMap::new(),
        },
    )
    .await
    {
        ResponsePayload::PaneSpawned(p) => p,
        other => panic!("unexpected response: {other:?}"),
    };

    let expected_line = format!(
        "PORT={expected_port} PROJ={expected_compose_project} DB=postgres://localhost:{expected_db_port}/app"
    );
    let snap_text = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snap = match send_rpc(
                &mut client,
                5,
                ClientMessage::GetSnapshot { pane_id: pane.id },
            )
            .await
            {
                ResponsePayload::Snapshot(s) => s,
                other => panic!("unexpected response: {other:?}"),
            };
            let text: String = snap
                .rows_data
                .iter()
                .map(|r| {
                    r.cells
                        .iter()
                        .map(|c| c.text.as_str())
                        .collect::<String>()
                        .trim_end()
                        .to_string()
                })
                .collect::<Vec<_>>()
                .join("\n");
            if text.contains(&expected_line) {
                return text;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("timed out waiting for remapped env in worktree pane");
    assert!(snap_text.contains(&expected_line));

    send_rpc(
        &mut client,
        6,
        ClientMessage::CloseWorkspace {
            id: wt_ws.id,
            compose_down: false,
            remove_worktree: false,
            force_remove_worktree: false,
        },
    )
    .await;
    send_rpc(
        &mut client,
        7,
        ClientMessage::CloseWorkspace {
            id: primary_ws.id,
            compose_down: false,
            remove_worktree: false,
            force_remove_worktree: false,
        },
    )
    .await;

    let _ = shutdown_tx.send(());
    srv_handle.await.unwrap();
}

#[tokio::test]
async fn external_status_ingests_via_ndjson_socket_and_rpc() {
    let dir = TempDir::new().unwrap();
    let sock = dir.path().join("daedalusd.sock");
    let events_sock = dir.path().join("events.sock");
    let runtime = dir.path().join("runtime");
    let ws_dir = dir.path().join("project");
    std::fs::create_dir_all(&ws_dir).unwrap();

    let server = DaemonServer::bind_with_events_socket(
        sock.clone(),
        runtime,
        Some(events_sock.clone()),
        test_supervisor(),
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

    let mut client = IpcStream::connect(&sock).await.unwrap();
    let ws = match send_rpc(
        &mut client,
        1,
        ClientMessage::CreateWorkspace {
            name: "project".to_string(),
            root_dir: ws_dir.clone(),
            env: HashMap::new(),
        },
    )
    .await
    {
        ResponsePayload::WorkspaceCreated(ws) => ws,
        other => panic!("unexpected response: {other:?}"),
    };

    // Subscribe on a dedicated listener stream so send_rpc on `client` cannot swallow events
    let mut sub_client = IpcStream::connect(&sock).await.unwrap();
    send_rpc(
        &mut sub_client,
        2,
        ClientMessage::Subscribe { pane_ids: None },
    )
    .await;

    // 1. Write 1-line JSON status for phrourion to events.sock
    let mut hook_stream = IpcStream::connect(&events_sock).await.unwrap();
    let ndjson = format!(
        "{}\n",
        serde_json::json!({
            "source": "phrourion",
            "root_dir": ws_dir,
            "level": "warn",
            "label": "3 dirty",
            "detail": "2 modified, 1 untracked"
        })
    );
    hook_stream.write_all(ndjson.as_bytes()).await.unwrap();
    hook_stream.shutdown().await.unwrap();
    drop(hook_stream);

    // 2. Send PublishExternalStatus RPC for cassandra
    send_rpc(
        &mut client,
        3,
        ClientMessage::PublishExternalStatus {
            workspace_id: Some(ws.id),
            root_dir: None,
            source: "cassandra".to_string(),
            level: StatusLevel::Error,
            label: "retry 3/5".to_string(),
            detail: Some("rate limited".to_string()),
        },
    )
    .await;

    // Assert both ExternalStatusUpdated events arrive on the subscribed client
    let mut seen_sources = HashMap::new();
    tokio::time::timeout(Duration::from_secs(3), async {
        while seen_sources.len() < 2 {
            match read_frame::<_, ServerMessage>(&mut sub_client)
                .await
                .unwrap()
            {
                Some(ServerMessage::Event(ServerEvent::ExternalStatusUpdated {
                    workspace_id,
                    status,
                })) if workspace_id == ws.id => {
                    seen_sources.insert(status.source.clone(), status);
                }
                Some(ServerMessage::Event(_)) => continue,
                other => panic!("unexpected message while waiting for status events: {other:?}"),
            }
        }
    })
    .await
    .expect("timed out waiting for ExternalStatusUpdated events");

    let phrourion = seen_sources.get("phrourion").unwrap();
    assert_eq!(phrourion.level, StatusLevel::Warn);
    assert_eq!(phrourion.label, "3 dirty");
    assert_eq!(phrourion.detail.as_deref(), Some("2 modified, 1 untracked"));

    let cassandra = seen_sources.get("cassandra").unwrap();
    assert_eq!(cassandra.level, StatusLevel::Error);
    assert_eq!(cassandra.label, "retry 3/5");
    assert_eq!(cassandra.detail.as_deref(), Some("rate limited"));

    // Assert both appear in ListWorkspaces
    let workspaces = match send_rpc(&mut client, 4, ClientMessage::ListWorkspaces).await {
        ResponsePayload::Workspaces(list) => list,
        other => panic!("unexpected response: {other:?}"),
    };
    assert_eq!(workspaces.len(), 1);
    assert_eq!(workspaces[0].external_statuses.len(), 2);
    assert!(
        workspaces[0]
            .external_statuses
            .iter()
            .any(|s| s.source == "phrourion" && s.label == "3 dirty")
    );
    assert!(
        workspaces[0]
            .external_statuses
            .iter()
            .any(|s| s.source == "cassandra" && s.label == "retry 3/5")
    );

    let _ = shutdown_tx.send(());
    srv_handle.await.unwrap();
    #[cfg(unix)]
    assert!(
        !events_sock.exists(),
        "events.sock should be removed on daemon shutdown"
    );
}

#[tokio::test]
async fn persistence_restores_dormant_workspaces_across_daemon_restart_and_skips_deleted_dirs() {
    let dir = TempDir::new().unwrap();
    let sock = dir.path().join("daedalusd.sock");
    let runtime = dir.path().join("runtime");
    let state_path = dir.path().join("state").join("state.json");

    let keep_dir = dir.path().join("keep");
    let gone_dir = dir.path().join("gone");
    std::fs::create_dir_all(&keep_dir).unwrap();
    std::fs::create_dir_all(&gone_dir).unwrap();
    std::fs::write(
        keep_dir.join(".daedalus.toml"),
        "[agent]\npreset = \"agy\"\n[env]\nRESTORED_ENV = \"from_manifest\"\n",
    )
    .unwrap();

    // Daemon 1: create two workspaces and shut down cleanly.
    let server1 = DaemonServer::bind_with_options(
        sock.clone(),
        runtime.clone(),
        None,
        Some(state_path.clone()),
        test_supervisor(),
    )
    .await
    .unwrap();
    let (shutdown1_tx, shutdown1_rx) = oneshot::channel::<()>();
    let srv1_handle = tokio::spawn(async move {
        let _ = server1
            .serve(async {
                let _ = shutdown1_rx.await;
            })
            .await;
    });

    let mut c1 = IpcStream::connect(&sock).await.unwrap();
    let ws_keep = match send_rpc(
        &mut c1,
        1,
        ClientMessage::CreateWorkspace {
            name: "keep".to_string(),
            root_dir: keep_dir.clone(),
            env: HashMap::new(),
        },
    )
    .await
    {
        ResponsePayload::WorkspaceCreated(ws) => ws,
        other => panic!("unexpected: {other:?}"),
    };
    let _ws_gone = match send_rpc(
        &mut c1,
        2,
        ClientMessage::CreateWorkspace {
            name: "gone".to_string(),
            root_dir: gone_dir.clone(),
            env: HashMap::new(),
        },
    )
    .await
    {
        ResponsePayload::WorkspaceCreated(ws) => ws,
        other => panic!("unexpected: {other:?}"),
    };
    drop(c1);

    let raw_state = std::fs::read_to_string(&state_path).unwrap();
    assert!(!raw_state.contains("from_manifest"));

    let _ = shutdown1_tx.send(());
    srv1_handle.await.unwrap();

    // Remove gone_dir before starting daemon 2.
    std::fs::remove_dir_all(&gone_dir).unwrap();

    let server2 = DaemonServer::bind_with_options(
        sock.clone(),
        runtime,
        None,
        Some(state_path.clone()),
        test_supervisor(),
    )
    .await
    .unwrap();
    let (shutdown2_tx, shutdown2_rx) = oneshot::channel::<()>();
    let srv2_handle = tokio::spawn(async move {
        let _ = server2
            .serve(async {
                let _ = shutdown2_rx.await;
            })
            .await;
    });

    let mut c2 = IpcStream::connect(&sock).await.unwrap();
    let listed = match send_rpc(&mut c2, 1, ClientMessage::ListWorkspaces).await {
        ResponsePayload::Workspaces(list) => list,
        other => panic!("unexpected: {other:?}"),
    };
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, ws_keep.id);
    assert!(
        listed[0].panes.is_empty(),
        "restored workspace must be dormant"
    );
    assert_eq!(
        listed[0]
            .manifest
            .as_ref()
            .and_then(|m| m.agent_preset.as_deref()),
        Some("agy")
    );
    assert_eq!(
        listed[0]
            .injected_env
            .get("RESTORED_ENV")
            .map(String::as_str),
        Some("from_manifest")
    );

    // Restored dormant workspace can lazily spawn panes observing refreshed manifest env.
    let pane = match send_rpc(
        &mut c2,
        2,
        ClientMessage::SpawnPane {
            workspace_id: ws_keep.id,
            role: PaneRole::Runner,
            command: Some(echo_restored_env_command()),
            shell: None,
            cols: 80,
            rows: 8,
            env: HashMap::new(),
        },
    )
    .await
    {
        ResponsePayload::PaneSpawned(p) => p,
        other => panic!("unexpected: {other:?}"),
    };

    let saw_env = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snap =
                match send_rpc(&mut c2, 3, ClientMessage::GetSnapshot { pane_id: pane.id }).await {
                    ResponsePayload::Snapshot(s) => s,
                    other => panic!("unexpected: {other:?}"),
                };
            let text: String = snap
                .rows_data
                .iter()
                .map(|r| r.cells.iter().map(|c| c.text.as_str()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n");
            if text.contains("RESTORED=from_manifest") {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("timed out waiting for restored workspace env in PTY");
    assert!(saw_env);

    send_rpc(
        &mut c2,
        4,
        ClientMessage::CloseWorkspace {
            id: ws_keep.id,
            compose_down: false,
            remove_worktree: false,
            force_remove_worktree: false,
        },
    )
    .await;
    let _ = shutdown2_tx.send(());
    srv2_handle.await.unwrap();

    let final_state = std::fs::read_to_string(&state_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&final_state).unwrap();
    assert_eq!(parsed["workspaces"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn project_manifest_loads_commands_merges_env_reserves_extra_ports_and_tolerates_malformed_toml()
 {
    let dir = TempDir::new().unwrap();
    let sock = dir.path().join("daedalusd.sock");
    let runtime = dir.path().join("runtime");

    let valid_dir = dir.path().join("valid-proj");
    std::fs::create_dir_all(&valid_dir).unwrap();
    std::fs::write(valid_dir.join(".env"), "PORT=16000\n").unwrap();
    std::fs::write(
        valid_dir.join(".daedalus.toml"),
        r#"
[agent]
preset = "codex"
command = ["codex", "--full-auto"]

[service]
command = ["docker", "compose", "up"]

[runner]
command = ["cargo", "test", "--workspace"]

[ports]
extra = [16080]

[env]
MANIFEST_ONLY = "from_toml"
CALLER_OVERRIDES = "from_toml"
"#,
    )
    .unwrap();

    let malformed_dir = dir.path().join("malformed-proj");
    std::fs::create_dir_all(&malformed_dir).unwrap();
    std::fs::write(malformed_dir.join(".daedalus.toml"), "[agent\nbroken =").unwrap();

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

    let mut client = IpcStream::connect(&sock).await.unwrap();
    let ws0 = match send_rpc(
        &mut client,
        1,
        ClientMessage::CreateWorkspace {
            name: "valid-0".to_string(),
            root_dir: valid_dir.clone(),
            env: HashMap::from([("CALLER_OVERRIDES".to_string(), "from_caller".to_string())]),
        },
    )
    .await
    {
        ResponsePayload::WorkspaceCreated(ws) => ws,
        other => panic!("unexpected: {other:?}"),
    };
    let manifest = ws0.manifest.expect("manifest should be returned");
    assert_eq!(manifest.agent_preset.as_deref(), Some("codex"));
    assert_eq!(
        manifest.agent_command,
        Some(vec!["codex".to_string(), "--full-auto".to_string()])
    );
    assert_eq!(
        manifest.service_command,
        Some(vec![
            "docker".to_string(),
            "compose".to_string(),
            "up".to_string()
        ])
    );
    assert_eq!(
        manifest.runner_command,
        Some(vec![
            "cargo".to_string(),
            "test".to_string(),
            "--workspace".to_string()
        ])
    );
    assert_eq!(
        ws0.injected_env.get("MANIFEST_ONLY").map(String::as_str),
        Some("from_toml")
    );
    assert_eq!(
        ws0.injected_env.get("CALLER_OVERRIDES").map(String::as_str),
        Some("from_caller")
    );

    // Block port 16180 (16080 + 100) so slot 1 must bump past 100 to 200 because of extra_ports.
    let blocker = std::net::TcpListener::bind(("127.0.0.1", 16180))
        .expect("port 16180 must be free before blocking");
    let ws1 = match send_rpc(
        &mut client,
        2,
        ClientMessage::CreateWorkspace {
            name: "valid-1".to_string(),
            root_dir: valid_dir,
            env: HashMap::new(),
        },
    )
    .await
    {
        ResponsePayload::WorkspaceCreated(ws) => ws,
        other => panic!("unexpected: {other:?}"),
    };
    drop(blocker);
    assert_eq!(ws1.slot_index, 1);
    assert_eq!(ws1.port_offset, 200);
    assert_eq!(
        ws1.injected_env.get("PORT").map(String::as_str),
        Some("16200")
    );

    // Malformed .daedalus.toml must fall back cleanly to manifest: None.
    let ws_bad = match send_rpc(
        &mut client,
        3,
        ClientMessage::CreateWorkspace {
            name: "malformed".to_string(),
            root_dir: malformed_dir,
            env: HashMap::new(),
        },
    )
    .await
    {
        ResponsePayload::WorkspaceCreated(ws) => ws,
        other => panic!("unexpected: {other:?}"),
    };
    assert_eq!(ws_bad.manifest, None);

    let _ = shutdown_tx.send(());
    srv_handle.await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn fish_osc133_captures_failure_trace_when_fish_installed() {
    let fish_path = match std::process::Command::new("which").arg("fish").output() {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        _ => return, // fish not installed on host; skip gracefully
    };

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

    let mut client = IpcStream::connect(&sock).await.unwrap();
    let ws = match send_rpc(
        &mut client,
        1,
        ClientMessage::CreateWorkspace {
            name: "fish-demo".to_string(),
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
            shell: Some(fish_path),
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
            data: b"sh -c 'echo FISH_FAIL_MARKER; exit 19'\n".to_vec(),
        },
    )
    .await;
    let (_, code, has_trace) = wait_for_command_finished(&mut client, 19).await;
    assert_eq!(code, 19);
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
    assert_eq!(trace.exit_code, 19);
    assert!(trace.output.contains("FISH_FAIL_MARKER"));

    send_rpc(
        &mut client,
        6,
        ClientMessage::CloseWorkspace {
            id: ws.id,
            compose_down: false,
            remove_worktree: false,
            force_remove_worktree: false,
        },
    )
    .await;
    let _ = shutdown_tx.send(());
    srv_handle.await.unwrap();
}

#[tokio::test]
async fn set_agent_state_and_hook_events_broadcast_and_clear_status() {
    let dir = TempDir::new().unwrap();
    let sock = dir.path().join("daedalusd.sock");
    let events_sock = dir.path().join("events.sock");
    let runtime = dir.path().join("runtime");
    let ws_dir = dir.path().join("agent-proj");
    std::fs::create_dir_all(&ws_dir).unwrap();

    let server = DaemonServer::bind_with_events_socket(
        sock.clone(),
        runtime,
        Some(events_sock.clone()),
        test_supervisor(),
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

    let mut client = IpcStream::connect(&sock).await.unwrap();
    let ws = match send_rpc(
        &mut client,
        1,
        ClientMessage::CreateWorkspace {
            name: "agent-ws".to_string(),
            root_dir: ws_dir.clone(),
            env: HashMap::new(),
        },
    )
    .await
    {
        ResponsePayload::WorkspaceCreated(ws) => ws,
        other => panic!("unexpected response: {other:?}"),
    };

    let agent_pane = match send_rpc(
        &mut client,
        2,
        ClientMessage::SpawnPane {
            workspace_id: ws.id,
            role: PaneRole::Agent,
            command: Some(sleep_command()),
            shell: None,
            cols: 80,
            rows: 24,
            env: HashMap::new(),
        },
    )
    .await
    {
        ResponsePayload::PaneSpawned(p) => p,
        other => panic!("unexpected response: {other:?}"),
    };
    assert_eq!(agent_pane.agent_state, Some(AgentState::Idle));

    let runner_pane = match send_rpc(
        &mut client,
        3,
        ClientMessage::SpawnPane {
            workspace_id: ws.id,
            role: PaneRole::Runner,
            command: Some(sleep_command()),
            shell: None,
            cols: 80,
            rows: 24,
            env: HashMap::new(),
        },
    )
    .await
    {
        ResponsePayload::PaneSpawned(p) => p,
        other => panic!("unexpected response: {other:?}"),
    };
    assert_eq!(runner_pane.agent_state, None);

    // Explicitly setting state on a non-Agent pane must fail cleanly.
    let non_agent_err = send_rpc_result(
        &mut client,
        4,
        ClientMessage::SetAgentState {
            pane_id: Some(runner_pane.id),
            workspace_id: None,
            root_dir: None,
            state: AgentState::Thinking,
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(non_agent_err, ProtoError::InvalidRequest(_)));

    let mut sub_client = IpcStream::connect(&sock).await.unwrap();
    send_rpc(
        &mut sub_client,
        10,
        ClientMessage::Subscribe { pane_ids: None },
    )
    .await;

    // 1. SetAgentState RPC targeting workspace_id
    let resp = send_rpc(
        &mut client,
        5,
        ClientMessage::SetAgentState {
            pane_id: None,
            workspace_id: Some(ws.id),
            root_dir: None,
            state: AgentState::WaitingForApproval,
        },
    )
    .await;
    assert_eq!(resp, ResponsePayload::Ok);

    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(ServerMessage::Event(ServerEvent::AgentStateChanged {
                workspace_id,
                pane_id,
                state,
            })) = read_frame::<_, ServerMessage>(&mut sub_client)
                .await
                .unwrap()
                && workspace_id == ws.id
                && pane_id == agent_pane.id
                && state == AgentState::WaitingForApproval
            {
                break;
            }
        }
    })
    .await
    .expect("timed out waiting for AgentStateChanged from RPC");

    // 2. events.sock agent_state + status + clear_status ingestion
    let mut hook_stream = IpcStream::connect(&events_sock).await.unwrap();
    let payload = format!(
        "{}\n{}\n{}\n",
        serde_json::json!({
            "kind": "agent_state",
            "root_dir": ws_dir,
            "state": "thinking"
        }),
        serde_json::json!({
            "kind": "status",
            "root_dir": ws_dir,
            "source": "phrourion",
            "level": "warn",
            "label": "2 dirty"
        }),
        serde_json::json!({
            "kind": "clear_status",
            "workspace_id": ws.id,
            "source": "phrourion"
        }),
    );
    hook_stream.write_all(payload.as_bytes()).await.unwrap();
    hook_stream.shutdown().await.unwrap();
    drop(hook_stream);

    let mut saw_thinking = false;
    let mut saw_status_set = false;
    let mut saw_status_cleared = false;
    tokio::time::timeout(Duration::from_secs(3), async {
        while !(saw_thinking && saw_status_set && saw_status_cleared) {
            match read_frame::<_, ServerMessage>(&mut sub_client)
                .await
                .unwrap()
            {
                Some(ServerMessage::Event(ServerEvent::AgentStateChanged {
                    workspace_id,
                    pane_id,
                    state: AgentState::Thinking,
                })) if workspace_id == ws.id && pane_id == agent_pane.id => {
                    saw_thinking = true;
                }
                Some(ServerMessage::Event(ServerEvent::ExternalStatusUpdated {
                    workspace_id,
                    status,
                })) if workspace_id == ws.id && status.source == "phrourion" => {
                    if status.label == "2 dirty" {
                        saw_status_set = true;
                    } else if status.label.is_empty() {
                        saw_status_cleared = true;
                    }
                }
                Some(ServerMessage::Event(_)) => continue,
                other => panic!("unexpected message: {other:?}"),
            }
        }
    })
    .await
    .expect("timed out waiting for hook events");

    let listed = match send_rpc(&mut client, 6, ClientMessage::ListWorkspaces).await {
        ResponsePayload::Workspaces(list) => list,
        other => panic!("unexpected response: {other:?}"),
    };
    assert_eq!(listed.len(), 1);
    assert!(listed[0].external_statuses.is_empty());
    let listed_agent = listed[0]
        .panes
        .iter()
        .find(|p| p.id == agent_pane.id)
        .unwrap();
    assert_eq!(listed_agent.agent_state, Some(AgentState::Thinking));

    // 3. ClearExternalStatus RPC removes published status and broadcasts ExternalStatusUpdated
    send_rpc(
        &mut client,
        7,
        ClientMessage::PublishExternalStatus {
            workspace_id: Some(ws.id),
            root_dir: None,
            source: "cassandra".to_string(),
            level: StatusLevel::Error,
            label: "rate limited".to_string(),
            detail: None,
        },
    )
    .await;
    let clear_resp = send_rpc(
        &mut client,
        8,
        ClientMessage::ClearExternalStatus {
            workspace_id: None,
            root_dir: Some(ws_dir.clone()),
            source: "cassandra".to_string(),
        },
    )
    .await;
    assert_eq!(clear_resp, ResponsePayload::Ok);

    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(ServerMessage::Event(ServerEvent::ExternalStatusUpdated {
                workspace_id,
                status,
            })) = read_frame::<_, ServerMessage>(&mut sub_client)
                .await
                .unwrap()
                && workspace_id == ws.id
                && status.source == "cassandra"
                && status.label.is_empty()
            {
                break;
            }
        }
    })
    .await
    .expect("timed out waiting for ClearExternalStatus broadcast");

    let listed_after_clear = match send_rpc(&mut client, 9, ClientMessage::ListWorkspaces).await {
        ResponsePayload::Workspaces(list) => list,
        other => panic!("unexpected response: {other:?}"),
    };
    assert!(listed_after_clear[0].external_statuses.is_empty());

    let _ = shutdown_tx.send(());
    srv_handle.await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn get_failure_trace_includes_git_branch_and_diff_stat_for_dirty_repo() {
    let dir = TempDir::new().unwrap();
    let sock = dir.path().join("daedalusd.sock");
    let runtime = dir.path().join("runtime");
    let repo_dir = dir.path().join("dirty-repo");
    std::fs::create_dir_all(&repo_dir).unwrap();

    run_git(&repo_dir, &["init", "-b", "main"]);
    std::fs::write(repo_dir.join("README.md"), "# initial\n").unwrap();
    run_git(&repo_dir, &["add", "README.md"]);
    run_git(
        &repo_dir,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-m",
            "init",
        ],
    );
    // Make repo dirty with both modified tracked file and untracked file
    std::fs::write(repo_dir.join("README.md"), "# initial\nmodified line\n").unwrap();
    std::fs::write(repo_dir.join("notes.txt"), "untracked\n").unwrap();

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

    let mut client = IpcStream::connect(&sock).await.unwrap();
    let ws = match send_rpc(
        &mut client,
        1,
        ClientMessage::CreateWorkspace {
            name: "dirty-repo".to_string(),
            root_dir: repo_dir.clone(),
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
            data: b"sh -c 'echo FAIL_GIT_CTX; exit 23'\n".to_vec(),
        },
    )
    .await;
    let (_, code, has_trace) = wait_for_command_finished(&mut client, 23).await;
    assert_eq!(code, 23);
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
    assert_eq!(trace.exit_code, 23);
    assert_eq!(trace.git_branch.as_deref(), Some("main"));
    let diff_stat = trace
        .git_diff_stat
        .expect("dirty repo failure trace should include git_diff_stat");
    assert!(diff_stat.contains("README.md"));
    assert!(diff_stat.contains("notes.txt"));

    let _ = shutdown_tx.send(());
    srv_handle.await.unwrap();
}

#[tokio::test]
async fn close_workspace_removes_linked_worktree_and_refuses_primary_checkout() {
    let dir = TempDir::new().unwrap();
    let sock = dir.path().join("daedalusd.sock");
    let runtime = dir.path().join("runtime");
    let worktrees_dir = dir.path().join("worktrees");
    let repo_dir = dir.path().join("repo");
    std::fs::create_dir_all(&repo_dir).unwrap();

    run_git(&repo_dir, &["init", "-b", "main"]);
    std::fs::write(repo_dir.join("README.md"), "# repo\n").unwrap();
    run_git(&repo_dir, &["add", "README.md"]);
    run_git(
        &repo_dir,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-m",
            "init",
        ],
    );

    let state_path = dir.path().join("state.json");
    let start = async || {
        let server = DaemonServer::bind_with_options(
            sock.clone(),
            runtime.clone(),
            None,
            Some(state_path.clone()),
            test_supervisor(),
        )
        .await
        .unwrap();
        server.set_worktrees_dir(worktrees_dir.clone());
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let srv_handle = tokio::spawn(async move {
            server
                .serve(async {
                    let _ = shutdown_rx.await;
                })
                .await
                .unwrap();
        });
        let client = IpcStream::connect(&sock).await.unwrap();
        (shutdown_tx, srv_handle, client)
    };
    let (shutdown_tx, srv_handle, mut client) = start().await;

    let primary_ws = match send_rpc(
        &mut client,
        1,
        ClientMessage::CreateWorkspace {
            name: "primary".to_string(),
            root_dir: repo_dir.clone(),
            env: HashMap::new(),
        },
    )
    .await
    {
        ResponsePayload::WorkspaceCreated(ws) => ws,
        other => panic!("unexpected: {other:?}"),
    };

    let wt_ws = match send_rpc(
        &mut client,
        2,
        ClientMessage::CreateWorktreeWorkspace {
            repo_dir: repo_dir.clone(),
            branch: "feat/cleanup".to_string(),
            create_branch: true,
            env: HashMap::new(),
        },
    )
    .await
    {
        ResponsePayload::WorkspaceCreated(ws) => ws,
        other => panic!("unexpected: {other:?}"),
    };
    assert!(wt_ws.root_dir.is_dir());
    assert!(
        wt_ws
            .git_status
            .expect("worktree git_status should be populated")
            .is_worktree
    );

    // Refusing to remove primary repo checkout must keep workspace and directory intact
    let primary_err = send_rpc_result(
        &mut client,
        3,
        ClientMessage::CloseWorkspace {
            id: primary_ws.id,
            compose_down: false,
            remove_worktree: true,
            force_remove_worktree: true,
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(primary_err, ProtoError::InvalidRequest(_)));
    assert!(repo_dir.is_dir());

    let target = wt_ws.root_dir.to_str().unwrap();
    let ResponsePayload::PaneSpawned(pane) = send_rpc(
        &mut client,
        40,
        ClientMessage::SpawnPane {
            workspace_id: wt_ws.id,
            role: PaneRole::Runner,
            command: Some(sleep_command()),
            shell: None,
            cols: 80,
            rows: 24,
            env: HashMap::new(),
        },
    )
    .await
    else {
        panic!("expected pane");
    };
    run_git(&repo_dir, &["worktree", "lock", target]);
    let locked = send_rpc_result(
        &mut client,
        41,
        ClientMessage::CloseWorkspace {
            id: wt_ws.id,
            compose_down: false,
            remove_worktree: true,
            force_remove_worktree: true,
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(locked, ProtoError::InvalidRequest(msg) if msg.contains("locked")));
    assert!(matches!(
        send_rpc(
            &mut client,
            42,
            ClientMessage::GetSnapshot { pane_id: pane.id }
        )
        .await,
        ResponsePayload::Snapshot(_)
    ));
    run_git(&repo_dir, &["worktree", "unlock", target]);

    // Dirty linked worktree without force must fail and preserve the workspace
    std::fs::write(wt_ws.root_dir.join("dirty.txt"), "unsaved\n").unwrap();
    let dirty_err = send_rpc_result(
        &mut client,
        4,
        ClientMessage::CloseWorkspace {
            id: wt_ws.id,
            compose_down: false,
            remove_worktree: true,
            force_remove_worktree: false,
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(dirty_err, ProtoError::InvalidRequest(_)));
    assert!(wt_ws.root_dir.is_dir());

    // A clean initialized submodule passes the dirty/locked checks, but Git requires
    // --force for removal. The failed close must keep a retryable workspace record.
    std::fs::remove_file(wt_ws.root_dir.join("dirty.txt")).unwrap();
    run_git(
        &wt_ws.root_dir,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            repo_dir.to_str().unwrap(),
            "nested",
        ],
    );
    run_git(
        &wt_ws.root_dir,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-m",
            "add submodule",
        ],
    );
    let late = send_rpc_result(
        &mut client,
        43,
        ClientMessage::CloseWorkspace {
            id: wt_ws.id,
            compose_down: false,
            remove_worktree: true,
            force_remove_worktree: false,
        },
    )
    .await
    .unwrap_err();
    assert!(
        matches!(late, ProtoError::Internal(msg) if msg.contains("git worktree remove failed"))
    );
    let ResponsePayload::Workspaces(after_failure) =
        send_rpc(&mut client, 44, ClientMessage::ListWorkspaces).await
    else {
        panic!("expected workspace list");
    };
    let retained = after_failure.iter().find(|ws| ws.id == wt_ws.id).unwrap();
    assert!(retained.panes.is_empty());
    assert!(retained.needs_default_panes());
    assert_eq!(retained.slot_index, wt_ws.slot_index);
    for (id, role) in [(45, PaneRole::Agent), (46, PaneRole::Runner)] {
        let resumed = send_rpc(
            &mut client,
            id,
            ClientMessage::EnsureDefaultPane {
                workspace_id: wt_ws.id,
                role,
                command: Some(sleep_command()),
                cols: 80,
                rows: 24,
            },
        )
        .await;
        if role == PaneRole::Runner {
            assert!(matches!(resumed, ResponsePayload::PaneSpawned(_)));
        } else {
            assert!(matches!(
                resumed,
                ResponsePayload::DefaultPaneExisting(None)
            ));
        }
    }

    drop(client);
    shutdown_tx.send(()).unwrap();
    srv_handle.await.unwrap();
    let (shutdown_tx, srv_handle, mut client) = start().await;

    // With force = true, linked worktree is removed cleanly
    let ok = send_rpc(
        &mut client,
        5,
        ClientMessage::CloseWorkspace {
            id: wt_ws.id,
            compose_down: false,
            remove_worktree: true,
            force_remove_worktree: true,
        },
    )
    .await;
    assert_eq!(ok, ResponsePayload::Ok);
    assert!(!wt_ws.root_dir.exists());

    let remaining_wts = match send_rpc(
        &mut client,
        6,
        ClientMessage::DiscoverWorktrees {
            repo_dir: repo_dir.clone(),
        },
    )
    .await
    {
        ResponsePayload::Worktrees(wts) => wts,
        other => panic!("unexpected: {other:?}"),
    };
    assert_eq!(remaining_wts.len(), 1);
    assert!(remaining_wts[0].is_primary);

    // Clean close of primary workspace without remove_worktree succeeds
    send_rpc(
        &mut client,
        7,
        ClientMessage::CloseWorkspace {
            id: primary_ws.id,
            compose_down: false,
            remove_worktree: false,
            force_remove_worktree: false,
        },
    )
    .await;
    assert!(repo_dir.is_dir());

    let _ = shutdown_tx.send(());
    srv_handle.await.unwrap();
}

#[tokio::test]
async fn tcp_wsl2_bridge_rpc_round_trip() {
    let dir = TempDir::new().unwrap();
    let runtime = dir.path().join("runtime");
    let ws_dir = dir.path().join("bridge-ws");
    std::fs::create_dir_all(&ws_dir).unwrap();

    let supervisor = test_supervisor();
    let expected_mode = supervisor.isolation_mode().as_str().to_string();

    let server =
        DaemonServer::bind_with_supervisor(PathBuf::from("tcp://127.0.0.1:0"), runtime, supervisor)
            .await
            .unwrap();
    let endpoint = server.socket_path();
    let endpoint_str = endpoint.to_string_lossy();
    assert!(
        endpoint_str.starts_with("tcp://127.0.0.1:"),
        "expected resolved tcp://127.0.0.1:<port>, got {endpoint_str}"
    );
    assert_ne!(endpoint_str, "tcp://127.0.0.1:0");

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let srv_handle = tokio::spawn(async move {
        let _ = server
            .serve(async {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    let mut client = IpcStream::connect(&endpoint).await.unwrap();
    match send_rpc(&mut client, 1, ClientMessage::Ping).await {
        ResponsePayload::Pong {
            version,
            vt_backend,
            isolation_mode,
        } => {
            assert_eq!(version, env!("CARGO_PKG_VERSION"));
            assert!(!vt_backend.is_empty());
            assert_eq!(isolation_mode, expected_mode);
        }
        other => panic!("expected Pong, got {other:?}"),
    }

    let created = match send_rpc(
        &mut client,
        2,
        ClientMessage::CreateWorkspace {
            name: "wsl2-bridge".to_string(),
            root_dir: ws_dir,
            env: HashMap::new(),
        },
    )
    .await
    {
        ResponsePayload::WorkspaceCreated(ws) => ws,
        other => panic!("expected WorkspaceCreated, got {other:?}"),
    };
    assert_eq!(created.name, "wsl2-bridge");

    let listed = match send_rpc(&mut client, 3, ClientMessage::ListWorkspaces).await {
        ResponsePayload::Workspaces(list) => list,
        other => panic!("expected Workspaces, got {other:?}"),
    };
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, created.id);

    let ok = send_rpc(
        &mut client,
        4,
        ClientMessage::CloseWorkspace {
            id: created.id,
            compose_down: false,
            remove_worktree: false,
            force_remove_worktree: false,
        },
    )
    .await;
    assert_eq!(ok, ResponsePayload::Ok);

    let _ = shutdown_tx.send(());
    srv_handle.await.unwrap();
}

#[tokio::test]
async fn auth_token_rejects_unauthenticated_and_allows_authenticated_clients() {
    let dir = TempDir::new().unwrap();
    let runtime = dir.path().join("runtime");

    let server = DaemonServer::bind_with_supervisor(
        PathBuf::from("tcp://127.0.0.1:0"),
        runtime,
        test_supervisor(),
    )
    .await
    .unwrap()
    .with_auth_token(Some("super-secret-token".to_string()));
    let endpoint = server.socket_path();

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let srv_handle = tokio::spawn(async move {
        let _ = server
            .serve(async {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    let mut c1 = IpcStream::connect(&endpoint).await.unwrap();
    assert!(matches!(
        send_rpc_result(&mut c1, 1, ClientMessage::Ping).await,
        Err(ProtoError::Unauthorized(_))
    ));
    assert!(matches!(
        send_rpc_result(&mut c1, 2, ClientMessage::ListWorkspaces).await,
        Err(ProtoError::Unauthorized(_))
    ));
    assert!(matches!(
        send_rpc_result(
            &mut c1,
            3,
            ClientMessage::Authenticate {
                token: "wrong-token".to_string(),
            },
        )
        .await,
        Err(ProtoError::Unauthorized(_))
    ));
    assert!(matches!(
        send_rpc_result(&mut c1, 4, ClientMessage::Ping).await,
        Err(ProtoError::Unauthorized(_))
    ));

    let auth_ok = send_rpc(
        &mut c1,
        5,
        ClientMessage::Authenticate {
            token: "super-secret-token".to_string(),
        },
    )
    .await;
    assert_eq!(auth_ok, ResponsePayload::Ok);

    assert!(matches!(
        send_rpc(&mut c1, 6, ClientMessage::Ping).await,
        ResponsePayload::Pong { .. }
    ));
    assert!(matches!(
        send_rpc(&mut c1, 7, ClientMessage::ListWorkspaces).await,
        ResponsePayload::Workspaces(_)
    ));

    // Auth state is per-connection: a second client must also authenticate.
    let mut c2 = IpcStream::connect(&endpoint).await.unwrap();
    assert!(matches!(
        send_rpc_result(&mut c2, 1, ClientMessage::Ping).await,
        Err(ProtoError::Unauthorized(_))
    ));
    assert_eq!(
        send_rpc(
            &mut c2,
            2,
            ClientMessage::Authenticate {
                token: "super-secret-token".to_string(),
            },
        )
        .await,
        ResponsePayload::Ok
    );
    assert!(matches!(
        send_rpc(&mut c2, 3, ClientMessage::Ping).await,
        ResponsePayload::Pong { .. }
    ));

    let _ = shutdown_tx.send(());
    srv_handle.await.unwrap();
}

fn interactive_shell_for_action_test() -> Vec<String> {
    #[cfg(unix)]
    {
        vec!["/bin/sh".to_string()]
    }
    #[cfg(windows)]
    {
        vec!["cmd.exe".to_string(), "/Q".to_string()]
    }
}

#[tokio::test]
async fn run_workspace_action_sends_command_to_target_pane() {
    let dir = TempDir::new().unwrap();
    let sock = dir.path().join("daedalusd.sock");
    let runtime = dir.path().join("runtime");
    let ws_dir = dir.path().join("ws");
    std::fs::create_dir_all(&ws_dir).unwrap();
    std::fs::write(
        ws_dir.join(".daedalus.toml"),
        r#"
[[actions]]
id = "runner-echo"
label = "Runner Echo"
target = "runner"
command = "echo RUNNER_ACTION_OK"

[[actions]]
id = "agent-prompt"
label = "Agent Prompt"
target = "agent"
command = "AGENT_ACTION_OK"

[[actions]]
id = "service-cmd"
label = "Service Cmd"
target = "service"
command = "echo SVC_ACTION_OK"
"#,
    )
    .unwrap();

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

    let mut client = IpcStream::connect(&sock).await.unwrap();
    let ws = match send_rpc(
        &mut client,
        1,
        ClientMessage::CreateWorkspace {
            name: "actions-ws".to_string(),
            root_dir: ws_dir,
            env: HashMap::new(),
        },
    )
    .await
    {
        ResponsePayload::WorkspaceCreated(ws) => ws,
        other => panic!("unexpected response: {other:?}"),
    };
    let manifest = ws.manifest.expect("manifest should be loaded");
    assert_eq!(manifest.actions.len(), 3);

    // Unknown action returns InvalidRequest
    let unknown_err = send_rpc_result(
        &mut client,
        2,
        ClientMessage::RunWorkspaceAction {
            workspace_id: ws.id,
            action_id: "missing-action".to_string(),
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(unknown_err, ProtoError::InvalidRequest(_)));

    // Missing target pane returns InvalidRequest
    let missing_pane_err = send_rpc_result(
        &mut client,
        3,
        ClientMessage::RunWorkspaceAction {
            workspace_id: ws.id,
            action_id: "service-cmd".to_string(),
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(missing_pane_err, ProtoError::InvalidRequest(_)));

    let runner_pane = match send_rpc(
        &mut client,
        4,
        ClientMessage::SpawnPane {
            workspace_id: ws.id,
            role: PaneRole::Runner,
            command: Some(interactive_shell_for_action_test()),
            shell: None,
            cols: 120,
            rows: 20,
            env: HashMap::new(),
        },
    )
    .await
    {
        ResponsePayload::PaneSpawned(p) => p,
        other => panic!("unexpected response: {other:?}"),
    };

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        send_rpc(
            &mut client,
            5,
            ClientMessage::RunWorkspaceAction {
                workspace_id: ws.id,
                action_id: "runner-echo".to_string(),
            },
        )
        .await,
        ResponsePayload::Ok
    );

    let runner_text = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snap = match send_rpc(
                &mut client,
                6,
                ClientMessage::GetSnapshot {
                    pane_id: runner_pane.id,
                },
            )
            .await
            {
                ResponsePayload::Snapshot(s) => s,
                other => panic!("unexpected response: {other:?}"),
            };
            let text: String = snap
                .rows_data
                .iter()
                .map(|r| {
                    r.cells
                        .iter()
                        .map(|c| c.text.as_str())
                        .collect::<String>()
                        .trim_end()
                        .to_string()
                })
                .collect::<Vec<_>>()
                .join("\n");
            if text.contains("RUNNER_ACTION_OK") {
                return text;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("timed out waiting for RUNNER_ACTION_OK in runner pane snapshot");
    assert!(runner_text.contains("RUNNER_ACTION_OK"));

    #[cfg(unix)]
    {
        let agent_pane = match send_rpc(
            &mut client,
            7,
            ClientMessage::SpawnPane {
                workspace_id: ws.id,
                role: PaneRole::Agent,
                command: Some(vec![
                    "/bin/sh".to_string(),
                    "-c".to_string(),
                    "stty -echo; cat -v".to_string(),
                ]),
                shell: None,
                cols: 120,
                rows: 20,
                env: HashMap::new(),
            },
        )
        .await
        {
            ResponsePayload::PaneSpawned(p) => p,
            other => panic!("unexpected response: {other:?}"),
        };

        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            send_rpc(
                &mut client,
                8,
                ClientMessage::RunWorkspaceAction {
                    workspace_id: ws.id,
                    action_id: "agent-prompt".to_string(),
                },
            )
            .await,
            ResponsePayload::Ok
        );

        let agent_text = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let snap = match send_rpc(
                    &mut client,
                    9,
                    ClientMessage::GetSnapshot {
                        pane_id: agent_pane.id,
                    },
                )
                .await
                {
                    ResponsePayload::Snapshot(s) => s,
                    other => panic!("unexpected response: {other:?}"),
                };
                let text: String = snap
                    .rows_data
                    .iter()
                    .map(|r| {
                        r.cells
                            .iter()
                            .map(|c| c.text.as_str())
                            .collect::<String>()
                            .trim_end()
                            .to_string()
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if text.contains("^[[200~AGENT_ACTION_OK^[[201~") {
                    return text;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("timed out waiting for bracketed paste AGENT_ACTION_OK in agent pane snapshot");
        assert!(agent_text.contains("^[[200~AGENT_ACTION_OK^[[201~"));
    }

    send_rpc(
        &mut client,
        10,
        ClientMessage::CloseWorkspace {
            id: ws.id,
            compose_down: false,
            remove_worktree: false,
            force_remove_worktree: false,
        },
    )
    .await;
    let _ = shutdown_tx.send(());
    srv_handle.await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn daemon_uses_env_token_without_passing_it_to_panes() {
    let dir = TempDir::new().unwrap();
    let socket = dir.path().join("control.sock");
    let mut daemon = tokio::process::Command::new(env!("CARGO_BIN_EXE_daedalusd"))
        .arg("--socket")
        .arg(&socket)
        .arg("--events-socket")
        .arg(dir.path().join("events.sock"))
        .arg("--runtime-dir")
        .arg(dir.path().join("runtime"))
        .arg("--state-file")
        .arg(dir.path().join("state.json"))
        .env("DAEDALUS_AUTH_TOKEN", "test-daemon-token")
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut client = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(stream) = IpcStream::connect(&socket).await {
                break stream;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("daemon startup timed out");
    assert!(matches!(
        send_rpc_result(&mut client, 1, ClientMessage::Ping).await,
        Err(ProtoError::Unauthorized(_))
    ));
    assert_eq!(
        send_rpc(
            &mut client,
            2,
            ClientMessage::Authenticate {
                token: "test-daemon-token".into()
            }
        )
        .await,
        ResponsePayload::Ok
    );
    let ResponsePayload::WorkspaceCreated(ws) = send_rpc(
        &mut client,
        3,
        ClientMessage::CreateWorkspace {
            name: "credential-test".into(),
            root_dir: dir.path().into(),
            env: HashMap::new(),
        },
    )
    .await
    else {
        panic!("expected workspace");
    };
    let marker = dir.path().join("result");
    let command = vec![
        "/bin/sh".into(),
        "-c".into(),
        r#"if test "${DAEDALUS_AUTH_TOKEN+x}" = x; then echo present; else echo absent; fi > "$1""#
            .into(),
        "sh".into(),
        marker.to_str().unwrap().into(),
    ];
    send_rpc(
        &mut client,
        4,
        ClientMessage::SpawnPane {
            workspace_id: ws.id,
            role: PaneRole::Runner,
            command: Some(command),
            shell: None,
            cols: 80,
            rows: 24,
            env: HashMap::new(),
        },
    )
    .await;
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(text) = std::fs::read_to_string(&marker)
                && !text.is_empty()
            {
                break text;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("pane did not write marker");
    daemon.kill().await.unwrap();
    assert_eq!(result.trim(), "absent");
}

#[tokio::test]
async fn relative_workspace_root_survives_restart_from_a_different_directory() {
    let dir = TempDir::new().unwrap();
    let mut original = None;
    for (index, preset) in ["codex", "claude"].iter().enumerate() {
        let cwd = dir.path().join(preset);
        std::fs::create_dir(&cwd).unwrap();
        std::fs::write(
            cwd.join(".daedalus.toml"),
            format!("[agent]\npreset = '{preset}'\n"),
        )
        .unwrap();
        let socket = dir.path().join(format!("daemon-{index}.sock"));
        let mut daemon = tokio::process::Command::new(env!("CARGO_BIN_EXE_daedalusd"))
            .current_dir(&cwd)
            .arg("--socket")
            .arg(&socket)
            .arg("--runtime-dir")
            .arg(dir.path().join("runtime"))
            .arg("--events-socket")
            .arg(dir.path().join(format!("events-{index}.sock")))
            .arg("--state-file")
            .arg(dir.path().join("state.json"))
            .env_remove("DAEDALUS_AUTH_TOKEN")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut client = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Ok(stream) = IpcStream::connect(&socket).await {
                    break stream;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("daemon startup timed out");
        if index == 0 {
            let ResponsePayload::WorkspaceCreated(ws) = send_rpc(
                &mut client,
                1,
                ClientMessage::CreateWorkspace {
                    name: "relative".into(),
                    root_dir: ".".into(),
                    env: HashMap::new(),
                },
            )
            .await
            else {
                panic!("expected created workspace")
            };
            assert!(ws.root_dir.is_absolute());
            original = Some(ws.id);
        } else {
            let ResponsePayload::Workspaces(list) =
                send_rpc(&mut client, 1, ClientMessage::ListWorkspaces).await
            else {
                panic!("expected restored workspace");
            };
            assert_eq!(list.len(), 1);
            assert_eq!(Some(list[0].id), original);
            assert_eq!(
                list[0].manifest.as_ref().unwrap().agent_preset.as_deref(),
                Some("codex")
            );
        }
        drop(client);
        daemon.kill().await.unwrap();
    }
}
