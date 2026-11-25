use super::super::rpc::RpcConnection;
use super::super::test_support::ipc_pair;
use super::super::{WorkbenchOperation, session_loop};
use super::*;
use daedalus_proto::transport::IpcListener;
use daedalus_proto::{PaneId, ProtoError, WorkspaceId};
use std::future::Future;
use std::path::PathBuf;
use tokio::sync::mpsc;

struct Sink(mpsc::UnboundedSender<UiEvent>);

impl UiEventSink for Sink {
    fn emit(&self, event: UiEvent) {
        self.0.send(event).unwrap();
    }
}

fn ui() -> (Arc<dyn UiEventSink>, mpsc::UnboundedReceiver<UiEvent>) {
    let (sender, events) = mpsc::unbounded_channel();
    (Arc::new(Sink(sender)), events)
}

async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect("test operation stalled")
}

async fn listener() -> (IpcListener, PathBuf) {
    let listener = IpcListener::bind(Path::new("tcp://127.0.0.1:0"))
        .await
        .unwrap();
    let endpoint = listener.local_endpoint();
    (listener, endpoint)
}

async fn next_request(stream: &mut IpcStream) -> Request {
    bounded(read_frame(stream)).await.unwrap().unwrap()
}

async fn reply(stream: &mut IpcStream, id: u64, result: Result<ResponsePayload, ProtoError>) {
    bounded(write_frame(stream, &ServerMessage::Response { id, result }))
        .await
        .unwrap();
}

fn command(request: ClientMessage, workspace_id: WorkspaceId) -> UiCommand {
    UiCommand::Workbench {
        operation: WorkbenchOperation::Read,
        workspace_id,
        request: Box::new(request),
    }
}

fn results(workspace_id: WorkspaceId, refresh: Option<&str>) -> ClientMessage {
    ClientMessage::GetWidgetResults {
        workspace_id,
        refresh: refresh.map(str::to_owned),
    }
}

fn action(workspace_id: WorkspaceId) -> ClientMessage {
    ClientMessage::InvokeWidgetAction {
        workspace_id,
        widget: "details".into(),
        action: "show".into(),
    }
}

fn expect_toast(events: &mut mpsc::UnboundedReceiver<UiEvent>, expected: &str) {
    assert!(matches!(
        events.try_recv().unwrap(),
        UiEvent::Toast { message, is_error: false } if message == expected
    ));
}

#[tokio::test]
async fn manual_refresh_supersedes_polling_and_rejects_another_manual_job() {
    let (mut listener, endpoint) = listener().await;
    let (sink, mut events) = ui();
    let mut lane = WidgetLane::new(&endpoint, None, &sink);
    let id = WorkspaceId::new();
    let poll = results(id, None);
    let refresh = results(id, Some("build"));

    assert!(lane.dispatch(&command(poll.clone(), id)));
    let mut poll_peer = bounded(listener.accept()).await.unwrap();
    assert_eq!(next_request(&mut poll_peer).await.payload, poll);
    assert!(lane.dispatch(&command(poll.clone(), id)));
    assert!(events.try_recv().is_err());

    assert!(lane.dispatch(&command(refresh.clone(), id)));
    expect_toast(&mut events, "Refreshing widget…");
    assert!(
        bounded(read_frame::<_, Request>(&mut poll_peer))
            .await
            .unwrap()
            .is_none()
    );
    let mut refresh_peer = bounded(listener.accept()).await.unwrap();
    let request = next_request(&mut refresh_peer).await;
    assert_eq!(request.payload, refresh);

    assert!(lane.dispatch(&command(poll.clone(), id)));
    assert!(events.try_recv().is_err());
    assert!(lane.dispatch(&command(action(id), id)));
    expect_toast(&mut events, "A widget operation is already running.");

    let payload = ResponsePayload::WidgetResults(vec![]);
    reply(&mut refresh_peer, request.id, Ok(payload.clone())).await;
    bounded(lane.join_next()).await;
    assert!(!lane.has_jobs());
    assert!(matches!(
        events.try_recv().unwrap(),
        UiEvent::Workbench { workspace_id, response, .. }
            if workspace_id == id && *response == Ok(payload)
    ));
    assert!(events.try_recv().is_err());

    // A skipped poll doesn't prevent the next scheduled observation from running.
    assert!(lane.dispatch(&command(poll.clone(), id)));
    let mut renewed_peer = bounded(listener.accept()).await.unwrap();
    let request = next_request(&mut renewed_peer).await;
    assert_eq!(request.payload, poll);
    reply(
        &mut renewed_peer,
        request.id,
        Ok(ResponsePayload::WidgetResults(vec![])),
    )
    .await;
    bounded(lane.join_next()).await;
    assert!(matches!(
        events.try_recv().unwrap(),
        UiEvent::Workbench { response, .. }
            if matches!(*response, Ok(ResponsePayload::WidgetResults(_)))
    ));
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn failed_poll_is_silent_but_unanswered_actions_report_failure() {
    let (mut listener, endpoint) = listener().await;
    let (sink, mut events) = ui();
    let mut lane = WidgetLane::new(&endpoint, None, &sink);
    let id = WorkspaceId::new();

    for manual in [false, true] {
        let expected = if manual {
            action(id)
        } else {
            results(id, None)
        };
        assert!(lane.dispatch(&command(expected.clone(), id)));
        if manual {
            expect_toast(&mut events, "Running widget action…");
        }
        let mut peer = bounded(listener.accept()).await.unwrap();
        assert_eq!(next_request(&mut peer).await.payload, expected);
        drop(peer);
        bounded(lane.join_next()).await;
        if manual {
            assert!(matches!(
                events.try_recv().unwrap(),
                UiEvent::Workbench { workspace_id, response, .. }
                    if workspace_id == id
                        && response.as_ref().as_ref().unwrap_err().contains("closed before reporting")
            ));
        }
        assert!(events.try_recv().is_err());
    }
}

#[tokio::test]
async fn widget_socket_surfaces_authentication_and_protocol_failures() {
    let (mut listener, endpoint) = listener().await;
    for (response, expected) in [
        (
            Err(ProtoError::Unauthorized("rejected".into())),
            "unauthorized: rejected",
        ),
        (
            Ok(ResponsePayload::WidgetResults(vec![])),
            "Unexpected authentication response",
        ),
    ] {
        let request = action(WorkspaceId::new());
        let exchange = async {
            let mut peer = listener.accept().await.unwrap();
            let auth = next_request(&mut peer).await;
            assert_eq!(
                auth.payload,
                ClientMessage::Authenticate {
                    token: "fixture".into()
                }
            );
            reply(&mut peer, auth.id, response).await;
            assert!(read_frame::<_, Request>(&mut peer).await.unwrap().is_none());
        };
        let (result, ()) = bounded(async {
            tokio::join!(run(&endpoint, Some("fixture".into()), request), exchange)
        })
        .await;
        assert!(result.unwrap_err().contains(expected));
    }

    let exchange = async {
        let mut peer = listener.accept().await.unwrap();
        let request = next_request(&mut peer).await;
        reply(&mut peer, request.id + 1, Ok(ResponsePayload::Ok)).await;
    };
    let (result, ()) =
        bounded(async { tokio::join!(run(&endpoint, None, action(WorkspaceId::new())), exchange) })
            .await;
    assert_eq!(
        result.unwrap_err(),
        "Unexpected reply on the widget connection."
    );

    // The TCP scheme without a socket address fails before a request can be sent.
    assert!(
        bounded(run(Path::new("tcp://"), None, action(WorkspaceId::new())))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn primary_disconnect_drops_the_widget_connection_without_reporting_a_result() {
    let (mut listener, endpoint) = listener().await;
    let (stream, mut primary) = ipc_pair().await;
    let (sink, mut events) = ui();
    let mut conn = RpcConnection::new(stream, Arc::clone(&sink));
    conn.workbench_supported = true;
    let (sender, mut commands) = mpsc::unbounded_channel();
    let id = WorkspaceId::new();
    let request = action(id);
    sender.send(command(request.clone(), id)).unwrap();
    let peer = async {
        let mut widget = listener.accept().await.unwrap();
        assert_eq!(next_request(&mut widget).await.payload, request);

        let pane = PaneId::new();
        sender
            .send(UiCommand::SendInput {
                pane_id: pane,
                data: b"x".to_vec(),
            })
            .unwrap();
        let typed = next_request(&mut primary).await;
        assert_eq!(
            typed.payload,
            ClientMessage::SendInput {
                pane_id: pane,
                data: b"x".to_vec()
            }
        );
        reply(&mut primary, typed.id, Ok(ResponsePayload::Ok)).await;

        drop(primary);
        assert!(
            read_frame::<_, Request>(&mut widget)
                .await
                .unwrap()
                .is_none()
        );
    };
    bounded(async {
        tokio::join!(
            session_loop(&mut conn, &sink, &mut commands, &endpoint, None),
            peer,
        )
    })
    .await;
    expect_toast(&mut events, "Running widget action…");
    assert!(events.try_recv().is_err());
    conn.close().await;
}
