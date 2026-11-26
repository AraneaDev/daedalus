use super::rpc::{RpcConnection, RpcError};
use super::workspace::emit_spawned_pane;
use super::{AssignmentLaunch, DEFAULT_PANE_COLS, DEFAULT_PANE_ROWS, UiEvent, UiEventSink};
use crate::assignments::{self, Assignment};
use crate::state::AgentPreset;
use daedalus_proto::{
    ClientMessage, PaneId, PaneRole, PaneSummary, ProjectSetupBaseline, ProtoError,
    ResponsePayload, WorkspaceId,
};
use std::path::PathBuf;
use std::sync::Arc;

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
    launch: AssignmentLaunch,
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
    let use_saved_runner = match launch {
        AssignmentLaunch::Direct => false,
        AssignmentLaunch::SavedRunner => conn.runner_assignment_supported,
        AssignmentLaunch::ReviewedRunner => conn.guarded_runner_assignment_supported,
    };
    let summary = if use_saved_runner {
        match conn
            .call(ClientMessage::LaunchRunnerAssignment {
                workspace_id,
                assignment_id: assignment.id.clone(),
                expected: (launch == AssignmentLaunch::ReviewedRunner).then(|| assignment.clone()),
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
        let command = (!assignment.argv.is_empty() && !assignment.uses_default_shell())
            .then(|| assignment.argv.clone());
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

enum SetupSaveError {
    Conflict,
    Other(String),
}

impl SetupSaveError {
    fn message(self) -> String {
        match self {
            Self::Conflict => "Pane setup changed on another client. Your draft is still open. Reopen Settings to review the saved setup.".into(),
            Self::Other(message) => message,
        }
    }
}

async fn store_on_daemon(
    conn: &RpcConnection,
    workspace_id: WorkspaceId,
    entries: &[Assignment],
    expected: ProjectSetupBaseline,
) -> Result<Result<(), SetupSaveError>, RpcError> {
    if let Err(message) = daedalus_proto::validate_assignments(entries) {
        return Ok(Err(SetupSaveError::Other(message)));
    }
    match conn
        .call(ClientMessage::SetProjectSetup {
            workspace_id,
            assignments: entries.to_vec(),
            expected: Some(expected),
        })
        .await
    {
        Ok(ResponsePayload::Ok) => Ok(Ok(())),
        Ok(ResponsePayload::ProjectSetupConflict(_)) => Ok(Err(SetupSaveError::Conflict)),
        Ok(other) => Ok(Err(SetupSaveError::Other(format!(
            "Unexpected reply while saving settings: {other:?}"
        )))),
        Err(RpcError::Proto(err)) => Ok(Err(SetupSaveError::Other(err.to_string()))),
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
        store_on_daemon(
            conn,
            workspace_id,
            &entries,
            ProjectSetupBaseline::default(),
        )
        .await?
        .map_err(|error| match error {
            SetupSaveError::Conflict => {
                "Using the project setup already saved on the daemon.".into()
            }
            SetupSaveError::Other(message) => message,
        })
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
    expected: ProjectSetupBaseline,
    request_id: u64,
) -> Result<(), RpcError> {
    let result = if conn.project_setup_supported {
        store_on_daemon(conn, workspace_id, &entries, expected).await?
    } else {
        Err(SetupSaveError::Other(
            "This daemon does not support saving project settings.".into(),
        ))
    };
    let conflict = matches!(result, Err(SetupSaveError::Conflict));
    sink.emit(UiEvent::WorkspaceSettingsSaved {
        workspace_id,
        request_id,
        result: result.map_err(SetupSaveError::message),
    });
    if conflict {
        super::reconcile_membership(conn, sink).await?;
    }
    Ok(())
}

pub(super) async fn apply(
    conn: &RpcConnection,
    sink: &Arc<dyn UiEventSink>,
    workspace_id: WorkspaceId,
    entries: Vec<Assignment>,
    launch: Vec<String>,
    expected: ProjectSetupBaseline,
) -> Result<(), RpcError> {
    let stored = if conn.project_setup_supported {
        store_on_daemon(conn, workspace_id, &entries, expected).await?
    } else {
        match assignments::save_if_current(workspace_id, &entries, &expected) {
            Ok(true) => Ok(()),
            Ok(false) => Err(SetupSaveError::Conflict),
            Err(message) => Err(SetupSaveError::Other(message)),
        }
    };
    if let Err(error) = stored {
        let conflict = matches!(error, SetupSaveError::Conflict);
        sink.emit(UiEvent::SetupApplied {
            workspace_id,
            error: Some(match error {
                SetupSaveError::Conflict => "Pane setup changed on another client. Close the review and open Settings to review the saved setup.".into(),
                SetupSaveError::Other(message) => message,
            }),
        });
        if conflict && conn.project_setup_supported {
            super::reconcile_membership(conn, sink).await?;
        }
        return Ok(());
    }
    launch_entries(
        conn,
        sink,
        workspace_id,
        entries
            .into_iter()
            .filter(|entry| launch.contains(&entry.id)),
    )
    .await
}

pub(super) async fn launch_stored(
    conn: &RpcConnection,
    sink: &Arc<dyn UiEventSink>,
    workspace_id: WorkspaceId,
) -> Result<(), RpcError> {
    // Settings may have changed after the workspace creation reply. Launch
    // from a fresh read without persisting the creation-time snapshot.
    let result = match conn.call(ClientMessage::ListWorkspaces).await {
        Ok(ResponsePayload::Workspaces(workspaces)) => workspaces
            .into_iter()
            .find(|workspace| workspace.id == workspace_id)
            .ok_or_else(|| ProtoError::WorkspaceNotFound(workspace_id).to_string())
            .and_then(|workspace| {
                let entries = workspace.setup.ok_or_else(|| {
                    "This workspace has no saved pane setup. Open Settings to configure it."
                        .to_owned()
                })?;
                // Legacy daemon state may have been written before validation.
                daedalus_proto::validate_assignments(&entries)?;
                Ok(entries)
            }),
        Ok(_) => Err("Unexpected reply while loading the saved setup.".into()),
        Err(RpcError::Proto(error)) => Err(error.to_string()),
        Err(error) => return Err(error),
    };
    match result {
        Ok(entries) => {
            launch_entries(
                conn,
                sink,
                workspace_id,
                entries
                    .into_iter()
                    .filter(|entry| entry.role == PaneRole::Agent),
            )
            .await
        }
        Err(message) => {
            sink.emit(UiEvent::SetupApplied {
                workspace_id,
                error: Some(message),
            });
            Ok(())
        }
    }
}

async fn launch_entries(
    conn: &RpcConnection,
    sink: &Arc<dyn UiEventSink>,
    workspace_id: WorkspaceId,
    entries: impl IntoIterator<Item = Assignment>,
) -> Result<(), RpcError> {
    let mut failures = Vec::new();
    for entry in entries {
        let name = entry.name.clone();
        let launch = if conn.project_setup_supported && entry.role == PaneRole::Runner {
            AssignmentLaunch::ReviewedRunner
        } else {
            AssignmentLaunch::Direct
        };
        match launch_assignment(
            conn,
            sink,
            workspace_id,
            entry,
            launch,
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
            message: format!("Open Settings to edit or retry {}.", failures.join(" · ")),
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
