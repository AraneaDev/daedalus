pub mod codec;
pub mod rpc;
pub mod types;

pub use codec::{read_frame, write_frame, CodecError, MAX_FRAME_SIZE};
pub use rpc::{ClientMessage, ProtoError, Request, ResponsePayload, ServerEvent, ServerMessage};
pub use types::{
    wrap_bracketed_paste, Cell, CellFlags, Color, CursorState, FailureTrace, GridDelta, GridRow,
    GridSnapshot, PaneId, PaneRole, PaneStatus, PaneSummary, WorkspaceId, WorkspaceInfo,
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
}
