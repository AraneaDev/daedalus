#[cfg(unix)]
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use daedalus_proto::transport::IpcStream;
use daedalus_proto::{
    AgentState, ClientMessage, PaneRole, Request, ResponsePayload, ServerMessage, read_frame,
    write_frame,
};
#[cfg(unix)]
use daedalus_proto::{GridSnapshot, PaneId, ServerEvent};
use daedalus_ui::bridge::{BridgeConfig, DaemonBridge, UiCommand, UiEvent, UiEventSink};
use daedalus_ui::state::AgentPreset;
use daedalusd::server::DaemonServer;
use daedalusd::supervisor::{IsolationMode, Supervisor};
use tempfile::TempDir;
use tokio::sync::{mpsc, oneshot};

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

struct ChannelSink {
    tx: mpsc::UnboundedSender<UiEvent>,
}

impl UiEventSink for ChannelSink {
    fn emit(&self, event: UiEvent) {
        let _ = self.tx.send(event);
    }
}

#[cfg(unix)]
fn snapshot_text(snapshot: &GridSnapshot) -> String {
    snapshot
        .rows_data
        .iter()
        .map(|row| {
            row.cells
                .iter()
                .map(|cell| cell.text.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(unix)]
#[tokio::test]
async fn daemon_bridge_bootstraps_three_panes_and_forwards_runner_failure_to_agent() {
    let dir = TempDir::new().unwrap();
    let sock = dir.path().join("daedalusd.sock");
    let runtime = dir.path().join("runtime");

    let server = DaemonServer::bind_with_supervisor(sock.clone(), runtime, test_supervisor())
        .await
        .unwrap();
    server.set_worktrees_dir(dir.path().join("worktrees"));
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let srv_handle = tokio::spawn(async move {
        let _ = server
            .serve(async {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    let (evt_tx, mut evt_rx) = mpsc::unbounded_channel();
    let sink: Arc<dyn UiEventSink> = Arc::new(ChannelSink { tx: evt_tx });
    let bridge = DaemonBridge::start(
        BridgeConfig {
            bootstrap_explicit_dir: true,
            socket_path: sock,
            initial_dir: dir.path().to_path_buf(),
            default_agent: AgentPreset::Shell,
            auth_token: None,
            auto_spawn_daemon: false,
        },
        sink,
    );

    // 1. Wait for Connected, WorkspaceCreated, and 3 PaneSpawned events (Agent, Service, Runner).
    let mut connected = false;
    let mut created_workspace_id = None;
    let mut panes_by_role: HashMap<PaneRole, PaneId> = HashMap::new();

    tokio::time::timeout(Duration::from_secs(10), async {
        while !connected || created_workspace_id.is_none() || panes_by_role.len() < 3 {
            let evt = evt_rx.recv().await.expect("event channel closed");
            match evt {
                UiEvent::Connected => connected = true,
                UiEvent::WorkspaceCreated(ws) => {
                    assert!(
                        created_workspace_id.is_none(),
                        "expected exactly 1 auto-bootstrapped workspace"
                    );
                    created_workspace_id = Some(ws.id);
                }
                UiEvent::PaneSpawned {
                    summary,
                    agent_preset,
                } => {
                    if summary.role == PaneRole::Agent {
                        assert_eq!(agent_preset, Some(AgentPreset::Shell));
                    } else {
                        assert_eq!(agent_preset, None);
                    }
                    panes_by_role.insert(summary.role, summary.id);
                }
                _ => {}
            }
        }
    })
    .await
    .expect("timed out waiting for 3-pane auto-bootstrap");

    let agent_pane_id = *panes_by_role.get(&PaneRole::Agent).expect("Agent pane");
    let _service_pane_id = *panes_by_role.get(&PaneRole::Service).expect("Service pane");
    let runner_pane_id = *panes_by_role.get(&PaneRole::Runner).expect("Runner pane");

    assert!(bridge.send(UiCommand::AddAgent {
        workspace_id: created_workspace_id.unwrap(),
        preset: AgentPreset::Shell,
        cols: 80,
        rows: 24,
    }));
    let additional_agent = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let UiEvent::PaneSpawned {
                summary,
                agent_preset,
            } = evt_rx.recv().await.unwrap()
            {
                assert_eq!(summary.role, PaneRole::Agent);
                assert_eq!(agent_preset, Some(AgentPreset::Shell));
                break summary.id;
            }
        }
    })
    .await
    .expect("additional agent should start");
    assert_ne!(agent_pane_id, additional_agent);
    assert!(bridge.send(UiCommand::ClosePane(additional_agent)));
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let UiEvent::PaneClosed { pane_id } = evt_rx.recv().await.unwrap()
                && pane_id == additional_agent
            {
                break;
            }
        }
    })
    .await
    .expect("daemon should acknowledge closing just the additional agent");

    // Allow interactive shells to finish emitting their initial prompt / OSC 133 hooks.
    tokio::time::sleep(Duration::from_millis(400)).await;

    // 2. Run a failing command in the Runner pane and wait for CommandFinished with exit 19.
    bridge.send(UiCommand::SendInput {
        pane_id: runner_pane_id,
        data: b"sh -c 'echo UI_BRIDGE_FAIL; exit 19'\n".to_vec(),
    });

    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let evt = evt_rx.recv().await.expect("event channel closed");
            if let UiEvent::ServerEvent(ServerEvent::CommandFinished {
                pane_id,
                exit_code,
                has_failure_trace,
                ..
            }) = evt
                && pane_id == runner_pane_id
                && exit_code == 19
            {
                assert!(
                    has_failure_trace,
                    "expected Runner failure trace to be captured for exit 19"
                );
                break;
            }
        }
    })
    .await
    .expect("timed out waiting for Runner CommandFinished(exit_code=19)");

    // 3. Forward the Runner failure trace to the Agent pane and verify confirmation toast + output.
    bridge.send(UiCommand::ForwardRunnerFailureToAgent {
        runner_pane_id,
        agent_pane_id,
    });

    let mut saw_toast = false;
    tokio::time::timeout(Duration::from_secs(10), async {
        while !saw_toast {
            let evt = evt_rx.recv().await.expect("event channel closed");
            if let UiEvent::Toast { message, is_error } = evt
                && message.contains("exit 19")
            {
                assert!(!is_error, "expected non-error confirmation toast");
                saw_toast = true;
            }
        }
    })
    .await
    .expect("timed out waiting for failure-forward confirmation toast");

    // Poll Agent pane snapshot until the pasted failure trace text is visible on screen.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut found_in_agent = false;
    while tokio::time::Instant::now() < deadline && !found_in_agent {
        bridge.send(UiCommand::RequestSnapshot {
            pane_id: agent_pane_id,
        });
        let wait_until = tokio::time::Instant::now() + Duration::from_millis(250);
        while tokio::time::Instant::now() < wait_until {
            let remaining = wait_until.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, evt_rx.recv()).await {
                Ok(Some(UiEvent::Snapshot(snap))) if snap.pane_id == agent_pane_id => {
                    if snapshot_text(&snap).contains("UI_BRIDGE_FAIL") {
                        found_in_agent = true;
                        break;
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) => panic!("event channel closed unexpectedly"),
                Err(_) => break,
            }
        }
    }
    assert!(
        found_in_agent,
        "expected Agent pane snapshot to contain forwarded 'UI_BRIDGE_FAIL' trace"
    );

    // 4. Cleanly shut down bridge and daemon server.
    tokio::task::spawn_blocking(move || bridge.shutdown())
        .await
        .expect("bridge shutdown task panicked");

    let _ = shutdown_tx.send(());
    tokio::time::timeout(Duration::from_secs(5), srv_handle)
        .await
        .expect("timed out waiting for DaemonServer shutdown")
        .expect("DaemonServer task panicked");
}

#[tokio::test]
async fn daemon_bridge_handles_mid_session_disconnect_and_reconnects_cleanly() {
    let dir = TempDir::new().unwrap();
    let sock = dir.path().join("daedalusd.sock");
    let runtime = dir.path().join("runtime");

    // Run server1 inside its own Tokio runtime so shutting it down drops all spawned
    // `handle_client` tasks and closes the UDS socket with EOF, matching a real daemon restart.
    let (ready1_tx, ready1_rx) = oneshot::channel::<()>();
    let (shutdown1_tx, shutdown1_rx) = oneshot::channel::<()>();
    let sock1 = sock.clone();
    let runtime1 = runtime.clone();
    let state_file = dir.path().join("state.json");
    let state_file1 = state_file.clone();
    let srv1_thread = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let server1 = DaemonServer::bind_with_options(
                sock1,
                runtime1,
                None,
                Some(state_file1),
                test_supervisor(),
            )
            .await
            .unwrap();
            let _ = ready1_tx.send(());
            let _ = server1
                .serve(async {
                    let _ = shutdown1_rx.await;
                })
                .await;
        });
        // Dropping `rt` cancels any lingering `handle_client` tasks and closes the client socket.
        rt.shutdown_timeout(Duration::from_millis(200));
    });
    ready1_rx.await.expect("server1 failed to bind");

    let (evt_tx, mut evt_rx) = mpsc::unbounded_channel();
    let sink: Arc<dyn UiEventSink> = Arc::new(ChannelSink { tx: evt_tx });
    let bridge = DaemonBridge::start(
        BridgeConfig {
            bootstrap_explicit_dir: true,
            socket_path: sock.clone(),
            initial_dir: dir.path().to_path_buf(),
            default_agent: AgentPreset::Shell,
            auth_token: None,
            auto_spawn_daemon: false,
        },
        sink,
    );

    // Wait for initial bootstrap to finish completely (3 panes spawned + 3 initial snapshots).
    let mut initial_workspace = None;
    let mut initial_panes = 0usize;
    let mut initial_snapshots = 0usize;
    tokio::time::timeout(Duration::from_secs(10), async {
        while initial_panes < 3 || initial_snapshots < 3 {
            match evt_rx.recv().await.expect("event channel closed") {
                UiEvent::WorkspaceCreated(ws) => initial_workspace = Some(ws.id),
                UiEvent::PaneSpawned { .. } => initial_panes += 1,
                UiEvent::Snapshot(_) => initial_snapshots += 1,
                _ => {}
            }
        }
    })
    .await
    .expect("timed out waiting for initial bootstrap");

    // Shut down daemon 1 mid-session and verify DaemonBridge emits Disconnected without hanging.
    let _ = shutdown1_tx.send(());
    tokio::task::spawn_blocking(move || srv1_thread.join().unwrap())
        .await
        .expect("server1 thread panicked");

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(
                evt_rx.recv().await.expect("event channel closed"),
                UiEvent::Disconnected
            ) {
                break;
            }
        }
    })
    .await
    .expect("timed out waiting for UiEvent::Disconnected");

    let original_id = initial_workspace.expect("bootstrap should create a workspace");
    let create_workspace = |name: &str| UiCommand::CreateWorkspace {
        name: name.into(),
        root_dir: dir.path().to_path_buf(),
        agent_preset: AgentPreset::Shell,
    };
    assert!(!bridge.send(create_workspace("rejected")));
    assert!(!bridge.send(UiCommand::CloseWorkspace {
        id: original_id,
        compose_down: false,
        remove_worktree: false,
        force_remove_worktree: false,
    }));
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut rejected = 0;
        while rejected < 2 {
            if let UiEvent::Toast {
                message,
                is_error: true,
            } = evt_rx.recv().await.unwrap()
                && message.contains("unavailable")
            {
                rejected += 1;
            }
        }
    })
    .await
    .expect("both rejected commands should have visible feedback");

    // Start daemon 2 on the same socket and verify DaemonBridge reconnects and reloads workspaces.
    let server2 = DaemonServer::bind_with_options(
        sock.clone(),
        runtime,
        None,
        Some(state_file),
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

    let mut reconnected = false;
    let mut reloaded_workspaces = false;
    tokio::time::timeout(Duration::from_secs(10), async {
        while !reconnected || !reloaded_workspaces {
            match evt_rx.recv().await.expect("event channel closed") {
                UiEvent::Connected => reconnected = true,
                UiEvent::WorkspacesLoaded(_) => reloaded_workspaces = true,
                _ => {}
            }
        }
    })
    .await
    .expect("timed out waiting for bridge reconnect + WorkspacesLoaded");

    assert!(bridge.send(create_workspace("after reconnect")));
    let created_id = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let UiEvent::WorkspaceCreated(ws) = evt_rx.recv().await.unwrap() {
                assert_eq!(ws.name, "after reconnect");
                break ws.id;
            }
        }
    })
    .await
    .unwrap();
    assert!(bridge.send(UiCommand::CloseWorkspace {
        id: created_id,
        compose_down: false,
        remove_worktree: false,
        force_remove_worktree: false,
    }));
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let UiEvent::WorkspacesLoaded(workspaces) = evt_rx.recv().await.unwrap() {
                assert!(workspaces.iter().any(|ws| ws.id == original_id));
                assert!(
                    workspaces
                        .iter()
                        .all(|ws| ws.id != created_id && ws.name != "rejected")
                );
                break;
            }
        }
    })
    .await
    .unwrap();

    tokio::task::spawn_blocking(move || bridge.shutdown())
        .await
        .expect("bridge shutdown task panicked");

    let _ = shutdown2_tx.send(());
    tokio::time::timeout(Duration::from_secs(5), srv2_handle)
        .await
        .expect("timed out waiting for server2 shutdown")
        .expect("server2 panicked");
}

#[tokio::test]
async fn daemon_bridge_activates_dormant_workspace_with_manifest_and_searches_scrollback() {
    let dir = TempDir::new().unwrap();
    let ws_dir = dir.path().join("project");
    std::fs::create_dir_all(&ws_dir).unwrap();
    #[cfg(unix)]
    let manifest_toml = r#"
[runner]
command = ["sh", "-c", "echo MANIFEST_RUNNER_NEEDLE; sleep 5"]
"#;
    #[cfg(windows)]
    let manifest_toml = r#"
[runner]
command = ["cmd.exe", "/C", "echo", "MANIFEST_RUNNER_NEEDLE", "&", "ping", "-n", "5", "127.0.0.1"]
"#;
    std::fs::write(ws_dir.join(".daedalus.toml"), manifest_toml).unwrap();

    let sock = dir.path().join("daedalusd.sock");
    let runtime = dir.path().join("runtime");
    let state_file = dir.path().join("state.json");

    // Seed state.json via DaemonServer 1 creating a workspace and closing cleanly.
    {
        let srv1 = DaemonServer::bind_with_options(
            sock.clone(),
            runtime.clone(),
            None,
            Some(state_file.clone()),
            test_supervisor(),
        )
        .await
        .unwrap();
        let (tx1, rx1) = oneshot::channel::<()>();
        let handle1 = tokio::spawn(async move {
            let _ = srv1
                .serve(async {
                    let _ = rx1.await;
                })
                .await;
        });

        let (evt_tx, mut evt_rx) = mpsc::unbounded_channel();
        let sink: Arc<dyn UiEventSink> = Arc::new(ChannelSink { tx: evt_tx });
        let bridge1 = DaemonBridge::start(
            BridgeConfig {
                bootstrap_explicit_dir: true,
                socket_path: sock.clone(),
                initial_dir: ws_dir.clone(),
                default_agent: AgentPreset::Shell,
                auth_token: None,
                auto_spawn_daemon: false,
            },
            sink,
        );

        tokio::time::timeout(Duration::from_secs(10), async {
            let mut spawned = 0usize;
            while spawned < 3 {
                if matches!(
                    evt_rx.recv().await.expect("event channel closed"),
                    UiEvent::PaneSpawned { .. }
                ) {
                    spawned += 1;
                }
            }
        })
        .await
        .expect("timed out waiting for initial workspace creation");

        tokio::task::spawn_blocking(move || bridge1.shutdown())
            .await
            .unwrap();
        let _ = tx1.send(());
        handle1.await.unwrap();
    }

    // Start DaemonServer 2 which restores the workspace as dormant (`panes: []`).
    let srv2 = DaemonServer::bind_with_options(
        sock.clone(),
        runtime,
        None,
        Some(state_file),
        test_supervisor(),
    )
    .await
    .unwrap();
    let (tx2, rx2) = oneshot::channel::<()>();
    let handle2 = tokio::spawn(async move {
        let _ = srv2
            .serve(async {
                let _ = rx2.await;
            })
            .await;
    });

    let (evt_tx, mut evt_rx) = mpsc::unbounded_channel();
    let sink: Arc<dyn UiEventSink> = Arc::new(ChannelSink { tx: evt_tx });
    let bridge2 = DaemonBridge::start(
        BridgeConfig {
            bootstrap_explicit_dir: true,
            socket_path: sock,
            initial_dir: ws_dir,
            default_agent: AgentPreset::Shell,
            auth_token: None,
            auto_spawn_daemon: false,
        },
        sink,
    );

    let dormant_ws = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let UiEvent::WorkspacesLoaded(mut wss) =
                evt_rx.recv().await.expect("event channel closed")
            {
                assert_eq!(wss.len(), 1);
                let ws = wss.pop().unwrap();
                assert!(ws.panes.is_empty(), "restored workspace must be dormant");
                assert!(
                    ws.manifest.is_some(),
                    "restored workspace must load manifest"
                );
                break ws;
            }
        }
    })
    .await
    .expect("timed out waiting for dormant WorkspacesLoaded");

    bridge2.send(UiCommand::EnsureWorkspacePanes {
        workspace_id: dormant_ws.id,
        default_agent: AgentPreset::Shell,
        manifest: dormant_ws.manifest.clone(),
    });

    let mut ensured = false;
    let mut runner_pane_id = None;
    let mut spawned_count = 0usize;
    tokio::time::timeout(Duration::from_secs(10), async {
        while !ensured || spawned_count < 3 {
            match evt_rx.recv().await.expect("event channel closed") {
                UiEvent::PaneSpawned { summary, .. } => {
                    spawned_count += 1;
                    if summary.role == PaneRole::Runner {
                        runner_pane_id = Some(summary.id);
                    }
                }
                UiEvent::WorkspacePanesEnsured {
                    workspace_id,
                    complete: true,
                } if workspace_id == dormant_ws.id => {
                    ensured = true;
                }
                _ => {}
            }
        }
    })
    .await
    .expect("timed out waiting for EnsureWorkspacePanes");

    let runner_id = runner_pane_id.expect("runner pane must be spawned");

    bridge2.send(UiCommand::RequestScrollback {
        pane_id: runner_id,
        generation: 42,
        offset_from_bottom: 1,
        count: 2,
    });
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let UiEvent::ScrollbackLoaded {
                pane_id,
                generation,
                ..
            } = evt_rx.recv().await.unwrap()
            {
                assert_eq!((pane_id, generation), (runner_id, 42));
                break;
            }
        }
    })
    .await
    .expect("scrollback reply should carry its request generation");

    // Poll SearchPane until the manifest runner command output appears in the VT grid.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut found = false;
    let mut seen_toasts = Vec::new();
    let mut search_responses = 0usize;
    let mut last_snapshot_lines = Vec::new();
    while tokio::time::Instant::now() < deadline && !found {
        bridge2.send(UiCommand::SearchPane {
            pane_id: runner_id,
            query: "manifest_runner_needle".to_string(),
            case_sensitive: false,
            generation: 1,
        });
        let wait_until = tokio::time::Instant::now() + Duration::from_millis(250);
        while tokio::time::Instant::now() < wait_until {
            let remaining = wait_until.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, evt_rx.recv()).await {
                Ok(Some(UiEvent::SearchResults {
                    pane_id, matches, ..
                })) if pane_id == runner_id => {
                    search_responses += 1;
                    if !matches.is_empty() {
                        found = true;
                        break;
                    }
                }
                Ok(Some(UiEvent::Toast { message, .. })) => {
                    seen_toasts.push(message);
                }
                Ok(Some(UiEvent::ServerEvent(daedalus_proto::ServerEvent::GridDelta(delta))))
                    if delta.pane_id == runner_id =>
                {
                    for row in delta.dirty_rows {
                        let line: String = row.cells.into_iter().map(|c| c.text).collect();
                        let trimmed = line.trim_end().to_string();
                        if !trimmed.is_empty() {
                            last_snapshot_lines.push(trimmed);
                        }
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) => panic!("event channel closed"),
                Err(_) => break,
            }
        }
    }
    assert!(
        found,
        "expected SearchPane to locate manifest runner output in restored pane (search_responses={search_responses}, toasts={seen_toasts:?}, lines={last_snapshot_lines:?})"
    );

    tokio::task::spawn_blocking(move || bridge2.shutdown())
        .await
        .unwrap();
    let _ = tx2.send(());
    handle2.await.unwrap();
}

fn init_git_repo(dir: &std::path::Path) {
    let run = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("git command failed");
        assert!(status.success(), "git {args:?} failed with {status}");
    };
    run(&["init"]);
    run(&["config", "user.email", "test@example.com"]);
    run(&["config", "user.name", "Daedalus Test"]);
    std::fs::write(dir.join("README.md"), "# test\n").unwrap();
    run(&["add", "README.md"]);
    run(&["commit", "-m", "initial commit"]);
}

#[tokio::test]
async fn daemon_bridge_delivers_agent_state_changed_and_closes_worktree_with_removal() {
    let dir = TempDir::new().unwrap();
    let repo_dir = dir.path().join("repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    init_git_repo(&repo_dir);

    let sock = dir.path().join("daedalusd.sock");
    let runtime = dir.path().join("runtime");

    let server = DaemonServer::bind_with_supervisor(sock.clone(), runtime, test_supervisor())
        .await
        .unwrap();
    server.set_worktrees_dir(dir.path().join("worktrees"));
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let srv_handle = tokio::spawn(async move {
        let _ = server
            .serve(async {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    let (evt_tx, mut evt_rx) = mpsc::unbounded_channel();
    let sink: Arc<dyn UiEventSink> = Arc::new(ChannelSink { tx: evt_tx });
    let bridge = DaemonBridge::start(
        BridgeConfig {
            bootstrap_explicit_dir: true,
            socket_path: sock.clone(),
            initial_dir: repo_dir.clone(),
            default_agent: AgentPreset::Shell,
            auth_token: None,
            auto_spawn_daemon: false,
        },
        sink,
    );

    let mut primary_ws_id = None;
    let mut agent_pane_id = None;
    let mut spawned = 0usize;
    tokio::time::timeout(Duration::from_secs(10), async {
        while primary_ws_id.is_none() || spawned < 3 {
            match evt_rx.recv().await.expect("event channel closed") {
                UiEvent::WorkspaceCreated(ws) => primary_ws_id = Some(ws.id),
                UiEvent::PaneSpawned { summary, .. } => {
                    spawned += 1;
                    if summary.role == PaneRole::Agent {
                        agent_pane_id = Some(summary.id);
                    }
                }
                _ => {}
            }
        }
    })
    .await
    .expect("timed out waiting for initial workspace");

    let ws_id = primary_ws_id.unwrap();
    let agent_id = agent_pane_id.unwrap();

    // Trigger SetAgentState over a separate daemon connection and verify UiEvent::AgentStateChanged arrives.
    let mut ctl_stream = IpcStream::connect(&sock).await.unwrap();
    let req = Request {
        id: 1,
        payload: ClientMessage::SetAgentState {
            pane_id: Some(agent_id),
            workspace_id: Some(ws_id),
            root_dir: None,
            state: AgentState::WaitingForApproval,
        },
    };
    write_frame(&mut ctl_stream, &req).await.unwrap();
    let resp: ServerMessage = read_frame(&mut ctl_stream).await.unwrap().unwrap();
    assert!(matches!(
        resp,
        ServerMessage::Response {
            id: 1,
            result: Ok(ResponsePayload::Ok)
        }
    ));

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let UiEvent::AgentStateChanged {
                workspace_id,
                pane_id,
                state,
            } = evt_rx.recv().await.expect("event channel closed")
                && workspace_id == ws_id
                && pane_id == agent_id
                && state == AgentState::WaitingForApproval
            {
                break;
            }
        }
    })
    .await
    .expect("timed out waiting for UiEvent::AgentStateChanged");

    // Create a git worktree workspace through DaemonBridge, then close it with remove_worktree: true.
    bridge.send(UiCommand::CreateWorktreeWorkspace {
        repo_dir,
        branch: "feat/bridge-cleanup".to_string(),
        default_agent: AgentPreset::Shell,
    });

    let wt_ws = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let UiEvent::WorkspaceCreated(ws) =
                evt_rx.recv().await.expect("event channel closed")
                && ws.id != ws_id
            {
                break ws;
            }
        }
    })
    .await
    .expect("timed out waiting for worktree WorkspaceCreated");

    assert!(
        wt_ws.git_status.is_some_and(|gs| gs.is_worktree),
        "expected created worktree workspace to report is_worktree=true"
    );
    assert!(
        wt_ws.root_dir.exists(),
        "expected worktree directory to exist before close"
    );

    let dirty_file = wt_ws.root_dir.join("unsaved.txt");
    std::fs::write(&dirty_file, "keep this file").unwrap();
    bridge.send(UiCommand::CloseWorkspace {
        id: wt_ws.id,
        compose_down: false,
        remove_worktree: true,
        force_remove_worktree: false,
    });
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let UiEvent::WorkspaceCloseFailed(workspaces) = evt_rx.recv().await.unwrap() {
                let retained = workspaces.iter().find(|ws| ws.id == wt_ws.id).unwrap();
                assert_eq!(retained.panes.len(), 3);
                break;
            }
        }
    })
    .await
    .expect("failed close should refresh workspace and panes");
    std::fs::remove_file(dirty_file).unwrap();

    bridge.send(UiCommand::CloseWorkspace {
        id: wt_ws.id,
        compose_down: false,
        remove_worktree: true,
        force_remove_worktree: false,
    });

    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let UiEvent::WorkspacesLoaded(wss) =
                evt_rx.recv().await.expect("event channel closed")
                && !wss.iter().any(|w| w.id == wt_ws.id)
            {
                break;
            }
        }
    })
    .await
    .expect("timed out waiting for WorkspacesLoaded after CloseWorkspace");

    assert!(
        !wt_ws.root_dir.exists(),
        "expected CloseWorkspace(remove_worktree=true) to remove {}",
        wt_ws.root_dir.display()
    );

    tokio::task::spawn_blocking(move || bridge.shutdown())
        .await
        .expect("bridge shutdown task panicked");
    let _ = shutdown_tx.send(());
    tokio::time::timeout(Duration::from_secs(5), srv_handle)
        .await
        .expect("timed out waiting for DaemonServer shutdown")
        .expect("DaemonServer task panicked");
}

#[tokio::test]
async fn authenticated_tcp_bridge_runs_workspace_action() {
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join(".daedalus.toml"),
        r#"
[[actions]]
id = "mark"
label = "Mark workspace"
target = "runner"
command = "mkdir action-result"
"#,
    )
    .unwrap();
    let server = DaemonServer::bind_with_supervisor(
        "tcp://127.0.0.1:0".into(),
        dir.path().join("runtime"),
        test_supervisor(),
    )
    .await
    .unwrap()
    .with_auth_token(Some("bridge-test".into()));
    let socket_path = server.socket_path();
    let (stop, stopped) = oneshot::channel::<()>();
    let daemon = tokio::spawn(server.serve(async {
        let _ = stopped.await;
    }));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let bridge = DaemonBridge::start(
        BridgeConfig {
            bootstrap_explicit_dir: true,
            socket_path,
            initial_dir: dir.path().to_owned(),
            default_agent: AgentPreset::Shell,
            auto_spawn_daemon: false,
            auth_token: Some("bridge-test".into()),
        },
        Arc::new(ChannelSink { tx }),
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        let workspace_id = loop {
            if let UiEvent::WorkspaceCreated(ws) = rx.recv().await.unwrap() {
                break ws.id;
            }
        };
        bridge.send(UiCommand::RunWorkspaceAction {
            workspace_id,
            action_id: "mark".into(),
        });
        while !dir.path().join("action-result").is_dir() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("authenticated action did not write its result");
    tokio::task::spawn_blocking(move || bridge.shutdown())
        .await
        .unwrap();
    stop.send(()).unwrap();
    daemon.await.unwrap().unwrap();
}

#[tokio::test]
async fn queued_terminal_input_stays_with_its_connection() {
    use tokio::net::{TcpListener, TcpStream};

    async fn handshake(listener: &TcpListener) -> (TcpStream, Request) {
        let (mut peer, _) = listener.accept().await.unwrap();
        for expected in [
            ClientMessage::Ping,
            ClientMessage::Subscribe {
                pane_ids: None,
                project_setup_events: false,
            },
        ] {
            let request = read_frame::<_, Request>(&mut peer).await.unwrap().unwrap();
            assert_eq!(request.payload, expected);
            let result = match expected {
                ClientMessage::Ping => ResponsePayload::Pong {
                    version: "test".into(),
                    vt_backend: "builtin".into(),
                    isolation_mode: "test".into(),
                    journal_supported: false,
                    inspect_setup_supported: false,
                    recent_projects_supported: false,
                    project_setup_supported: false,
                    image_preview_supported: false,
                    home_dir: None,
                },
                _ => ResponsePayload::Ok,
            };
            write_frame(
                &mut peer,
                &ServerMessage::Response {
                    id: request.id,
                    result: Ok(result),
                },
            )
            .await
            .unwrap();
        }
        let list = read_frame::<_, Request>(&mut peer).await.unwrap().unwrap();
        assert_eq!(list.payload, ClientMessage::ListWorkspaces);
        (peer, list)
    }

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (tx, mut events) = mpsc::unbounded_channel();
    let bridge = DaemonBridge::start(
        BridgeConfig {
            bootstrap_explicit_dir: true,
            socket_path: format!("tcp://{}", listener.local_addr().unwrap()).into(),
            initial_dir: ".".into(),
            default_agent: AgentPreset::Shell,
            auto_spawn_daemon: false,
            auth_token: None,
        },
        Arc::new(ChannelSink { tx }),
    );
    let reconnect = async {
        let (peer, _) = handshake(&listener).await;
        let pane_id = daedalus_proto::PaneId::new();
        // The bridge is ready for submissions but cannot dispatch until ListWorkspaces returns.
        assert!(bridge.send(UiCommand::SendInput {
            pane_id,
            data: b"stale".to_vec(),
        }));
        drop(peer);
        let mut reported = false;
        loop {
            match events.recv().await.unwrap() {
                UiEvent::Toast {
                    is_error: true,
                    message,
                } if message.contains("Pending commands") => {
                    reported = true;
                }
                UiEvent::Disconnected => break,
                _ => {}
            }
        }
        assert!(reported);
        let (mut peer, list) = handshake(&listener).await;
        let workspace = toml::from_str(&format!(
            r#"id = "{}"
               name = "test"
               root_dir = "."
               slot_index = 0
               panes = []
               injected_env = {{}}"#,
            daedalus_proto::WorkspaceId::new()
        ))
        .unwrap();
        write_frame(
            &mut peer,
            &ServerMessage::Response {
                id: list.id,
                result: Ok(ResponsePayload::Workspaces(vec![workspace])),
            },
        )
        .await
        .unwrap();
        assert!(bridge.send(UiCommand::SendInput {
            pane_id,
            data: b"fresh".to_vec(),
        }));
        let request = read_frame::<_, Request>(&mut peer).await.unwrap().unwrap();
        assert_eq!(
            request.payload,
            ClientMessage::SendInput {
                pane_id,
                data: b"fresh".to_vec(),
            }
        );
    };
    tokio::time::timeout(Duration::from_secs(10), reconnect)
        .await
        .expect("bridge failed to recover after interrupted initialization");
}

#[tokio::test]
async fn shutdown_interrupts_unanswered_initialization_rpc() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (tx, _rx) = mpsc::unbounded_channel();
    let bridge = DaemonBridge::start(
        BridgeConfig {
            bootstrap_explicit_dir: true,
            socket_path: format!("tcp://{}", listener.local_addr().unwrap()).into(),
            initial_dir: ".".into(),
            default_agent: AgentPreset::Shell,
            auto_spawn_daemon: false,
            auth_token: None,
        },
        Arc::new(ChannelSink { tx }),
    );
    let (mut peer, _) = listener.accept().await.unwrap();
    let request = read_frame::<_, Request>(&mut peer).await.unwrap().unwrap();
    assert!(matches!(request.payload, ClientMessage::Ping));
    let shutdown = tokio::task::spawn_blocking(move || bridge.shutdown());
    let result = tokio::time::timeout(Duration::from_secs(2), shutdown).await;
    drop(peer);
    result
        .expect("shutdown waited for the silent daemon")
        .unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn attached_client_receives_cursor_and_history_only_deltas() {
    use daedalus_ui::state::PaneViewState;
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join(".daedalus.toml"),
        "[runner]\ncommand = ['sh', 'runner.sh']\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("runner.sh"),
        r#"stty -echo
read step
i=0
while [ "$i" -lt 45 ]; do echo old; i=$((i + 1)); done
printf LAST
read step
printf '\033[2D'
read step
printf '\033[?25l'
read step
printf '\033[3J'
read step
"#,
    )
    .unwrap();
    let server = DaemonServer::bind_with_supervisor(
        dir.path().join("daemon.sock"),
        dir.path().join("runtime"),
        test_supervisor(),
    )
    .await
    .unwrap();
    let socket_path = server.socket_path();
    let (stop, stopped) = oneshot::channel::<()>();
    let daemon = tokio::spawn(server.serve(async {
        let _ = stopped.await;
    }));
    let (tx, mut events) = mpsc::unbounded_channel();
    let bridge = DaemonBridge::start(
        BridgeConfig {
            bootstrap_explicit_dir: true,
            socket_path,
            initial_dir: dir.path().to_owned(),
            default_agent: AgentPreset::Shell,
            auto_spawn_daemon: false,
            auth_token: None,
        },
        Arc::new(ChannelSink { tx }),
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut pane = loop {
            if let UiEvent::PaneSpawned { summary, .. } = events.recv().await.unwrap()
                && summary.role == PaneRole::Runner
            {
                break PaneViewState::from_summary(summary);
            }
        };
        let id = pane.summary.id;
        bridge.send(UiCommand::SendInput {
            pane_id: id,
            data: b"start\n".to_vec(),
        });
        loop {
            if let UiEvent::ServerEvent(ServerEvent::GridDelta(delta)) =
                events.recv().await.unwrap()
                && delta.pane_id == id
            {
                pane.apply_delta(delta);
                if pane.scrollback_len > 0
                    && pane.cursor.col == 4
                    && pane
                        .viewport_rows
                        .last()
                        .is_some_and(|row| row.cells[0].text == "L")
                {
                    break;
                }
            }
        }
        pane.set_scroll_offset(5);
        let generation = pane.begin_scrollback_request().unwrap();
        bridge.send(UiCommand::RequestScrollback {
            pane_id: id,
            generation,
            offset_from_bottom: 5,
            count: usize::from(pane.summary.rows),
        });
        loop {
            if let UiEvent::ScrollbackLoaded {
                pane_id,
                generation,
                total_scrollback,
                rows,
            } = events.recv().await.unwrap()
                && pane_id == id
            {
                pane.apply_scrollback(generation, total_scrollback, rows);
                break;
            }
        }
        assert_eq!(pane.scroll_offset, 5);
        for (col, visible, scroll_offset) in [(2, true, 5), (2, false, 5), (2, false, 0)] {
            bridge.send(UiCommand::SendInput {
                pane_id: id,
                data: b"step\n".to_vec(),
            });
            loop {
                if let UiEvent::ServerEvent(ServerEvent::GridDelta(delta)) =
                    events.recv().await.unwrap()
                    && delta.pane_id == id
                {
                    assert!(delta.dirty_rows.is_empty());
                    pane.apply_delta(delta);
                    break;
                }
            }
            assert_eq!(
                (pane.cursor.col, pane.cursor.visible, pane.scroll_offset),
                (col, visible, scroll_offset)
            );
        }
        assert_eq!(pane.scrollback_len, 0);
        let last = pane.visible_rows().last().unwrap();
        assert!(
            last.cells
                .iter()
                .map(|cell| cell.text.as_str())
                .collect::<String>()
                .starts_with("LAST")
        );
    })
    .await
    .expect("missing terminal delta");
    tokio::task::spawn_blocking(move || bridge.shutdown())
        .await
        .unwrap();
    stop.send(()).unwrap();
    daemon.await.unwrap().unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn manual_workspace_runs_only_explicit_assignments_and_stays_manual_after_restart() {
    use daedalus_ui::assignments::Assignment;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("manual");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(
        project.join(".daedalus.toml"),
        r#"
[runner]
command = ["sh", "-c", "touch unexpected-manifest-launch"]
"#,
    )
    .unwrap();
    let mut manual_id = None;
    for restarted in [false, true] {
        let sock = dir.path().join("daemon.sock");
        let server = DaemonServer::bind_with_options(
            sock.clone(),
            dir.path().join("runtime"),
            None,
            Some(dir.path().join("state.json")),
            test_supervisor(),
        )
        .await
        .unwrap();
        let (stop, stopped) = oneshot::channel();
        let daemon = tokio::spawn(async move {
            server
                .serve(async {
                    let _ = stopped.await;
                })
                .await
                .unwrap()
        });
        let (tx, mut events) = mpsc::unbounded_channel();
        let bridge = DaemonBridge::start(
            BridgeConfig {
                bootstrap_explicit_dir: true,
                socket_path: sock,
                initial_dir: dir.path().into(),
                default_agent: AgentPreset::Shell,
                auth_token: None,
                auto_spawn_daemon: false,
            },
            Arc::new(ChannelSink { tx }),
        );
        tokio::time::timeout(Duration::from_secs(12), async {
            if restarted {
                loop {
                    if let UiEvent::WorkspacesLoaded(workspaces) = events.recv().await.unwrap() {
                        let workspace = workspaces
                            .iter()
                            .find(|ws| Some(ws.id) == manual_id)
                            .unwrap();
                        assert!(!workspace.needs_default_panes());
                        assert!(workspace.panes.is_empty());
                        break;
                    }
                }
                return;
            }
            let mut startup_panes = 0;
            while startup_panes < 3 {
                if matches!(events.recv().await.unwrap(), UiEvent::PaneSpawned { .. }) {
                    startup_panes += 1;
                }
            }
            assert!(bridge.send(UiCommand::CreateManualWorkspace {
                session: 1,
                name: "Manual project".into(),
                root_dir: project.join("missing"),
            }));
            loop {
                if let UiEvent::ManualWorkspaceFailed { session, message } =
                    events.recv().await.unwrap()
                {
                    assert_eq!(session, 1);
                    assert!(message.contains("not a directory"));
                    break;
                }
            }
            assert!(bridge.send(UiCommand::CreateManualWorkspace {
                session: 2,
                name: "Manual project".into(),
                root_dir: project.clone()
            }));
            let ws = loop {
                if let UiEvent::ManualWorkspaceCreated { session, workspace } =
                    events.recv().await.unwrap()
                {
                    assert_eq!(session, 2);
                    break workspace;
                }
            };
            assert_eq!(ws.root_dir, project);
            assert!(ws.manifest.is_some());
            assert!(ws.panes.is_empty());
            assert!(!ws.needs_default_panes());
            manual_id = Some(ws.id);
            let mut launched_panes = Vec::new();
            for (id, role) in [("first", PaneRole::Agent), ("second", PaneRole::Runner)] {
                assert!(!project.join(id).exists());
                let assignment = Assignment {
                    id: id.into(),
                    name: format!("Assignment {id}"),
                    role,
                    argv: vec![
                        "sh".into(),
                        "-c".into(),
                        r#"printf '%s\n' "$2" >> "$1""#.into(),
                        "fixture".into(),
                        id.into(),
                        "two words $HOME".into(),
                    ],
                };
                assert!(bridge.send(UiCommand::LaunchAssignment {
                    workspace_id: ws.id,
                    assignment: assignment.clone(),
                    cols: 80,
                    rows: 24
                }));
                let mut spawned = None;
                loop {
                    match events.recv().await.unwrap() {
                        UiEvent::PaneSpawned { summary, .. } => {
                            assert_eq!(summary.role, role);
                            assert!(spawned.replace(summary.id).is_none());
                        }
                        UiEvent::AssignmentStarted {
                            workspace_id,
                            pane_id,
                            assignment: actual,
                        } => {
                            assert_eq!(workspace_id, ws.id);
                            assert_eq!(actual, assignment);
                            assert_eq!(spawned, Some(pane_id));
                            assert!(!launched_panes.contains(&pane_id));
                            launched_panes.push(pane_id);
                            break;
                        }
                        _ => {}
                    }
                }
                while std::fs::read_to_string(project.join(id))
                    .unwrap_or_default()
                    .is_empty()
                {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        })
        .await
        .expect("manual workspace creation, launch or restoration timed out");
        tokio::task::spawn_blocking(move || bridge.shutdown())
            .await
            .unwrap();
        stop.send(()).unwrap();
        daemon.await.unwrap();
        assert!(!project.join("unexpected-manifest-launch").exists());
        for id in ["first", "second"] {
            assert_eq!(
                std::fs::read_to_string(project.join(id)).unwrap(),
                "two words $HOME\n"
            );
            assert!(!dir.path().join(id).exists());
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn wizard_inspects_installs_and_launches_only_selected_roles() {
    use daedalus_ui::assignments::{self, Assignment};
    // The bridge uses the user's assignment directory. Isolate it without changing
    // the environment of other tests running in this process.
    const CHILD: &str = "DAEDALUS_TEST_SETUP_WORKSPACE";
    let Some(project) = std::env::var_os(CHILD).map(std::path::PathBuf::from) else {
        let dir = TempDir::new().unwrap();
        let project = dir.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "wizard_inspects_installs_and_launches_only_selected_roles",
                "--nocapture",
            ])
            .env(CHILD, &project)
            .env("DAEDALUS_CONFIG", dir.path().join("config.toml"))
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    };
    let dir = TempDir::new().unwrap();
    let sock = dir.path().join("daemon.sock");
    let server = DaemonServer::bind_with_supervisor(
        sock.clone(),
        dir.path().join("runtime"),
        test_supervisor(),
    )
    .await
    .unwrap();
    let (stop, stopped) = oneshot::channel();
    let daemon = tokio::spawn(async move {
        server
            .serve(async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });
    let config = BridgeConfig {
        socket_path: sock,
        initial_dir: project.clone(),
        bootstrap_explicit_dir: false,
        default_agent: AgentPreset::Shell,
        auth_token: None,
        auto_spawn_daemon: false,
    };
    let connect = || {
        let (tx, events) = mpsc::unbounded_channel();
        (
            DaemonBridge::start(config.clone(), Arc::new(ChannelSink { tx })),
            events,
        )
    };
    let (bridge, mut events) = connect();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let UiEvent::WorkspacesLoaded(workspaces) = events.recv().await.unwrap() {
                assert!(workspaces.is_empty());
                break;
            }
        }
        assert!(bridge.send(UiCommand::InspectSetup {
            session: 4,
            root_dir: project.clone(),
            executables: vec!["sh".into()]
        }));
        loop {
            if let UiEvent::SetupInspected { session, result } = events.recv().await.unwrap() {
                assert_eq!(session, 4);
                assert!(
                    result
                        .unwrap()
                        .available
                        .iter()
                        .any(|executable| executable == "sh")
                );
                break;
            }
        }
        assert!(bridge.send(UiCommand::CreateManualWorkspace {
            session: 4,
            name: "Wizard".into(),
            root_dir: project.clone()
        }));
        let ws = loop {
            if let UiEvent::ManualWorkspaceCreated { workspace, .. } = events.recv().await.unwrap()
            {
                assert!(!workspace.needs_default_panes());
                assert!(workspace.panes.is_empty());
                break workspace.id;
            }
        };
        assert!(bridge.send(UiCommand::InstallSetupTool {
            workspace_id: ws,
            argv: vec![
                "sh".into(),
                "-c".into(),
                r#"read -r answer && [ "$answer" = resume ] && touch "$1""#.into(),
                "fixture".into(),
                "installed tool".into()
            ]
        }));
        let installer = loop {
            if let UiEvent::SetupInstallerStarted {
                workspace_id,
                pane_id,
            } = events.recv().await.unwrap()
            {
                assert_eq!(workspace_id, ws);
                break pane_id;
            }
        };

        // Keep the daemon and its PTY alive across a fresh bridge connection.
        tokio::task::spawn_blocking(move || bridge.shutdown())
            .await
            .unwrap();
        let (bridge, mut events) = connect();
        loop {
            if let UiEvent::WorkspacesLoaded(workspaces) = events.recv().await.unwrap() {
                assert_eq!(workspaces.len(), 1);
                let workspace = &workspaces[0];
                assert_eq!(workspace.id, ws);
                assert_eq!(workspace.panes.len(), 1);
                let pane = &workspace.panes[0];
                assert_eq!(pane.id, installer);
                assert!(!matches!(
                    pane.status,
                    daedalus_proto::PaneStatus::Exited { .. }
                ));
                break;
            }
        }
        assert!(bridge.send(UiCommand::SendInput {
            pane_id: installer,
            data: b"resume\n".to_vec()
        }));
        loop {
            if let UiEvent::ServerEvent(ServerEvent::PaneExited { pane_id, exit_code }) =
                events.recv().await.unwrap()
            {
                assert_eq!(pane_id, installer);
                assert_eq!(exit_code, Some(0));
                break;
            }
        }
        let entries = vec![
            Assignment {
                id: "agent".into(),
                name: "Unavailable agent".into(),
                role: PaneRole::Agent,
                argv: vec!["daedalus-test-absent-agent".into()],
            },
            Assignment {
                id: "serve".into(),
                name: "Serve".into(),
                role: PaneRole::Service,
                argv: vec!["touch".into(), "started service".into()],
            },
            Assignment {
                id: "test".into(),
                name: "Run tests".into(),
                role: PaneRole::Runner,
                argv: vec!["touch".into(), "unexpected tests".into()],
            },
        ];
        assert!(bridge.send(UiCommand::ApplySetup {
            workspace_id: ws,
            assignments: entries.clone(),
            launch: vec!["agent".into(), "serve".into()]
        }));
        let mut started = Vec::new();
        loop {
            match events.recv().await.unwrap() {
                UiEvent::AssignmentStarted { assignment, .. } => started.push(assignment.id),
                UiEvent::SetupApplied {
                    workspace_id,
                    error,
                } => {
                    assert_eq!(ws, workspace_id);
                    assert_eq!(error, None);
                    break;
                }
                _ => {}
            }
        }
        assert_eq!(started, ["serve"]);
        // The daemon owns the setup now, so nothing is written locally.
        assert!(assignments::load(ws).unwrap().is_empty());
        tokio::task::spawn_blocking(move || bridge.shutdown())
            .await
            .unwrap();
        let (bridge, mut events) = connect();
        loop {
            if let UiEvent::WorkspacesLoaded(workspaces) = events.recv().await.unwrap() {
                let workspace = workspaces.iter().find(|w| w.id == ws).unwrap();
                assert_eq!(workspace.setup.as_deref(), Some(entries.as_slice()));
                break;
            }
        }
        while !project.join("started service").exists() || !project.join("installed tool").exists()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!project.join("unexpected tests").exists());
        tokio::task::spawn_blocking(move || bridge.shutdown())
            .await
            .unwrap();
    })
    .await
    .expect("wizard setup timed out");
    stop.send(()).unwrap();
    daemon.await.unwrap();
}
