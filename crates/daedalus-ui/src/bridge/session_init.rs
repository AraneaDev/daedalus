use super::{
    BridgeConfig, DEFAULT_PANE_COLS, DEFAULT_PANE_ROWS, UiCommand, UiEvent, UiEventSink,
    rpc::{RpcConnection, RpcError},
    workspace::{create_and_populate_workspace, workspace_name_from_dir},
};
use daedalus_proto::{ClientMessage, PaneId, ProtoError, ResponsePayload, WorkspaceInfo};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::{mpsc, watch};

pub(super) async fn initialize_session(
    conn: &mut RpcConnection,
    config: &BridgeConfig,
    sink: &Arc<dyn UiEventSink>,
    bootstrapped: &mut bool,
    session: &watch::Sender<Option<mpsc::UnboundedSender<UiCommand>>>,
    cmd_tx: mpsc::UnboundedSender<UiCommand>,
) -> Result<(), RpcError> {
    let capabilities = negotiate(conn, config.auth_token.as_deref()).await?;

    // Accept commands while terminal state loads, but publish readiness only once
    // the connection can drain its queue.
    session.send_replace(Some(cmd_tx));
    capabilities.emit(sink.as_ref());
    hydrate_initial_workspaces(conn, config, sink, bootstrapped).await?;
    sink.emit(UiEvent::Connected);
    Ok(())
}

struct Capabilities {
    persistence_health: Option<daedalus_proto::PersistenceHealth>,
    workbench: bool,
    journal: bool,
    project_setups: bool,
    recent_projects: bool,
    home_dir: Option<std::path::PathBuf>,
}

impl Capabilities {
    fn emit(self, sink: &dyn UiEventSink) {
        sink.emit(UiEvent::WorkbenchCapability(self.workbench));
        sink.emit(UiEvent::JournalCapability(self.journal));
        sink.emit(UiEvent::DaemonCapabilities {
            persistence_health: self.persistence_health,
            home_dir: self.home_dir,
            project_setups: self.project_setups,
            recent_projects: self.recent_projects,
        });
    }
}

async fn negotiate(
    conn: &mut RpcConnection,
    auth_token: Option<&str>,
) -> Result<Capabilities, RpcError> {
    if let Some(token) = auth_token.filter(|token| !token.is_empty()) {
        let response = conn
            .call(ClientMessage::Authenticate {
                token: token.to_owned(),
            })
            .await?;
        if !matches!(response, ResponsePayload::Ok) {
            return Err(RpcError::Proto(ProtoError::InvalidRequest(
                "unexpected authentication response".into(),
            )));
        }
    }
    let ResponsePayload::Pong {
        persistence_health,
        journal_supported,
        inspect_setup_supported,
        recent_projects_supported,
        project_setup_supported,
        image_preview_supported,
        paste_supported,
        workbench_supported,
        workspace_membership_supported,
        runner_assignment_supported,
        guarded_runner_assignment_supported,
        resolved_import_preview_supported,
        agent_launch_metadata_supported,
        home_dir,
        ..
    } = conn.call(ClientMessage::Ping).await?
    else {
        return Err(RpcError::Proto(ProtoError::InvalidRequest(
            "Unexpected response to daemon handshake.".into(),
        )));
    };
    conn.inspect_setup_supported = inspect_setup_supported;
    conn.recent_projects_supported = recent_projects_supported;
    conn.image_preview_supported = image_preview_supported;
    conn.project_setup_supported = project_setup_supported;
    conn.paste_supported = paste_supported;
    conn.workbench_supported = workbench_supported;
    conn.runner_assignment_supported = runner_assignment_supported;
    conn.guarded_runner_assignment_supported = guarded_runner_assignment_supported;
    conn.resolved_import_preview_supported = resolved_import_preview_supported;
    conn.agent_launch_metadata_supported = agent_launch_metadata_supported;
    let _ = conn
        .call(ClientMessage::Subscribe {
            pane_ids: None,
            project_setup_events: project_setup_supported,
        })
        .await?;
    if workspace_membership_supported {
        conn.call(ClientMessage::WatchWorkspaceMembership).await?;
    }
    Ok(Capabilities {
        persistence_health,
        workbench: workbench_supported,
        journal: journal_supported,
        project_setups: project_setup_supported,
        recent_projects: recent_projects_supported,
        home_dir,
    })
}

async fn hydrate_initial_workspaces(
    conn: &RpcConnection,
    config: &BridgeConfig,
    sink: &Arc<dyn UiEventSink>,
    bootstrapped: &mut bool,
) -> Result<(), RpcError> {
    let workspaces = list_workspaces(conn).await?;

    if workspaces.is_empty() && !*bootstrapped && config.bootstrap_explicit_dir {
        let name = workspace_name_from_dir(&config.initial_dir);
        create_and_populate_workspace(
            conn,
            sink,
            name,
            config.initial_dir.clone(),
            config.default_agent,
            DEFAULT_PANE_COLS,
            DEFAULT_PANE_ROWS,
        )
        .await?;
        *bootstrapped = true;
    } else {
        *bootstrapped = true;
        sink.emit(UiEvent::WorkspacesLoaded(workspaces.clone()));
        let mut hydrated = HashSet::new();
        if !hydrate_panes(conn, sink, &workspaces, &mut hydrated)
            .await?
            .is_empty()
        {
            let mut current = list_workspaces(conn).await?;
            sink.emit(UiEvent::WorkspaceMembershipLoaded(current.clone()));
            let closed = hydrate_panes(conn, sink, &current, &mut hydrated).await?;
            if !closed.is_empty() {
                // Churn can continue during the refresh. Prune those panes locally
                // and leave ongoing membership updates to the connected session.
                for workspace in &mut current {
                    workspace.panes.retain(|pane| !closed.contains(&pane.id));
                }
                sink.emit(UiEvent::WorkspaceMembershipLoaded(current));
            }
        }
    }

    Ok(())
}

async fn list_workspaces(conn: &RpcConnection) -> Result<Vec<WorkspaceInfo>, RpcError> {
    match conn.call(ClientMessage::ListWorkspaces).await? {
        ResponsePayload::Workspaces(workspaces) => Ok(workspaces),
        _ => Err(RpcError::Proto(ProtoError::Internal(
            "Unexpected workspace list response.".into(),
        ))),
    }
}

async fn hydrate_panes(
    conn: &RpcConnection,
    sink: &Arc<dyn UiEventSink>,
    workspaces: &[WorkspaceInfo],
    hydrated: &mut HashSet<PaneId>,
) -> Result<HashSet<PaneId>, RpcError> {
    let mut closed = HashSet::new();
    for pane in workspaces.iter().flat_map(|workspace| &workspace.panes) {
        if hydrated.contains(&pane.id) {
            continue;
        }
        match conn
            .call(ClientMessage::GetSnapshot { pane_id: pane.id })
            .await
        {
            Ok(ResponsePayload::Snapshot(snapshot)) => {
                hydrated.insert(pane.id);
                sink.emit(UiEvent::Snapshot(snapshot));
            }
            Err(RpcError::Proto(ProtoError::PaneNotFound(id))) if id == pane.id => {
                closed.insert(id);
            }
            Err(error) => return Err(error),
            Ok(_) => {}
        }
    }
    Ok(closed)
}
