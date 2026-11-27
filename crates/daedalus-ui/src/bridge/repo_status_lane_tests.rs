use super::test_support::ipc_pair;
use super::*;
use daedalus_proto::transport::{IpcListener, IpcStream};
use daedalus_proto::{Request, ServerMessage, read_frame, write_frame};
use std::path::Path;

struct Sink(mpsc::UnboundedSender<UiEvent>);

impl UiEventSink for Sink {
    fn emit(&self, event: UiEvent) {
        self.0.send(event).unwrap();
    }
}

fn repo_check(workspace_id: WorkspaceId) -> UiCommand {
    UiCommand::Workbench {
        workspace_id,
        operation: WorkbenchOperation::Read,
        request: Box::new(ClientMessage::CheckWorkspaceRepo { workspace_id }),
    }
}

async fn next_request(peer: &mut IpcStream) -> Request {
    read_frame(peer).await.unwrap().unwrap()
}

async fn reply(peer: &mut IpcStream, id: u64, response: ResponsePayload) {
    write_frame(
        peer,
        &ServerMessage::Response {
            id,
            result: Ok(response),
        },
    )
    .await
    .unwrap();
}

async fn authenticate(peer: &mut IpcStream) {
    let request = next_request(peer).await;
    assert_eq!(
        request.payload,
        ClientMessage::Authenticate {
            token: "repo-status-fixture".into(),
        }
    );
    reply(peer, request.id, ResponsePayload::Ok).await;
}

async fn expect_input(
    commands: &mpsc::UnboundedSender<UiCommand>,
    primary: &mut IpcStream,
    pane_id: PaneId,
    input: u8,
) {
    commands
        .send(UiCommand::SendInput {
            pane_id,
            data: vec![input],
        })
        .unwrap();
    let request = next_request(primary).await;
    assert_eq!(
        request.payload,
        ClientMessage::SendInput {
            pane_id,
            data: vec![input],
        }
    );
    reply(primary, request.id, ResponsePayload::Ok).await;
}

#[tokio::test]
async fn repo_status_timeout_does_not_block_input_and_rechecks_only_the_latest_workspace() {
    let (stream, mut primary) = ipc_pair().await;
    let mut listener = IpcListener::bind(Path::new("tcp://127.0.0.1:0"))
        .await
        .unwrap();
    let endpoint = listener.local_endpoint();
    let (event_tx, mut events) = mpsc::unbounded_channel();
    let sink: Arc<dyn UiEventSink> = Arc::new(Sink(event_tx));
    let mut conn = RpcConnection::new(stream, Arc::clone(&sink));
    conn.workbench_supported = true;
    let (commands, mut command_rx) = mpsc::unbounded_channel();
    let first = WorkspaceId::new();
    let latest = WorkspaceId::new();
    commands.send(repo_check(first)).unwrap();

    let peer = async {
        let mut repo = tokio::select! {
            repo = listener.accept() => repo.unwrap(),
            request = read_frame::<_, Request>(&mut primary) => {
                panic!("repo status used the terminal connection: {request:?}");
            }
        };
        authenticate(&mut repo).await;
        assert_eq!(
            next_request(&mut repo).await.payload,
            ClientMessage::CheckWorkspaceRepo {
                workspace_id: first
            }
        );

        commands.send(repo_check(WorkspaceId::new())).unwrap();
        commands.send(repo_check(latest)).unwrap();
        commands.send(repo_check(latest)).unwrap();
        let pane_id = PaneId::new();
        expect_input(&commands, &mut primary, pane_id, b'x').await;

        // An outstanding check holds one slot and the pending slot holds only
        // the latest workspace. Leave the outstanding peer unanswered.
        tokio::select! {
            biased;
            frame = read_frame::<_, Request>(&mut repo) => assert!(frame.unwrap().is_none()),
            stream = listener.accept() => panic!("second repo check started while one was pending: {stream:?}"),
        }
        let mut pending = listener.accept().await.unwrap();
        authenticate(&mut pending).await;
        let request = next_request(&mut pending).await;
        assert_eq!(
            request.payload,
            ClientMessage::CheckWorkspaceRepo {
                workspace_id: latest,
            }
        );
        reply(
            &mut pending,
            request.id,
            ResponsePayload::WorkspaceRepoStatus(Some(true)),
        )
        .await;
        assert!(matches!(
            events.recv().await.unwrap(),
            UiEvent::Workbench { workspace_id, operation: WorkbenchOperation::Read, response }
                if workspace_id == latest
                    && *response == Ok(ResponsePayload::WorkspaceRepoStatus(Some(true)))
        ));
        assert!(events.try_recv().is_err());

        expect_input(&commands, &mut primary, pane_id, b'y').await;
        commands.send(UiCommand::Shutdown).unwrap();
    };

    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(
            session_loop(
                &mut conn,
                &sink,
                &mut command_rx,
                &endpoint,
                Some("repo-status-fixture"),
            ),
            peer,
        )
    })
    .await
    .expect("repo status or terminal input stalled");
    conn.close().await;
}

#[tokio::test]
async fn successful_repo_reply_is_ignored_after_a_different_workspace_is_queued() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut listener = IpcListener::bind(Path::new("tcp://127.0.0.1:0"))
            .await
            .unwrap();
        let endpoint = listener.local_endpoint();
        let (event_tx, mut events) = mpsc::unbounded_channel();
        let sink: Arc<dyn UiEventSink> = Arc::new(Sink(event_tx));
        let mut lane = repo_status_lane::RepoStatusLane::new(&endpoint, None, &sink);
        let previous = WorkspaceId::new();
        let current = WorkspaceId::new();
        assert!(lane.dispatch(&repo_check(previous)));
        let mut previous_peer = listener.accept().await.unwrap();
        let previous_request = next_request(&mut previous_peer).await;
        assert_eq!(
            previous_request.payload,
            ClientMessage::CheckWorkspaceRepo {
                workspace_id: previous,
            }
        );

        assert!(lane.dispatch(&repo_check(current)));
        reply(
            &mut previous_peer,
            previous_request.id,
            ResponsePayload::WorkspaceRepoStatus(Some(true)),
        )
        .await;
        lane.join_next().await;
        assert!(events.try_recv().is_err());

        let mut current_peer = listener.accept().await.unwrap();
        let current_request = next_request(&mut current_peer).await;
        assert_eq!(
            current_request.payload,
            ClientMessage::CheckWorkspaceRepo {
                workspace_id: current,
            }
        );
        reply(
            &mut current_peer,
            current_request.id,
            ResponsePayload::WorkspaceRepoStatus(None),
        )
        .await;
        lane.join_next().await;
        assert!(matches!(
            events.try_recv().unwrap(),
            UiEvent::Workbench { workspace_id, response, .. }
                if workspace_id == current && *response == Ok(ResponsePayload::WorkspaceRepoStatus(None))
        ));
        assert!(!lane.has_job());
    })
    .await
    .expect("queued repo status check stalled");
}
