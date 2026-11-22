use daedalus_proto::transport::IpcListener;
use daedalus_proto::{ClientMessage, ProtoError, Request, ServerMessage, read_frame, write_frame};
#[cfg(unix)]
use daedalus_proto::{ResponsePayload, transport::IpcStream};
use std::path::Path;
use tokio::process::Command;

#[tokio::test]
async fn old_daemon_directory_lookup_explains_id_alternative() {
    for disconnect in [false, true] {
        let mut listener = IpcListener::bind(Path::new("tcp://127.0.0.1:0"))
            .await
            .unwrap();
        let endpoint = listener.local_endpoint();
        let alias = std::env::temp_dir().join("alias-only-known-to-daemon");
        let expected = alias.clone();
        let mock = tokio::spawn(async move {
            let mut stream = listener.accept().await.unwrap();
            let req: Request = read_frame(&mut stream).await.unwrap().unwrap();
            assert_eq!(
                req.payload,
                ClientMessage::ResolveWorkspaceDirectory { root_dir: expected }
            );
            if !disconnect {
                write_frame(
                    &mut stream,
                    &ServerMessage::Response {
                        id: req.id,
                        result: Err(ProtoError::InvalidRequest("unsupported request".into())),
                    },
                )
                .await
                .unwrap();
            }
        });
        let output = Command::new(env!("CARGO_BIN_EXE_daedalus-ctl"))
            .arg("--socket")
            .arg(endpoint)
            .args(["action", "run", "build", "--dir"])
            .arg(alias)
            .output()
            .await
            .unwrap();
        mock.await.unwrap();
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("--workspace <ID>"), "{stderr}");
        assert!(stderr.contains("upgrade daedalusd"), "{stderr}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn directory_rpc_resolves_alias_to_deepest_then_lowest_slot_workspace() {
    use daedalusd::{
        server::DaemonServer,
        supervisor::{IsolationMode, Supervisor},
    };
    use tokio::sync::oneshot;

    let dir = tempfile::tempdir().unwrap();
    let outer = dir.path().join("outer");
    let nested = outer.join("nested");
    let leaf = nested.join("leaf");
    std::fs::create_dir_all(&leaf).unwrap();
    let sibling = outer.join("sibling");
    std::fs::create_dir(&sibling).unwrap();
    let alias = dir.path().join("alias");
    std::os::unix::fs::symlink(&nested, &alias).unwrap();
    let server = DaemonServer::bind_with_supervisor(
        dir.path().join("d.sock"),
        dir.path().join("runtime"),
        Supervisor::with_mode(IsolationMode::PgidFallback),
    )
    .await
    .unwrap();
    let endpoint = server.socket_path();
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(async move {
        server
            .serve(async {
                let _ = stopped.await;
            })
            .await
            .unwrap()
    });
    let mut stream = IpcStream::connect(&endpoint).await.unwrap();
    for (id, name, root) in [
        (1, "outer", &outer),
        (2, "first nested", &nested),
        (3, "second nested", &nested),
        (4, "sibling", &sibling),
    ] {
        write_frame(
            &mut stream,
            &Request {
                id,
                payload: ClientMessage::CreateWorkspace {
                    name: name.into(),
                    root_dir: root.clone(),
                    env: Default::default(),
                },
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            read_frame::<_, ServerMessage>(&mut stream).await.unwrap(),
            Some(ServerMessage::Response {
                result: Ok(ResponsePayload::WorkspaceCreated(_)),
                ..
            })
        ));
    }
    for (id, root_dir, expected) in [
        (5, alias.join("leaf"), "first nested"),
        (6, nested.join("../sibling"), "sibling"),
    ] {
        write_frame(
            &mut stream,
            &Request {
                id,
                payload: ClientMessage::ResolveWorkspaceDirectory { root_dir },
            },
        )
        .await
        .unwrap();
        let Some(ServerMessage::Response {
            result: Ok(ResponsePayload::WorkspaceResolved(ws)),
            ..
        }) = read_frame(&mut stream).await.unwrap()
        else {
            panic!("missing resolved workspace")
        };
        assert_eq!(ws.name, expected);
    }
    drop(stream);
    stop.send(()).unwrap();
    task.await.unwrap();
}
