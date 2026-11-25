use crate::client::DaemonClient;
use crate::{expect_ok, unexpected_response};
use daedalus_proto::ResponsePayload;
use daedalus_proto::{ClientMessage, PaneId};
use uuid::Uuid;

pub(crate) async fn failure_trace(
    client: &mut DaemonClient,
    pane_id: Uuid,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    match client
        .request(ClientMessage::GetFailureTrace {
            pane_id: PaneId(pane_id),
        })
        .await?
    {
        ResponsePayload::FailureTrace(trace) => match trace {
            Some(t) => {
                if json {
                    println!("{}", serde_json::to_string_pretty(&t)?);
                } else {
                    println!("pane_id: {}", t.pane_id);
                    println!("exit_code: {}", t.exit_code);
                    println!("command: {}", t.command.as_deref().unwrap_or("<unknown>"));
                    if let Some(cwd) = &t.cwd {
                        println!("cwd: {}", cwd.display());
                    }
                    if let Some(branch) = &t.git_branch {
                        println!("git_branch: {branch}");
                    }
                    if let Some(diff_stat) = &t.git_diff_stat {
                        println!("git_diff_stat:\n{diff_stat}");
                    }
                    println!("output:\n{}", t.output);
                }
            }
            None => {
                if json {
                    println!("null");
                } else {
                    println!("No failure trace captured for pane {pane_id}");
                }
            }
        },
        other => return Err(unexpected_response(other)),
    }
    Ok(())
}

pub(crate) async fn events(
    client: &mut DaemonClient,
    pane: Option<Uuid>,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    expect_ok(
        client
            .request(ClientMessage::Subscribe {
                pane_ids: pane.map(|p| vec![PaneId(p)]),
                project_setup_events: false,
            })
            .await?,
    )?;
    while let Some(evt) = client.next_event().await? {
        if json {
            println!("{}", serde_json::to_string(&evt)?);
        } else {
            println!("{evt:?}");
        }
    }
    Ok(())
}
