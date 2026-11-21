use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use daedalus_proto::{GridSnapshot, PaneId, PaneRole, ServerEvent};
use daedalus_ui::bridge::{BridgeConfig, DaemonBridge, UiCommand, UiEvent, UiEventSink};
use daedalus_ui::state::AgentPreset;
use daedalusd::server::DaemonServer;
use daedalusd::supervisor::{IsolationMode, Supervisor};
use tempfile::TempDir;
use tokio::sync::{mpsc, oneshot};

struct ChannelSink {
    tx: mpsc::UnboundedSender<UiEvent>,
}

impl UiEventSink for ChannelSink {
    fn emit(&self, event: UiEvent) {
        let _ = self.tx.send(event);
    }
}

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

#[tokio::test]
async fn daemon_bridge_bootstraps_three_panes_and_forwards_runner_failure_to_agent() {
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

    let (evt_tx, mut evt_rx) = mpsc::unbounded_channel();
    let sink: Arc<dyn UiEventSink> = Arc::new(ChannelSink { tx: evt_tx });
    let bridge = DaemonBridge::start(
        BridgeConfig {
            socket_path: sock,
            initial_dir: dir.path().to_path_buf(),
            default_agent: AgentPreset::Shell,
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
    let srv1_thread = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let server1 = DaemonServer::bind_with_supervisor(
                sock1,
                runtime1,
                Supervisor::with_mode(IsolationMode::PgidFallback),
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
            socket_path: sock.clone(),
            initial_dir: dir.path().to_path_buf(),
            default_agent: AgentPreset::Shell,
            auto_spawn_daemon: false,
        },
        sink,
    );

    // Wait for initial bootstrap to finish completely (3 panes spawned + 3 initial snapshots).
    let mut initial_panes = 0usize;
    let mut initial_snapshots = 0usize;
    tokio::time::timeout(Duration::from_secs(10), async {
        while initial_panes < 3 || initial_snapshots < 3 {
            match evt_rx.recv().await.expect("event channel closed") {
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

    // Start daemon 2 on the same socket and verify DaemonBridge reconnects and reloads workspaces.
    let server2 = DaemonServer::bind_with_supervisor(
        sock.clone(),
        runtime,
        Supervisor::with_mode(IsolationMode::PgidFallback),
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
    std::fs::write(
        ws_dir.join(".daedalus.toml"),
        r#"
[runner]
command = ["sh", "-c", "echo MANIFEST_RUNNER_NEEDLE; sleep 5"]
"#,
    )
    .unwrap();

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
            Supervisor::with_mode(IsolationMode::PgidFallback),
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
                socket_path: sock.clone(),
                initial_dir: ws_dir.clone(),
                default_agent: AgentPreset::Shell,
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
        Supervisor::with_mode(IsolationMode::PgidFallback),
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
            socket_path: sock,
            initial_dir: ws_dir,
            default_agent: AgentPreset::Shell,
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
                UiEvent::WorkspacePanesEnsured { workspace_id }
                    if workspace_id == dormant_ws.id =>
                {
                    ensured = true;
                }
                _ => {}
            }
        }
    })
    .await
    .expect("timed out waiting for EnsureWorkspacePanes");

    let runner_id = runner_pane_id.expect("runner pane must be spawned");

    // Poll SearchPane until the manifest runner command output appears in the VT grid.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut found = false;
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
                })) if pane_id == runner_id && !matches.is_empty() => {
                    found = true;
                    break;
                }
                Ok(Some(_)) => {}
                Ok(None) => panic!("event channel closed"),
                Err(_) => break,
            }
        }
    }
    assert!(
        found,
        "expected SearchPane to locate manifest runner output in restored pane"
    );

    tokio::task::spawn_blocking(move || bridge2.shutdown())
        .await
        .unwrap();
    let _ = tx2.send(());
    handle2.await.unwrap();
}
