use crate::{
    CustomActionCommand,
    client::{ClientError, DaemonClient},
    expect_ok, unexpected_response,
};
use daedalus_proto::{ClientMessage, ProtoError, ResponsePayload, WorkspaceId, WorkspaceInfo};
use std::path::PathBuf;

fn target_directory(dir: Option<PathBuf>, remote: bool) -> std::io::Result<PathBuf> {
    if remote {
        return Ok(dir.unwrap_or_else(|| PathBuf::from(".")));
    }
    match dir {
        // A rooted path such as `/home/dev/project` names a daemon-host directory even on
        // Windows, where it has no drive and so is not `is_absolute`.
        Some(dir) if dir.has_root() => Ok(dir),
        Some(dir) => Ok(std::env::current_dir()?.join(dir)),
        None => std::env::current_dir(),
    }
}

async fn resolve_directory(
    client: &mut DaemonClient,
    dir: Option<PathBuf>,
    remote: bool,
) -> Result<WorkspaceInfo, ClientError> {
    let root_dir = target_directory(dir, remote)?;
    let result = client
        .request(ClientMessage::ResolveWorkspaceDirectory { root_dir })
        .await
        .map_err(directory_lookup_error)?;
    match result {
        ResponsePayload::WorkspaceResolved(ws) => Ok(ws),
        other => Err(unexpected_response(other)),
    }
}

fn directory_lookup_error(err: ClientError) -> ClientError {
    let unsupported = match err.downcast_ref::<ProtoError>() {
        Some(ProtoError::InvalidRequest(reason)) => {
            let reason = reason.to_ascii_lowercase();
            reason.contains("unsupported") || reason.contains("unknown request")
        }
        _ => false,
    };
    let disconnected = err
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == std::io::ErrorKind::UnexpectedEof);
    if unsupported || disconnected {
        format!("workspace directory lookup failed: {err}. For older daemons, pass --workspace <ID> or upgrade daedalusd").into()
    } else {
        err
    }
}

pub(crate) async fn execute(
    client: &mut DaemonClient,
    action: CustomActionCommand,
    remote: bool,
) -> Result<(), ClientError> {
    match action {
        CustomActionCommand::List {
            workspace,
            dir,
            json,
        } => {
            let ws = if let Some(id) = workspace {
                match client.request(ClientMessage::ListWorkspaces).await? {
                    ResponsePayload::Workspaces(list) => list
                        .into_iter()
                        .find(|ws| ws.id == WorkspaceId(id))
                        .ok_or_else(|| format!("workspace not found: {id}"))?,
                    other => return Err(unexpected_response(other)),
                }
            } else {
                resolve_directory(client, dir, remote).await?
            };
            let actions = ws.manifest.map(|m| m.actions).unwrap_or_default();
            if json {
                println!("{}", serde_json::to_string_pretty(&actions)?);
            } else if actions.is_empty() {
                println!("no actions configured");
            } else {
                for a in actions {
                    if let Some(shortcut) = &a.shortcut {
                        println!(
                            "{}\t{}\t{}\t[{}]\t{}",
                            a.id, a.target, a.label, shortcut, a.command
                        );
                    } else {
                        println!("{}\t{}\t{}\t{}", a.id, a.target, a.label, a.command);
                    }
                }
            }
        }
        CustomActionCommand::Run {
            action_id,
            workspace,
            dir,
        } => {
            let workspace_id = match workspace {
                Some(id) => WorkspaceId(id),
                None => resolve_directory(client, dir, remote).await?.id,
            };
            expect_ok(
                client
                    .request(ClientMessage::RunWorkspaceAction {
                        workspace_id,
                        action_id: action_id.clone(),
                    })
                    .await?,
            )?;
            println!("ran action {action_id}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_paths_are_anchored_on_the_intended_host() {
        let relative = PathBuf::from("remote-only/../alias");
        assert_eq!(
            target_directory(Some(relative.clone()), true).unwrap(),
            relative
        );
        assert_eq!(target_directory(None, true).unwrap(), PathBuf::from("."));
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(target_directory(None, false).unwrap(), cwd);
        assert_eq!(
            target_directory(Some(relative.clone()), false).unwrap(),
            cwd.join(relative)
        );
        let daemon_path = PathBuf::from("/home/dev/project");
        assert_eq!(
            target_directory(Some(daemon_path.clone()), false).unwrap(),
            daemon_path
        );
    }
}
