use super::rpc::{RpcConnection, RpcError};
use super::workspace::emit_spawned_pane;
use super::{DEFAULT_PANE_COLS, DEFAULT_PANE_ROWS, UiEvent, UiEventSink};
use crate::assignments::{self, Assignment};
use crate::state::AgentPreset;
use daedalus_proto::{
    ClientMessage, PaneId, PaneRole, PaneSummary, ProtoError, ResponsePayload, WorkspaceId,
};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::warn;

pub(super) async fn inspect(
    conn: &RpcConnection,
    sink: &Arc<dyn UiEventSink>,
    session: u64,
    root_dir: PathBuf,
    executables: Vec<String>,
) -> Result<(), RpcError> {
    let result = if !conn.inspect_setup_supported {
        Err("Update the daemon on the workspace host to use guided setup.".into())
    } else {
        match conn
            .call(ClientMessage::InspectSetup {
                root_dir,
                executables,
            })
            .await
        {
            Ok(ResponsePayload::SetupInspection(report)) => Ok(report),
            Err(RpcError::Proto(err)) => Err(err.to_string()),
            Err(err) => return Err(err),
            Ok(_) => Err("Unexpected reply to project inspection.".into()),
        }
    };
    sink.emit(UiEvent::SetupInspected { session, result });
    Ok(())
}

async fn spawn(
    conn: &RpcConnection,
    workspace_id: WorkspaceId,
    role: PaneRole,
    command: Option<Vec<String>>,
    cols: u16,
    rows: u16,
) -> Result<PaneSummary, RpcError> {
    match conn
        .call(ClientMessage::SpawnPane {
            workspace_id,
            role,
            command,
            cols,
            rows,
            shell: None,
            env: Default::default(),
        })
        .await?
    {
        ResponsePayload::PaneSpawned(summary) => Ok(summary),
        _ => Err(RpcError::Proto(ProtoError::Internal(
            "Unexpected terminal launch response.".into(),
        ))),
    }
}

async fn snapshot(
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

pub(super) async fn launch_assignment(
    conn: &RpcConnection,
    sink: &Arc<dyn UiEventSink>,
    workspace_id: WorkspaceId,
    assignment: Assignment,
    saved_runner: bool,
    cols: u16,
    rows: u16,
) -> Result<(), RpcError> {
    let agent_preset = if assignment.role == PaneRole::Agent {
        assignment
            .argv
            .first()
            .and_then(|program| AgentPreset::from_executable(program))
    } else {
        None
    };
    let summary = if saved_runner && conn.runner_assignment_supported {
        match conn
            .call(ClientMessage::LaunchRunnerAssignment {
                workspace_id,
                assignment_id: assignment.id.clone(),
                cols,
                rows,
            })
            .await?
        {
            ResponsePayload::PaneSpawned(summary) => summary,
            _ => {
                return Err(RpcError::Proto(ProtoError::Internal(
                    "Unexpected runner launch response.".into(),
                )));
            }
        }
    } else {
        let command = (!assignment.argv.is_empty()).then(|| assignment.argv.clone());
        spawn(conn, workspace_id, assignment.role, command, cols, rows).await?
    };
    let pane_id = summary.id;
    emit_spawned_pane(conn, sink, summary, agent_preset, false);
    sink.emit(UiEvent::AssignmentStarted {
        workspace_id,
        pane_id,
        assignment,
    });
    snapshot(conn, sink, pane_id).await
}

async fn store_on_daemon(
    conn: &RpcConnection,
    workspace_id: WorkspaceId,
    entries: &[Assignment],
) -> Result<Result<(), String>, RpcError> {
    if let Err(message) = daedalus_proto::validate_assignments(entries) {
        return Ok(Err(message));
    }
    match conn
        .call(ClientMessage::SetProjectSetup {
            workspace_id,
            assignments: entries.to_vec(),
        })
        .await
    {
        Ok(ResponsePayload::Ok) => Ok(Ok(())),
        Ok(other) => Ok(Err(format!(
            "Unexpected reply while saving settings: {other:?}"
        ))),
        Err(RpcError::Proto(err)) => Ok(Err(err.to_string())),
        Err(err) => Err(err),
    }
}

pub(super) async fn save_project_setup(
    conn: &RpcConnection,
    sink: &Arc<dyn UiEventSink>,
    workspace_id: WorkspaceId,
    entries: Vec<Assignment>,
    request_id: u64,
) -> Result<(), RpcError> {
    let result = if conn.project_setup_supported {
        store_on_daemon(conn, workspace_id, &entries).await?
    } else {
        Err("This daemon does not support saving project settings.".into())
    };
    sink.emit(UiEvent::ProjectSetupMigrationFinished {
        workspace_id,
        request_id,
        result,
    });
    Ok(())
}

pub(super) async fn save_workspace_settings(
    conn: &RpcConnection,
    sink: &Arc<dyn UiEventSink>,
    workspace_id: WorkspaceId,
    entries: Vec<Assignment>,
    request_id: u64,
) -> Result<(), RpcError> {
    let result = if conn.project_setup_supported {
        store_on_daemon(conn, workspace_id, &entries).await?
    } else {
        Err("This daemon does not support saving project settings.".into())
    };
    sink.emit(UiEvent::WorkspaceSettingsSaved {
        workspace_id,
        request_id,
        result,
    });
    Ok(())
}

pub(super) async fn refresh_recent_projects(
    conn: &RpcConnection,
    sink: &Arc<dyn UiEventSink>,
) -> Result<(), RpcError> {
    if !conn.recent_projects_supported {
        return Ok(());
    }
    match conn.call(ClientMessage::ListRecentProjects).await {
        Ok(ResponsePayload::RecentProjects(projects)) => {
            sink.emit(UiEvent::RecentProjectsLoaded(projects));
        }
        Err(RpcError::Proto(error)) => warn!("listing recent projects failed: {error:?}"),
        Err(error) => return Err(error),
        Ok(other) => warn!("unexpected reply to ListRecentProjects: {other:?}"),
    }
    Ok(())
}

pub(super) async fn apply(
    conn: &RpcConnection,
    sink: &Arc<dyn UiEventSink>,
    workspace_id: WorkspaceId,
    entries: Vec<Assignment>,
    launch: Vec<String>,
) -> Result<(), RpcError> {
    let stored = if conn.project_setup_supported {
        store_on_daemon(conn, workspace_id, &entries).await?
    } else {
        assignments::save(workspace_id, &entries)
    };
    if let Err(message) = stored {
        sink.emit(UiEvent::SetupApplied {
            workspace_id,
            error: Some(message),
        });
        return Ok(());
    }
    let mut failures = Vec::new();
    for entry in entries
        .into_iter()
        .filter(|entry| launch.contains(&entry.id))
    {
        let name = entry.name.clone();
        let saved_runner = conn.project_setup_supported && entry.role == PaneRole::Runner;
        match launch_assignment(
            conn,
            sink,
            workspace_id,
            entry,
            saved_runner,
            DEFAULT_PANE_COLS,
            DEFAULT_PANE_ROWS,
        )
        .await
        {
            Ok(()) => {}
            Err(RpcError::Proto(err)) => failures.push(format!("{name}: {err}")),
            Err(err) => return Err(err),
        }
    }
    sink.emit(UiEvent::SetupApplied {
        workspace_id,
        error: None,
    });
    if !failures.is_empty() {
        sink.emit(UiEvent::Toast {
            message: format!(
                "Workspace saved. Open Settings to edit or retry {}.",
                failures.join(" · ")
            ),
            is_error: true,
        });
    }
    Ok(())
}

pub(super) async fn install(
    conn: &RpcConnection,
    sink: &Arc<dyn UiEventSink>,
    workspace_id: WorkspaceId,
    argv: Vec<String>,
) -> Result<(), RpcError> {
    if argv.first().is_none_or(|value| value.trim().is_empty())
        || argv.iter().any(|value| value.contains('\0'))
    {
        sink.emit(UiEvent::SetupInstallerFailed {
            workspace_id,
            message: "Choose a valid installer command.".into(),
        });
        return Ok(());
    }
    match spawn(
        conn,
        workspace_id,
        PaneRole::Runner,
        Some(argv),
        DEFAULT_PANE_COLS,
        DEFAULT_PANE_ROWS,
    )
    .await
    {
        Ok(summary) => {
            let pane_id = summary.id;
            emit_spawned_pane(conn, sink, summary, None, false);
            sink.emit(UiEvent::SetupInstallerStarted {
                workspace_id,
                pane_id,
            });
            snapshot(conn, sink, pane_id).await?;
            // A fast installer can exit before SpawnPane registers the terminal in the UI.
            // Reconcile once after registration. Subsequent exits arrive via the subscription.
            if let ResponsePayload::Workspaces(workspaces) =
                conn.call(ClientMessage::ListWorkspaces).await?
                && let Some(pane) = workspaces
                    .iter()
                    .find(|ws| ws.id == workspace_id)
                    .and_then(|ws| ws.panes.iter().find(|pane| pane.id == pane_id))
                && let daedalus_proto::PaneStatus::Exited { code } = pane.status
            {
                sink.emit(UiEvent::SetupInstallerFinished(pane_id, code));
            }
        }
        Err(RpcError::Proto(err)) => sink.emit(UiEvent::SetupInstallerFailed {
            workspace_id,
            message: err.to_string(),
        }),
        Err(err) => return Err(err),
    }
    Ok(())
}
