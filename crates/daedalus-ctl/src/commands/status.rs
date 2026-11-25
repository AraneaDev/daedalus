use crate::client::DaemonClient;
use crate::{
    AgentStateAction, StatusAction, expect_ok, resolve_status_source, resolve_target_root_dir,
};
use daedalus_proto::{ClientMessage, PaneId, WorkspaceId};

pub(crate) async fn execute(
    client: &mut DaemonClient,
    action: StatusAction,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        StatusAction::Publish {
            preset,
            source,
            label,
            level,
            workspace,
            dir,
            detail,
        } => {
            let source = resolve_status_source(preset, source)?;
            let root_dir = resolve_target_root_dir(workspace.is_some(), dir)?;
            expect_ok(
                client
                    .request(ClientMessage::PublishExternalStatus {
                        workspace_id: workspace.map(WorkspaceId),
                        root_dir,
                        source,
                        level: level.into(),
                        label,
                        detail,
                    })
                    .await?,
            )?;
            println!("published");
        }
        StatusAction::Clear {
            source,
            preset,
            workspace,
            dir,
        } => {
            let source = resolve_status_source(preset, source)?;
            let root_dir = resolve_target_root_dir(workspace.is_some(), dir)?;
            expect_ok(
                client
                    .request(ClientMessage::ClearExternalStatus {
                        workspace_id: workspace.map(WorkspaceId),
                        root_dir,
                        source,
                    })
                    .await?,
            )?;
            println!("cleared");
        }
    }
    Ok(())
}

pub(crate) async fn set_agent_state(
    client: &mut DaemonClient,
    action: AgentStateAction,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        AgentStateAction::Set {
            state,
            pane,
            workspace,
            dir,
        } => {
            let root_dir = resolve_target_root_dir(pane.is_some() || workspace.is_some(), dir)?;
            expect_ok(
                client
                    .request(ClientMessage::SetAgentState {
                        pane_id: pane.map(PaneId),
                        workspace_id: workspace.map(WorkspaceId),
                        root_dir,
                        state,
                    })
                    .await?,
            )?;
            println!("agent state set to {state}");
        }
    }
    Ok(())
}
