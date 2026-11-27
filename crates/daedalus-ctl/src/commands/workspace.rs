use crate::client::DaemonClient;
use crate::{
    WorkspaceAction, expect_ok, format_recent_project, parse_env_pairs, resolve_dir,
    unexpected_response,
};
use daedalus_proto::ResponsePayload;
use daedalus_proto::{ClientMessage, WorkspaceId};
use std::io::Write;

pub(super) async fn persistence_warning(
    client: &mut DaemonClient,
) -> Result<Option<&'static str>, Box<dyn std::error::Error>> {
    match client.request(ClientMessage::Ping).await? {
        ResponsePayload::Pong {
            persistence_health, ..
        } => Ok(persistence_health.and_then(daedalus_proto::PersistenceHealth::warning)),
        other => Err(unexpected_response(other)),
    }
}

pub(super) fn print_persistence_warning(warning: Option<&str>) {
    if let Some(message) = warning {
        // The daemon has already acknowledged the operation.
        let _ = writeln!(std::io::stderr().lock(), "{message}");
    }
}

pub(crate) async fn execute(
    client: &mut DaemonClient,
    action: WorkspaceAction,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        WorkspaceAction::Configure(args) => args.run(client).await?,
        WorkspaceAction::Import(args) => args.run(client).await?,
        WorkspaceAction::Export(args) => args.run(client).await?,
        WorkspaceAction::Update(args) => args.run(client).await?,
        WorkspaceAction::Create { name, dir, env } => {
            let root_dir = resolve_dir(Some(dir))?;
            let env_map = parse_env_pairs(&env)?;
            let warning = persistence_warning(client).await?;
            match client
                .request(ClientMessage::CreateWorkspace {
                    name,
                    root_dir,
                    env: env_map,
                })
                .await?
            {
                ResponsePayload::WorkspaceCreated(ws) => {
                    println!("{}", ws.id);
                    print_persistence_warning(warning);
                }
                other => return Err(unexpected_response(other)),
            }
        }
        WorkspaceAction::List { json } => {
            match client.request(ClientMessage::ListWorkspaces).await? {
                ResponsePayload::Workspaces(list) => {
                    if json {
                        let identities: Vec<_> = list
                        .iter()
                        .map(|workspace| {
                            serde_json::json!({
                                "id": workspace.id,
                                "name": workspace.name,
                                "root_dir": workspace.root_dir,
                                "pane_ids": workspace.panes.iter().map(|pane| pane.id).collect::<Vec<_>>(),
                            })
                        })
                        .collect();
                        println!("{}", serde_json::to_string(&identities)?);
                        return Ok(());
                    }
                    for ws in list {
                        let mut line = format!(
                            "{}  {}  ({})  panes={}",
                            ws.id,
                            ws.name,
                            ws.root_dir.display(),
                            ws.panes.len()
                        );
                        if let Some(branch) = &ws.git_branch {
                            line.push_str(&format!("  branch={branch}"));
                        }
                        if ws.port_offset > 0 {
                            line.push_str(&format!("  offset=+{}", ws.port_offset));
                        }
                        if !ws.external_statuses.is_empty() {
                            let badges: Vec<String> = ws
                                .external_statuses
                                .iter()
                                .map(|s| format!("{}:{}", s.source, s.label))
                                .collect();
                            line.push_str(&format!("  status=[{}]", badges.join(", ")));
                        }
                        println!("{line}");
                    }
                }
                other => return Err(unexpected_response(other)),
            }
        }
        WorkspaceAction::Recent => {
            let supported = matches!(
                client.request(ClientMessage::Ping).await?,
                ResponsePayload::Pong {
                    recent_projects_supported: true,
                    ..
                }
            );
            if !supported {
                return Err("This daemon does not keep recent projects. Upgrade daedalusd.".into());
            }
            match client.request(ClientMessage::ListRecentProjects).await? {
                ResponsePayload::RecentProjects(list) if list.is_empty() => {
                    println!("No recent projects.");
                }
                ResponsePayload::RecentProjects(list) => {
                    for project in &list {
                        println!("{}", format_recent_project(project));
                    }
                }
                other => return Err(unexpected_response(other)),
            }
        }
        WorkspaceAction::Close {
            id,
            compose_down,
            remove_worktree,
            force,
        } => {
            let warning = persistence_warning(client).await?;
            expect_ok(
                client
                    .request(ClientMessage::CloseWorkspace {
                        id: WorkspaceId(id),
                        compose_down,
                        remove_worktree,
                        force_remove_worktree: force,
                    })
                    .await?,
            )?;
            println!("closed {id}");
            print_persistence_warning(warning);
        }
    }
    Ok(())
}
