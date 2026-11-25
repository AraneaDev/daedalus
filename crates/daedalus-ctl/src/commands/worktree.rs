use crate::client::DaemonClient;
use crate::{WorktreeAction, parse_env_pairs, resolve_dir, unexpected_response};
use daedalus_proto::ClientMessage;
use daedalus_proto::ResponsePayload;

pub(crate) async fn execute(
    client: &mut DaemonClient,
    action: WorktreeAction,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        WorktreeAction::List { dir } => {
            let repo_dir = resolve_dir(dir)?;
            match client
                .request(ClientMessage::DiscoverWorktrees { repo_dir })
                .await?
            {
                ResponsePayload::Worktrees(worktrees) => {
                    for wt in worktrees {
                        let branch = wt.branch.as_deref().unwrap_or("<detached>");
                        let short_head: String = wt.head.chars().take(8).collect();
                        let tag = if wt.is_primary { " [primary]" } else { "" };
                        println!("{}  {}  ({}){}", wt.path.display(), branch, short_head, tag);
                    }
                }
                other => return Err(unexpected_response(other)),
            }
        }
        WorktreeAction::Create {
            branch,
            dir,
            existing,
            env,
        } => {
            let repo_dir = resolve_dir(dir)?;
            let env_map = parse_env_pairs(&env)?;
            match client
                .request(ClientMessage::CreateWorktreeWorkspace {
                    repo_dir,
                    branch,
                    create_branch: !existing,
                    env: env_map,
                })
                .await?
            {
                ResponsePayload::WorkspaceCreated(ws) => println!("{}", ws.id),
                other => return Err(unexpected_response(other)),
            }
        }
    }
    Ok(())
}
