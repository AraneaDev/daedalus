pub mod codec;
pub mod rpc;
pub mod types;

pub use codec::{CodecError, MAX_FRAME_SIZE, read_frame, write_frame};
pub use rpc::{ClientMessage, ProtoError, Request, ResponsePayload, ServerEvent, ServerMessage};
pub use types::{
    Cell, CellFlags, Color, ContainerHealth, CursorState, ExternalStatus, FailureTrace,
    GitWorktreeInfo, GridDelta, GridRow, GridSnapshot, PaneId, PaneRole, PaneStatus, PaneSummary,
    PortMapping, PortSource, StatusLevel, WorkspaceId, WorkspaceInfo, wrap_bracketed_paste,
};

use std::path::PathBuf;

fn resolve_runtime_dir(xdg: Option<&str>) -> PathBuf {
    if let Some(xdg) = xdg
        && !xdg.is_empty()
    {
        return PathBuf::from(xdg).join("daedalus");
    }
    std::env::temp_dir().join(format!("daedalus-{}", nix::unistd::Uid::current()))
}

pub fn default_runtime_dir() -> PathBuf {
    resolve_runtime_dir(std::env::var("XDG_RUNTIME_DIR").ok().as_deref())
}

pub fn default_socket_path() -> PathBuf {
    if let Ok(sock) = std::env::var("DAEDALUS_SOCKET")
        && !sock.is_empty()
    {
        return PathBuf::from(sock);
    }
    default_runtime_dir().join("daedalusd.sock")
}

fn resolve_events_socket_path(env_override: Option<&str>, xdg: Option<&str>) -> PathBuf {
    if let Some(sock) = env_override
        && !sock.is_empty()
    {
        return PathBuf::from(sock);
    }
    resolve_runtime_dir(xdg).join("events.sock")
}

pub fn default_events_socket_path() -> PathBuf {
    resolve_events_socket_path(
        std::env::var("DAEDALUS_EVENTS_SOCKET").ok().as_deref(),
        std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tokio::io::duplex;

    #[tokio::test]
    async fn round_trips_requests_responses_and_events() {
        let (mut client, mut server) = duplex(8192);
        let ws_id = WorkspaceId::new();
        let pane_id = PaneId::new();

        let req = Request {
            id: 7,
            payload: ClientMessage::SpawnPane {
                workspace_id: ws_id,
                role: PaneRole::Agent,
                command: Some(vec!["agy".to_string()]),
                shell: None,
                cols: 120,
                rows: 40,
                env: HashMap::from([("FOO".to_string(), "bar".to_string())]),
            },
        };
        write_frame(&mut client, &req).await.unwrap();
        let decoded: Request = read_frame(&mut server).await.unwrap().unwrap();
        assert_eq!(decoded, req);

        let evt = ServerMessage::Event(ServerEvent::CommandFinished {
            pane_id,
            command: Some("cargo test".to_string()),
            exit_code: 101,
            has_failure_trace: true,
        });
        write_frame(&mut server, &evt).await.unwrap();
        let decoded_evt: ServerMessage = read_frame(&mut client).await.unwrap().unwrap();
        assert_eq!(decoded_evt, evt);
    }

    #[tokio::test]
    async fn rejects_oversized_frame_header() {
        let (mut tx, mut rx) = duplex(64);
        use tokio::io::AsyncWriteExt;
        let bad_len = ((MAX_FRAME_SIZE + 1) as u32).to_be_bytes();
        tx.write_all(&bad_len).await.unwrap();
        tx.flush().await.unwrap();

        let res: Result<Option<Request>, CodecError> = read_frame(&mut rx).await;
        assert!(matches!(res, Err(CodecError::FrameTooLarge(_))));
    }

    #[tokio::test]
    async fn returns_none_on_clean_eof() {
        let (tx, mut rx) = duplex(64);
        drop(tx);
        let res: Result<Option<Request>, CodecError> = read_frame(&mut rx).await;
        assert!(matches!(res, Ok(None)));
    }

    #[test]
    fn wraps_bracketed_paste() {
        let wrapped = wrap_bracketed_paste(b"line1\nline2");
        assert_eq!(wrapped, b"\x1b[200~line1\nline2\x1b[201~");
    }

    #[test]
    fn strips_embedded_and_nested_bracketed_paste_breakout_sequences() {
        // Embedded 7-bit end marker
        assert_eq!(
            wrap_bracketed_paste(b"safe\x1b[201~rm -rf /"),
            b"\x1b[200~saferm -rf /\x1b[201~"
        );
        // Embedded 7-bit start marker
        assert_eq!(
            wrap_bracketed_paste(b"a\x1b[200~b"),
            b"\x1b[200~ab\x1b[201~"
        );
        // Embedded 8-bit CSI markers
        assert_eq!(
            wrap_bracketed_paste(b"a\x9b201~b\x9b200~c"),
            b"\x1b[200~abc\x1b[201~"
        );
        // Nested breakout attempt that would reconstruct \x1b[201~ after a single pass
        assert_eq!(
            wrap_bracketed_paste(b"before\x1b[\x1b[201~201~after"),
            b"\x1b[200~beforeafter\x1b[201~"
        );
        // Mixed 8-bit + 7-bit nested breakout attempt
        assert_eq!(
            wrap_bracketed_paste(b"x\x9b\x1b[200~201~y"),
            b"\x1b[200~xy\x1b[201~"
        );
    }

    #[test]
    fn runtime_dir_uses_xdg_or_uid_fallback() {
        assert_eq!(
            resolve_runtime_dir(Some("/run/user/1234")),
            PathBuf::from("/run/user/1234/daedalus")
        );
        let expected_fallback =
            std::env::temp_dir().join(format!("daedalus-{}", nix::unistd::Uid::current()));
        assert_eq!(resolve_runtime_dir(None), expected_fallback);
        assert_eq!(resolve_runtime_dir(Some("")), expected_fallback);
    }

    #[test]
    fn events_socket_path_uses_override_or_runtime_dir() {
        assert_eq!(
            resolve_events_socket_path(Some("/tmp/custom-events.sock"), Some("/run/user/1000")),
            PathBuf::from("/tmp/custom-events.sock")
        );
        assert_eq!(
            resolve_events_socket_path(Some(""), Some("/run/user/1000")),
            PathBuf::from("/run/user/1000/daedalus/events.sock")
        );
        assert_eq!(
            resolve_events_socket_path(None, Some("/run/user/1000")),
            PathBuf::from("/run/user/1000/daedalus/events.sock")
        );
    }

    #[tokio::test]
    async fn round_trips_phase3_types_rpcs_and_events() {
        let (mut client, mut server) = duplex(16384);
        let ws_id = WorkspaceId::new();

        let requests = vec![
            Request {
                id: 1,
                payload: ClientMessage::DiscoverWorktrees {
                    repo_dir: PathBuf::from("/home/tim/daedalus"),
                },
            },
            Request {
                id: 2,
                payload: ClientMessage::CreateWorktreeWorkspace {
                    repo_dir: PathBuf::from("/home/tim/daedalus"),
                    branch: "feat/auth".to_string(),
                    create_branch: true,
                    env: HashMap::from([("RUST_LOG".to_string(), "debug".to_string())]),
                },
            },
            Request {
                id: 3,
                payload: ClientMessage::CloseWorkspace {
                    id: ws_id,
                    compose_down: true,
                },
            },
            Request {
                id: 4,
                payload: ClientMessage::PublishExternalStatus {
                    workspace_id: Some(ws_id),
                    root_dir: Some(PathBuf::from("/home/tim/daedalus")),
                    source: "phrourion".to_string(),
                    level: StatusLevel::Warn,
                    label: "3 dirty".to_string(),
                    detail: Some("2 modified, 1 untracked".to_string()),
                },
            },
        ];

        for req in requests {
            write_frame(&mut client, &req).await.unwrap();
            let decoded: Request = read_frame(&mut server).await.unwrap().unwrap();
            assert_eq!(decoded, req);
        }

        let worktrees = vec![
            GitWorktreeInfo {
                path: PathBuf::from("/home/tim/daedalus"),
                branch: Some("master".to_string()),
                head: "15f8202deadbeef".to_string(),
                is_primary: true,
                is_detached: false,
            },
            GitWorktreeInfo {
                path: PathBuf::from("/home/tim/.local/share/daedalus/worktrees/daedalus/feat-auth"),
                branch: None,
                head: "abcdef123456".to_string(),
                is_primary: false,
                is_detached: true,
            },
        ];

        let port_mapping = PortMapping {
            host_port: 5532,
            container_port: Some(5432),
            service: Some("postgres".to_string()),
            source: PortSource::Docker,
        };
        let local_port = PortMapping {
            host_port: 3100,
            container_port: None,
            service: None,
            source: PortSource::LocalProcess,
        };
        let container = ContainerHealth {
            name: "daedalus-wt1-postgres-1".to_string(),
            service: Some("postgres".to_string()),
            state: "running".to_string(),
            health: Some("healthy".to_string()),
            ports: vec![5532],
        };
        let ext_status = ExternalStatus {
            source: "cassandra".to_string(),
            level: StatusLevel::Error,
            label: "retry 3/5".to_string(),
            detail: Some("rate limited".to_string()),
            updated_at_ms: 1_760_000_000_123,
        };

        let ws_info = WorkspaceInfo {
            id: ws_id,
            name: "feat-auth".to_string(),
            root_dir: PathBuf::from("/tmp/wt1"),
            slot_index: 1,
            injected_env: HashMap::from([("PORT".to_string(), "3100".to_string())]),
            panes: Vec::new(),
            port_offset: 100,
            compose_project_name: Some("daedalus-wt1".to_string()),
            git_branch: Some("feat/auth".to_string()),
            bound_ports: vec![port_mapping.clone(), local_port.clone()],
            containers: vec![container.clone()],
            external_statuses: vec![ext_status.clone()],
        };

        let server_msgs = vec![
            ServerMessage::Response {
                id: 1,
                result: Ok(ResponsePayload::Worktrees(worktrees)),
            },
            ServerMessage::Response {
                id: 2,
                result: Ok(ResponsePayload::WorkspaceCreated(ws_info)),
            },
            ServerMessage::Event(ServerEvent::WorkspaceRuntimeUpdated {
                workspace_id: ws_id,
                bound_ports: vec![port_mapping, local_port],
                containers: vec![container],
            }),
            ServerMessage::Event(ServerEvent::ExternalStatusUpdated {
                workspace_id: ws_id,
                status: ext_status,
            }),
        ];

        for msg in server_msgs {
            write_frame(&mut server, &msg).await.unwrap();
            let decoded: ServerMessage = read_frame(&mut client).await.unwrap().unwrap();
            assert_eq!(decoded, msg);
        }
    }

    #[test]
    fn serde_defaults_preserve_backward_compatibility_for_workspace_info_and_close_workspace() {
        #[derive(serde::Serialize)]
        struct LegacyWorkspaceInfo {
            id: WorkspaceId,
            name: String,
            root_dir: PathBuf,
            slot_index: u16,
            injected_env: HashMap<String, String>,
            panes: Vec<PaneSummary>,
        }

        #[derive(serde::Serialize)]
        enum LegacyClientMessage {
            CloseWorkspace { id: WorkspaceId },
        }

        let ws_id = WorkspaceId::new();
        let legacy_ws = LegacyWorkspaceInfo {
            id: ws_id,
            name: "legacy".to_string(),
            root_dir: PathBuf::from("/tmp/legacy"),
            slot_index: 0,
            injected_env: HashMap::new(),
            panes: Vec::new(),
        };
        let bytes = rmp_serde::to_vec_named(&legacy_ws).unwrap();
        let decoded_ws: WorkspaceInfo = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(decoded_ws.id, ws_id);
        assert_eq!(decoded_ws.port_offset, 0);
        assert_eq!(decoded_ws.compose_project_name, None);
        assert_eq!(decoded_ws.git_branch, None);
        assert!(decoded_ws.bound_ports.is_empty());
        assert!(decoded_ws.containers.is_empty());
        assert!(decoded_ws.external_statuses.is_empty());

        let legacy_close = LegacyClientMessage::CloseWorkspace { id: ws_id };
        let close_bytes = rmp_serde::to_vec_named(&legacy_close).unwrap();
        let decoded_close: ClientMessage = rmp_serde::from_slice(&close_bytes).unwrap();
        assert_eq!(
            decoded_close,
            ClientMessage::CloseWorkspace {
                id: ws_id,
                compose_down: false,
            }
        );
    }
}
