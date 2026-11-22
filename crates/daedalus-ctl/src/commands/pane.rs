use crate::{
    PaneAction,
    client::{ClientError, DaemonClient},
    expect_ok, parse_env_pairs, unexpected_response,
};
use daedalus_proto::{ClientMessage, PaneId, ResponsePayload, WorkspaceId, wrap_bracketed_paste};

pub(crate) async fn execute(
    client: &mut DaemonClient,
    action: PaneAction,
) -> Result<(), ClientError> {
    match action {
        PaneAction::Spawn {
            workspace_id,
            role,
            shell,
            cols,
            rows,
            env,
            command,
        } => {
            let env_map = parse_env_pairs(&env)?;
            let cmd = if command.is_empty() {
                None
            } else {
                Some(command)
            };
            match client
                .request(ClientMessage::SpawnPane {
                    workspace_id: WorkspaceId(workspace_id),
                    role: role.into(),
                    command: cmd,
                    shell,
                    cols,
                    rows,
                    env: env_map,
                })
                .await?
            {
                ResponsePayload::PaneSpawned(pane) => println!("{}", pane.id),
                other => return Err(unexpected_response(other)),
            }
        }
        PaneAction::Close { pane_id } => {
            expect_ok(
                client
                    .request(ClientMessage::ClosePane {
                        pane_id: PaneId(pane_id),
                    })
                    .await?,
            )?;
            println!("closed {pane_id}");
        }
        PaneAction::Send {
            pane_id,
            text,
            newline,
            bracketed_paste,
        } => {
            let mut bytes = if bracketed_paste {
                wrap_bracketed_paste(text.as_bytes())
            } else {
                text.into_bytes()
            };
            if newline {
                bytes.push(b'\n');
            }
            expect_ok(
                client
                    .request(ClientMessage::SendInput {
                        pane_id: PaneId(pane_id),
                        data: bytes,
                    })
                    .await?,
            )?;
        }
        PaneAction::Resize {
            pane_id,
            cols,
            rows,
        } => {
            expect_ok(
                client
                    .request(ClientMessage::ResizePane {
                        pane_id: PaneId(pane_id),
                        cols,
                        rows,
                    })
                    .await?,
            )?;
        }
        PaneAction::Snapshot { pane_id } => {
            match client
                .request(ClientMessage::GetSnapshot {
                    pane_id: PaneId(pane_id),
                })
                .await?
            {
                ResponsePayload::Snapshot(snap) => {
                    for row in snap.rows_data {
                        let line: String = row.cells.iter().map(|c| c.text.as_str()).collect();
                        println!("{}", line.trim_end());
                    }
                }
                other => return Err(unexpected_response(other)),
            }
        }
        PaneAction::Trace { pane_id } => {
            match client
                .request(ClientMessage::GetFailureTrace {
                    pane_id: PaneId(pane_id),
                })
                .await?
            {
                ResponsePayload::FailureTrace(trace) => match trace {
                    Some(t) => {
                        println!(
                            "exit_code={} command={}",
                            t.exit_code,
                            t.command.as_deref().unwrap_or("<unknown>")
                        );
                        println!("{}", t.output);
                    }
                    None => println!("no failure trace"),
                },
                other => return Err(unexpected_response(other)),
            }
        }
    }
    Ok(())
}
