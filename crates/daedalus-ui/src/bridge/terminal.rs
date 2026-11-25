use super::rpc::{RpcConnection, RpcError};
use super::{UiEvent, UiEventSink};
use crate::input::format_failure_trace_prompt;
use daedalus_proto::{ClientMessage, PaneId, ResponsePayload, WorkspaceId};
use std::sync::Arc;
use tracing::warn;

pub(super) async fn preview_image(
    conn: &RpcConnection,
    sink: &Arc<dyn UiEventSink>,
    workspace_id: WorkspaceId,
    pane_id: PaneId,
    path: String,
) -> Result<(), RpcError> {
    if !conn.image_preview_supported {
        sink.emit(UiEvent::Toast {
            message: "This daemon does not support workspace image previews".into(),
            is_error: true,
        });
    } else {
        match conn
            .call(ClientMessage::PreviewImage { workspace_id, path })
            .await?
        {
            ResponsePayload::ImagePreview(image) if image.valid() => {
                sink.emit(UiEvent::ImagePreview { pane_id, image })
            }
            _ => sink.emit(UiEvent::Toast {
                message: "Invalid preview received".into(),
                is_error: true,
            }),
        }
    }
    Ok(())
}

pub(super) async fn snapshot(
    conn: &RpcConnection,
    sink: &Arc<dyn UiEventSink>,
    pane_id: PaneId,
) -> Result<(), RpcError> {
    if let ResponsePayload::Snapshot(snapshot) =
        conn.call(ClientMessage::GetSnapshot { pane_id }).await?
    {
        sink.emit(UiEvent::Snapshot(snapshot));
    }
    Ok(())
}

pub(super) async fn scrollback(
    conn: &RpcConnection,
    sink: &Arc<dyn UiEventSink>,
    pane_id: PaneId,
    generation: u64,
    offset_from_bottom: usize,
    count: usize,
) -> Result<(), RpcError> {
    if let ResponsePayload::ScrollbackRows {
        pane_id,
        total_scrollback,
        rows,
    } = conn
        .call(ClientMessage::GetScrollback {
            pane_id,
            offset_from_bottom,
            count,
        })
        .await?
    {
        sink.emit(UiEvent::ScrollbackLoaded {
            pane_id,
            generation,
            total_scrollback,
            rows,
        });
    }
    Ok(())
}

pub(super) async fn search(
    conn: &RpcConnection,
    sink: &Arc<dyn UiEventSink>,
    pane_id: PaneId,
    query: String,
    case_sensitive: bool,
    generation: u64,
) -> Result<(), RpcError> {
    match conn
        .call(ClientMessage::SearchPane {
            pane_id,
            query,
            case_sensitive,
        })
        .await?
    {
        ResponsePayload::SearchMatches {
            pane_id,
            total_scrollback,
            matches,
        } => {
            sink.emit(UiEvent::SearchResults {
                pane_id,
                generation,
                total_scrollback,
                matches,
            });
        }
        other => {
            warn!(response = ?other, "unexpected response to SearchPane");
        }
    }
    Ok(())
}

pub(super) async fn forward_failure(
    conn: &RpcConnection,
    sink: &Arc<dyn UiEventSink>,
    runner_pane_id: PaneId,
    agent_pane_id: PaneId,
) -> Result<(), RpcError> {
    match conn
        .call(ClientMessage::GetFailureTrace {
            pane_id: runner_pane_id,
        })
        .await?
    {
        ResponsePayload::FailureTrace(Some(trace)) => {
            let exit_code = trace.exit_code;
            let data = format_failure_trace_prompt(&trace);
            let _ = conn
                .call(ClientMessage::SendInput {
                    pane_id: agent_pane_id,
                    data,
                })
                .await?;
            sink.emit(UiEvent::Toast {
                message: format!("Sent failure trace (exit {exit_code}) to Agent"),
                is_error: false,
            });
        }
        ResponsePayload::FailureTrace(None) => {
            sink.emit(UiEvent::Toast {
                message: "No failure trace captured in Runner pane yet".to_string(),
                is_error: true,
            });
        }
        other => {
            warn!(response = ?other, "unexpected response to GetFailureTrace");
        }
    }
    Ok(())
}

pub(super) async fn paste(
    conn: &RpcConnection,
    pane_id: PaneId,
    data: Vec<u8>,
) -> Result<(), RpcError> {
    let request = if conn.paste_supported {
        ClientMessage::Paste { pane_id, data }
    } else {
        ClientMessage::SendInput {
            pane_id,
            data: daedalus_proto::wrap_bracketed_paste(&data),
        }
    };
    conn.call(request).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{NoopSink, ipc_pair};
    use super::*;
    use daedalus_proto::{Request, ServerMessage, read_frame, write_frame};

    #[tokio::test]
    async fn paste_uses_typed_input_with_a_capable_peer_and_legacy_framing_otherwise() {
        let pane_id = PaneId::new();
        for supported in [false, true] {
            let (client, mut peer) = ipc_pair().await;
            let mut conn = RpcConnection::new(client, Arc::new(NoopSink));
            conn.paste_supported = supported;

            let (response, ()) =
                tokio::join!(paste(&conn, pane_id, b"selected text".to_vec()), async {
                    let request: Request = read_frame(&mut peer).await.unwrap().unwrap();
                    let expected = if supported {
                        ClientMessage::Paste {
                            pane_id,
                            data: b"selected text".to_vec(),
                        }
                    } else {
                        ClientMessage::SendInput {
                            pane_id,
                            data: b"\x1b[200~selected text\x1b[201~".to_vec(),
                        }
                    };
                    assert_eq!(request.payload, expected);
                    write_frame(
                        &mut peer,
                        &ServerMessage::Response {
                            id: request.id,
                            result: Ok(ResponsePayload::Ok),
                        },
                    )
                    .await
                    .unwrap();
                });
            response.unwrap();
            conn.close().await;
        }
    }
}
