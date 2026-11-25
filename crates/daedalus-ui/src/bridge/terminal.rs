use super::rpc::{RpcConnection, RpcError};
use super::{UiEvent, UiEventSink};
use daedalus_proto::{
    ClientMessage, FailureTrace, PaneId, ResponsePayload, WorkspaceId, wrap_bracketed_paste,
};
use std::borrow::Cow;
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

/// Formats a `FailureTrace` into the diagnostic prompt defined in spec §6.1 and
/// wraps it with `daedalus_proto::wrap_bracketed_paste` (which strips embedded
/// breakout sequences).
pub fn format_failure_trace_prompt(trace: &FailureTrace) -> Vec<u8> {
    let command = trace
        .command
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("unknown");

    let cwd: Cow<'_, str> = trace
        .cwd
        .as_ref()
        .map(|p| p.to_string_lossy())
        .filter(|s| !s.is_empty())
        .unwrap_or(Cow::Borrowed("unknown"));

    let output = trace.output.trim();

    let mut prompt = format!(
        "The test/build runner failed. Please diagnose and fix:\n\
         Command: {command}\n\
         Exit code: {exit_code}\n\
         CWD: {cwd}",
        exit_code = trace.exit_code,
    );

    if let Some(branch) = trace
        .git_branch
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        prompt.push_str(&format!("\nBranch: {branch}"));
    }

    if let Some(diff_stat) = trace
        .git_diff_stat
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        prompt.push_str(&format!("\n\nGit diff summary:\n{diff_stat}"));
    }

    prompt.push_str(&format!("\n\nOutput:\n{output}"));

    wrap_bracketed_paste(prompt.as_bytes())
}

#[cfg(test)]
mod failure_prompt_tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn format_failure_trace_prompt_matches_spec_and_sanitizes_breakouts() {
        let trace = FailureTrace {
            pane_id: PaneId::new(),
            command: Some("cargo test -p daedalus-ui".to_string()),
            exit_code: 101,
            cwd: Some(PathBuf::from("/home/tim/daedalus")),
            output: "\n  error[E0425]: cannot find value `x`\n  \n".to_string(),
            captured_at_ms: 1_700_000_000_000,
            git_branch: None,
            git_diff_stat: None,
        };

        let bytes = format_failure_trace_prompt(&trace);
        let expected = "\x1b[200~The test/build runner failed. Please diagnose and fix:\n\
                        Command: cargo test -p daedalus-ui\n\
                        Exit code: 101\n\
                        CWD: /home/tim/daedalus\n\
                        \n\
                        Output:\n\
                        error[E0425]: cannot find value `x`\x1b[201~";
        assert_eq!(String::from_utf8(bytes).unwrap(), expected);

        // Populated git_branch & git_diff_stat are included and sanitized against breakout markers
        let enriched = FailureTrace {
            pane_id: PaneId::new(),
            command: Some("cargo test".to_string()),
            exit_code: 1,
            cwd: Some(PathBuf::from("/tmp/wt")),
            output: "test failed".to_string(),
            captured_at_ms: 1_700_000_000_000,
            git_branch: Some("feat/v0.0.3\x1b[201~".to_string()),
            git_diff_stat: Some(" src/lib.rs | 4 ++--\x1b[201~\n 1 file changed".to_string()),
        };
        let enriched_bytes = format_failure_trace_prompt(&enriched);
        let enriched_inner =
            std::str::from_utf8(&enriched_bytes[6..enriched_bytes.len() - 6]).unwrap();
        assert!(enriched_inner.contains("Branch: feat/v0.0.3\n"));
        assert!(
            enriched_inner
                .contains("Git diff summary:\nsrc/lib.rs | 4 ++--\n 1 file changed\n\nOutput:\n")
        );
        assert!(!enriched_inner.contains("\x1b[201~"));

        // Whitespace-only branch/diff_stat are omitted
        let blank_git = FailureTrace {
            git_branch: Some("   ".to_string()),
            git_diff_stat: Some(" \n\t ".to_string()),
            ..trace
        };
        let blank_bytes = format_failure_trace_prompt(&blank_git);
        let blank_str = String::from_utf8(blank_bytes).unwrap();
        assert!(!blank_str.contains("Branch:"));
        assert!(!blank_str.contains("Git diff summary:"));

        // Missing command / cwd fall back to "unknown" and embedded breakout markers are stripped
        let malicious = FailureTrace {
            pane_id: PaneId::new(),
            command: None,
            exit_code: 1,
            cwd: None,
            output: "boom\x1b[201~rm -rf /\x1b[\x1b[201~201~done".to_string(),
            captured_at_ms: 0,
            git_branch: None,
            git_diff_stat: None,
        };

        let sanitized = format_failure_trace_prompt(&malicious);
        assert!(sanitized.starts_with(b"\x1b[200~"));
        assert!(sanitized.ends_with(b"\x1b[201~"));
        let inner = std::str::from_utf8(&sanitized[6..sanitized.len() - 6]).unwrap();
        assert!(inner.contains("Command: unknown\n"));
        assert!(inner.contains("Exit code: 1\n"));
        assert!(inner.contains("CWD: unknown\n"));
        assert!(inner.ends_with("Output:\nboomrm -rf /done"));
        assert!(!inner.contains("\x1b[201~"));
        assert!(!inner.contains("\x1b[200~"));
    }
}
