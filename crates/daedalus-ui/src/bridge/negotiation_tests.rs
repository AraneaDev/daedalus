//! Handshake and setup routing against a fake daemon. Every wait is bounded.

use super::rpc::RpcConnection;
use super::test_support::ipc_pair;
use super::{
    BridgeConfig, UiCommand, UiEvent, UiEventSink, handle_command, initialize_session, setup,
};
use crate::assignments::{self, Assignment};
use crate::state::AgentPreset;
use daedalus_proto::transport::IpcStream;
use daedalus_proto::{
    ClientMessage, PaneRole, RecentProject, Request, ResponsePayload, ServerEvent, ServerMessage,
    WorkspaceId, read_frame, write_frame,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};

const WAIT: Duration = Duration::from_secs(5);

struct ChannelSink(mpsc::UnboundedSender<UiEvent>);
impl UiEventSink for ChannelSink {
    fn emit(&self, event: UiEvent) {
        let _ = self.0.send(event);
    }
}

fn pong(supported: bool) -> ResponsePayload {
    ResponsePayload::Pong {
        version: "test".into(),
        vt_backend: "vt".into(),
        isolation_mode: "none".into(),
        journal_supported: false,
        inspect_setup_supported: true,
        recent_projects_supported: supported,
        project_setup_supported: supported,
        image_preview_supported: supported,
        paste_supported: supported,
        workbench_supported: false,
        workspace_membership_supported: false,
        runner_assignment_supported: false,
        resolved_import_preview_supported: false,
        agent_launch_metadata_supported: false,
        home_dir: supported.then(|| PathBuf::from("/home/test")),
    }
}

#[tokio::test]
async fn launch_hint_only_fills_missing_agent_metadata_for_older_daemons() {
    let (client, _server) = ipc_pair().await;
    let (tx, mut events) = mpsc::unbounded_channel();
    let sink: Arc<dyn UiEventSink> = Arc::new(ChannelSink(tx));
    let mut conn = RpcConnection::new(client, sink.clone());
    let pane = daedalus_proto::PaneSummary {
        id: daedalus_proto::PaneId::new(),
        workspace_id: WorkspaceId::new(),
        role: PaneRole::Agent,
        status: daedalus_proto::PaneStatus::Idle,
        cols: 80,
        rows: 24,
        cwd: None,
        last_exit_code: None,
        has_failure_trace: false,
        agent_state: None,
        launch_preset: None,
    };
    for (supported, reported, expected) in [
        (false, None, Some("claude")),
        (false, Some("codex"), Some("codex")),
        (true, None, None),
        (true, Some("codex"), Some("codex")),
    ] {
        conn.agent_launch_metadata_supported = supported;
        let mut summary = pane.clone();
        summary.launch_preset = reported.map(str::to_owned);
        super::workspace::emit_spawned_pane(
            &conn,
            &sink,
            summary,
            Some(AgentPreset::Claude),
            false,
        );
        let UiEvent::PaneSpawned {
            summary,
            select_tab,
        } = events.try_recv().unwrap()
        else {
            panic!("expected pane registration");
        };
        assert!(!select_tab);
        assert_eq!(summary.launch_preset.as_deref(), expected);
    }
    conn.close().await;
}

fn recent() -> RecentProject {
    RecentProject {
        name: "demo".into(),
        root_dir: PathBuf::from("/work/demo"),
        last_used: 7,
        has_setup: true,
    }
}

fn assignment(name: &str) -> Assignment {
    Assignment {
        id: "a".into(),
        name: name.into(),
        role: PaneRole::Runner,
        argv: vec!["true".into()],
    }
}

/// Answers requests like a daemon would and reports each request it received.
fn fake_daemon(
    mut stream: IpcStream,
    pong: ResponsePayload,
) -> (
    tokio::task::JoinHandle<()>,
    mpsc::UnboundedReceiver<ClientMessage>,
    mpsc::UnboundedSender<ServerEvent>,
) {
    let (seen_tx, seen_rx) = mpsc::unbounded_channel();
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<ServerEvent>();
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                request = read_frame::<_, Request>(&mut stream) => {
                    let Ok(Some(request)) = request else { return };
                    let result = match &request.payload {
                        ClientMessage::Ping => pong.clone(),
                        ClientMessage::PreviewImage {path, ..} if path == "sample.png" => ResponsePayload::ImagePreview(daedalus_proto::rpc::ImagePreview{width:1,height:1,rgba:vec![0,0,0,255]}),
                        ClientMessage::ListWorkspaces => ResponsePayload::Workspaces(vec![]),
                        ClientMessage::ListRecentProjects => {
                            ResponsePayload::RecentProjects(vec![recent()])
                        }
                        _ => ResponsePayload::Ok,
                    };
                    let _ = seen_tx.send(request.payload);
                    let reply = ServerMessage::Response { id: request.id, result: Ok(result) };
                    if write_frame(&mut stream, &reply).await.is_err() {
                        return;
                    }
                }
                Some(event) = event_rx.recv() => {
                    if write_frame(&mut stream, &ServerMessage::Event(event)).await.is_err() {
                        return;
                    }
                }
            }
        }
    });
    (task, seen_rx, event_tx)
}

fn config() -> BridgeConfig {
    BridgeConfig {
        socket_path: PathBuf::from("unused"),
        auth_token: None,
        initial_dir: PathBuf::from("/"),
        bootstrap_explicit_dir: false,
        default_agent: AgentPreset::default(),
        auto_spawn_daemon: false,
    }
}

async fn handshake(
    supported: bool,
) -> (
    RpcConnection,
    mpsc::UnboundedReceiver<UiEvent>,
    Vec<UiEvent>,
    tokio::task::JoinHandle<()>,
    mpsc::UnboundedReceiver<ClientMessage>,
    mpsc::UnboundedSender<ServerEvent>,
) {
    let (client, server) = ipc_pair().await;
    let (task, seen, events) = fake_daemon(server, pong(supported));
    let (ui_tx, mut ui_rx) = mpsc::unbounded_channel();
    let sink: Arc<dyn UiEventSink> = Arc::new(ChannelSink(ui_tx));
    let mut conn = RpcConnection::new(client, Arc::clone(&sink));
    let (session, _keep) = watch::channel(None);
    let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel();
    let mut bootstrapped = false;
    tokio::time::timeout(
        WAIT,
        initialize_session(
            &mut conn,
            &config(),
            &sink,
            &mut bootstrapped,
            &session,
            cmd_tx,
        ),
    )
    .await
    .expect("handshake timed out")
    .expect("handshake failed");
    let mut emitted = Vec::new();
    while let Ok(event) = ui_rx.try_recv() {
        emitted.push(event);
    }
    (conn, ui_rx, emitted, task, seen, events)
}

fn drain(seen: &mut mpsc::UnboundedReceiver<ClientMessage>) -> Vec<ClientMessage> {
    let mut all = Vec::new();
    while let Ok(message) = seen.try_recv() {
        all.push(message);
    }
    all
}

#[tokio::test]
async fn resolving_import_conflicts_requires_the_negotiated_daemon_capability() {
    for supported in [false, true] {
        let (client, server) = ipc_pair().await;
        let (task, mut seen, _) = fake_daemon(server, pong(false));
        let (ui_tx, mut ui_rx) = mpsc::unbounded_channel();
        let sink: Arc<dyn UiEventSink> = Arc::new(ChannelSink(ui_tx));
        let mut conn = RpcConnection::new(client, Arc::clone(&sink));
        conn.workbench_supported = true;
        conn.resolved_import_preview_supported = supported;
        let workspace_id = WorkspaceId::new();
        let operation = super::WorkbenchOperation::ImportPreview(uuid::Uuid::new_v4());
        let request = ClientMessage::ResolveWorkspaceImport {
            workspace_id,
            revision: 1,
            incoming: daedalus_proto::workbench::WorkspaceDocument::new("Imported".into()),
            linked: false,
            resolutions: Default::default(),
        };
        tokio::time::timeout(
            WAIT,
            handle_command(
                &conn,
                &sink,
                UiCommand::Workbench {
                    workspace_id,
                    operation: operation.clone(),
                    request: Box::new(request.clone()),
                },
            ),
        )
        .await
        .unwrap()
        .unwrap();
        let UiEvent::Workbench {
            operation: received,
            response,
            ..
        } = ui_rx.try_recv().unwrap()
        else {
            panic!("expected import reply");
        };
        assert_eq!(received, operation);
        if supported {
            assert_eq!(*response, Ok(ResponsePayload::Ok));
            assert_eq!(seen.try_recv().unwrap(), request);
        } else {
            assert!(matches!(*response, Err(message) if message.contains("Upgrade the daemon")));
            assert!(seen.try_recv().is_err());
        }
        task.abort();
    }
}

#[tokio::test]
async fn old_daemon_gets_no_new_requests() {
    let (conn, _ui, emitted, task, mut seen, _events) = handshake(false).await;

    assert!(emitted.iter().any(|event| matches!(
        event,
        UiEvent::DaemonCapabilities {
            home_dir: None,
            project_setups: false,
            recent_projects: false
        }
    )));
    assert!(
        !emitted
            .iter()
            .any(|event| matches!(event, UiEvent::RecentProjectsLoaded(_)))
    );

    let (ui_tx, _ui_rx) = mpsc::unbounded_channel();
    let sink: Arc<dyn UiEventSink> = Arc::new(ChannelSink(ui_tx));
    tokio::time::timeout(
        WAIT,
        handle_command(&conn, &sink, UiCommand::RefreshRecentProjects),
    )
    .await
    .unwrap()
    .unwrap();
    // An invalid entry fails local validation, so nothing is written to disk either.
    tokio::time::timeout(
        WAIT,
        setup::apply(
            &conn,
            &sink,
            WorkspaceId::new(),
            vec![assignment("")],
            vec![],
        ),
    )
    .await
    .unwrap()
    .unwrap();

    let workspace_id = WorkspaceId::new();
    tokio::time::timeout(
        WAIT,
        handle_command(
            &conn,
            &sink,
            UiCommand::Workbench {
                workspace_id,
                operation: super::WorkbenchOperation::Read,
                request: Box::new(ClientMessage::GetWidgetResults {
                    workspace_id,
                    refresh: None,
                }),
            },
        ),
    )
    .await
    .unwrap()
    .unwrap();

    let requests = drain(&mut seen);
    assert!(requests.iter().any(|message| matches!(
        message,
        ClientMessage::Subscribe {
            project_setup_events: false,
            ..
        }
    )));
    assert!(!requests.iter().any(|message| matches!(
        message,
        ClientMessage::ListRecentProjects
            | ClientMessage::SetProjectSetup { .. }
            | ClientMessage::WatchWorkspaceMembership
            | ClientMessage::GetWidgetResults { .. }
    )));
    conn.close().await;
    task.abort();
}

#[tokio::test]
async fn new_daemon_handshake_loads_recent_projects() {
    let (conn, _ui, emitted, task, mut seen, _events) = handshake(true).await;
    let order: Vec<&str> = emitted
        .iter()
        .filter_map(|event| match event {
            UiEvent::DaemonCapabilities {
                home_dir,
                project_setups,
                recent_projects,
            } => {
                assert_eq!(
                    home_dir.as_deref(),
                    Some(std::path::Path::new("/home/test"))
                );
                assert!(*project_setups);
                assert!(*recent_projects);
                Some("capabilities")
            }
            UiEvent::RecentProjectsLoaded(projects) => {
                assert_eq!(projects, &vec![recent()]);
                Some("recent")
            }
            UiEvent::WorkspacesLoaded(_) => Some("workspaces"),
            _ => None,
        })
        .collect();
    assert_eq!(order, ["capabilities", "recent", "workspaces"]);
    assert!(drain(&mut seen).iter().any(|message| matches!(
        message,
        ClientMessage::Subscribe {
            project_setup_events: true,
            ..
        }
    )));
    conn.close().await;
    task.abort();
}

#[tokio::test]
async fn apply_stores_setup_on_daemon_when_supported() {
    let (conn, _ui, _emitted, task, mut seen, _events) = handshake(true).await;
    drain(&mut seen);
    let (ui_tx, mut ui_rx) = mpsc::unbounded_channel();
    let sink: Arc<dyn UiEventSink> = Arc::new(ChannelSink(ui_tx));
    let workspace_id = WorkspaceId::new();
    let entries = vec![assignment("check")];
    tokio::time::timeout(
        WAIT,
        setup::apply(&conn, &sink, workspace_id, entries.clone(), vec![]),
    )
    .await
    .unwrap()
    .unwrap();

    let sent = drain(&mut seen);
    assert!(sent.iter().any(|message| matches!(
        message,
        ClientMessage::SetProjectSetup { workspace_id: id, assignments: stored }
            if *id == workspace_id && *stored == entries
    )));
    assert!(matches!(
        ui_rx.try_recv(),
        Ok(UiEvent::SetupApplied { error: None, .. })
    ));
    assert!(assignments::load(workspace_id).unwrap().is_empty());
    conn.close().await;
    task.abort();
}

#[tokio::test]
async fn project_setup_change_event_is_forwarded() {
    let (conn, mut ui, _emitted, task, _seen, events) = handshake(true).await;
    events
        .send(ServerEvent::ProjectSetupChanged {
            root_dir: PathBuf::from("/work/demo"),
            assignments: vec![assignment("check")],
        })
        .unwrap();
    let event = tokio::time::timeout(WAIT, ui.recv())
        .await
        .unwrap()
        .unwrap();
    match event {
        UiEvent::ProjectSetupChanged {
            root_dir,
            assignments,
        } => {
            assert_eq!(root_dir, PathBuf::from("/work/demo"));
            assert_eq!(assignments, vec![assignment("check")]);
        }
        other => panic!("unexpected event {other:?}"),
    }
    conn.close().await;
    task.abort();
}

#[tokio::test]
async fn invalid_setup_on_new_daemon_is_rejected_locally() {
    let (conn, _ui, _emitted, task, mut seen, _events) = handshake(true).await;
    drain(&mut seen);
    let (ui_tx, mut ui_rx) = mpsc::unbounded_channel();
    let sink: Arc<dyn UiEventSink> = Arc::new(ChannelSink(ui_tx));
    let workspace_id = WorkspaceId::new();
    let invalid = vec![assignment("")];

    tokio::time::timeout(
        WAIT,
        setup::apply(&conn, &sink, workspace_id, invalid.clone(), vec![]),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(
        ui_rx.try_recv(),
        Ok(UiEvent::SetupApplied { error: Some(_), .. })
    ));
    tokio::time::timeout(
        WAIT,
        handle_command(
            &conn,
            &sink,
            UiCommand::SaveProjectSetup {
                workspace_id,
                assignments: invalid,
            },
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(
        ui_rx.try_recv(),
        Ok(UiEvent::ProjectSetupSaveFailed { .. })
    ));
    assert!(
        !drain(&mut seen)
            .iter()
            .any(|message| matches!(message, ClientMessage::SetProjectSetup { .. }))
    );
    conn.close().await;
    task.abort();
}

#[tokio::test]
async fn preview_only_queries_capable_daemon_and_validates_reply() {
    for supported in [false, true] {
        let (conn, _, _, task, mut seen, _events) = handshake(supported).await;
        let (tx, mut ui) = mpsc::unbounded_channel();
        let sink: Arc<dyn UiEventSink> = Arc::new(ChannelSink(tx));
        let id = WorkspaceId::new();
        let pane = daedalus_proto::PaneId::new();
        for path in ["sample.png", "bad.png"] {
            let cmd = UiCommand::PreviewImage {
                workspace_id: id,
                pane_id: pane,
                path: path.into(),
            };
            tokio::time::timeout(WAIT, handle_command(&conn, &sink, cmd))
                .await
                .unwrap()
                .unwrap();
            let event = ui.try_recv().unwrap();
            if supported && path == "sample.png" {
                assert!(
                    matches!(event,UiEvent::ImagePreview{pane_id,image} if pane_id == pane && image.valid())
                );
            } else {
                assert!(matches!(event, UiEvent::Toast { is_error: true, .. }));
            }
            assert_eq!(drain(&mut seen).iter().any(|r| matches!(r,ClientMessage::PreviewImage{workspace_id,path:p} if *workspace_id == id && p == path)),supported);
        }
        conn.close().await;
        task.abort();
    }
}

#[tokio::test]
async fn saved_runner_launch_negotiates_identity_without_changing_ad_hoc_launch() {
    for (supported, saved_runner) in [(true, true), (true, false), (false, true)] {
        let (client, server) = ipc_pair().await;
        let (task, mut seen, _) = fake_daemon(server, pong(false));
        let (ui_tx, _) = mpsc::unbounded_channel();
        let sink: Arc<dyn UiEventSink> = Arc::new(ChannelSink(ui_tx));
        let mut conn = RpcConnection::new(client, Arc::clone(&sink));
        conn.runner_assignment_supported = supported;
        let workspace_id = WorkspaceId::new();
        let entry = assignment("Runner");
        let result = tokio::time::timeout(
            WAIT,
            setup::launch_assignment(
                &conn,
                &sink,
                workspace_id,
                entry.clone(),
                saved_runner,
                80,
                24,
            ),
        )
        .await
        .unwrap();
        // The fake daemon records the request and returns a generic Ok.
        assert!(result.is_err());
        let request = seen.try_recv().unwrap();
        if supported && saved_runner {
            assert!(matches!(request, ClientMessage::LaunchRunnerAssignment {
                workspace_id: id, assignment_id, cols: 80, rows: 24,
            } if id == workspace_id && assignment_id == entry.id));
        } else {
            assert!(matches!(request, ClientMessage::SpawnPane {
                workspace_id: id, role: PaneRole::Runner, command: Some(argv), cols: 80, rows: 24, ..
            } if id == workspace_id && argv == entry.argv));
        }
        task.abort();
    }
}
