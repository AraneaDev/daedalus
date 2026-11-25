use super::rpc::{RpcConnection, RpcError};
use super::{UiEvent, UiEventSink};
use crate::state::AgentPreset;
use daedalus_proto::{
    ClientMessage, PaneId, PaneRole, ProtoError, ResponsePayload, WorkspaceId, WorkspaceInfo,
    WorkspaceManifest,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::warn;

pub(super) async fn create_manual_workspace(
    conn: &RpcConnection,
    sink: &Arc<dyn UiEventSink>,
    session: u64,
    name: String,
    root_dir: PathBuf,
) -> Result<(), RpcError> {
    let event = match conn
        .call(ClientMessage::CreateManualWorkspace { name, root_dir })
        .await
    {
        Ok(ResponsePayload::WorkspaceCreated(workspace)) => {
            UiEvent::ManualWorkspaceCreated { session, workspace }
        }
        Ok(_) => UiEvent::ManualWorkspaceFailed {
            session,
            message: "Unexpected reply to workspace creation.".into(),
        },
        Err(RpcError::Proto(error)) => UiEvent::ManualWorkspaceFailed {
            session,
            message: error.to_string(),
        },
        Err(err @ (RpcError::Disconnected | RpcError::TimedOut)) => return Err(err),
    };
    sink.emit(event);
    Ok(())
}

pub(super) async fn close_workspace(
    conn: &RpcConnection,
    sink: &Arc<dyn UiEventSink>,
    id: WorkspaceId,
    compose_down: bool,
    remove_worktree: bool,
    force_remove_worktree: bool,
) -> Result<(), RpcError> {
    let close = conn
        .call(ClientMessage::CloseWorkspace {
            id,
            compose_down,
            remove_worktree,
            force_remove_worktree,
        })
        .await;
    if matches!(close, Err(RpcError::Disconnected | RpcError::TimedOut)) {
        return close.map(|_| ());
    }
    if let ResponsePayload::Workspaces(ws) = conn.call(ClientMessage::ListWorkspaces).await? {
        if close.is_ok() {
            sink.emit(UiEvent::WorkspacesLoaded(ws));
        } else {
            sink.emit(UiEvent::WorkspaceCloseFailed(ws));
        }
    }
    close.map(|_| ())
}

pub(super) async fn add_agent(
    conn: &RpcConnection,
    sink: &Arc<dyn UiEventSink>,
    workspace_id: WorkspaceId,
    preset: AgentPreset,
    cols: u16,
    rows: u16,
) -> Result<(), RpcError> {
    let response = conn
        .call(ClientMessage::SpawnPane {
            workspace_id,
            role: PaneRole::Agent,
            command: preset.command(),
            shell: None,
            env: Default::default(),
            cols,
            rows,
        })
        .await?;
    if let ResponsePayload::PaneSpawned(summary) = response {
        publish_pane(conn, sink, summary, Some(preset)).await?;
    }
    Ok(())
}

pub(super) async fn replace_agent_preset(
    conn: &RpcConnection,
    sink: &Arc<dyn UiEventSink>,
    workspace_id: WorkspaceId,
    old_pane_id: Option<PaneId>,
    preset: AgentPreset,
    cols: u16,
    rows: u16,
) -> Result<(), RpcError> {
    let summary = match conn
        .call(ClientMessage::ReplaceAgentPane {
            workspace_id,
            old_pane_id,
            command: preset.command(),
            cols: cols.max(1),
            rows: rows.max(1),
        })
        .await?
    {
        ResponsePayload::PaneSpawned(summary) => summary,
        other => {
            warn!(response = ?other, "unexpected response to ReplaceAgentPane");
            return Ok(());
        }
    };

    let pane_id = summary.id;
    sink.emit(UiEvent::PaneSpawned {
        summary,
        agent_preset: Some(preset),
    });

    if let Some(pane_id) = old_pane_id {
        sink.emit(UiEvent::PaneClosed { pane_id });
    }

    if let ResponsePayload::Snapshot(snapshot) =
        conn.call(ClientMessage::GetSnapshot { pane_id }).await?
    {
        sink.emit(UiEvent::Snapshot(snapshot));
    }

    sink.emit(UiEvent::Toast {
        message: format!("Switched Agent pane to {}", preset.short_name()),
        is_error: false,
    });
    Ok(())
}

pub(super) async fn create_and_populate_workspace(
    conn: &RpcConnection,
    sink: &Arc<dyn UiEventSink>,
    name: String,
    root_dir: PathBuf,
    requested_agent: AgentPreset,
    cols: u16,
    rows: u16,
) -> Result<(), RpcError> {
    let ws = match conn
        .call(ClientMessage::CreateWorkspace {
            name,
            root_dir,
            env: HashMap::new(),
        })
        .await?
    {
        ResponsePayload::WorkspaceCreated(ws) => ws,
        other => {
            warn!(response = ?other, "unexpected response to CreateWorkspace");
            return Ok(());
        }
    };

    populate_workspace_panes(conn, sink, ws, requested_agent, cols, rows).await
}

pub(super) async fn create_worktree_and_populate_workspace(
    conn: &RpcConnection,
    sink: &Arc<dyn UiEventSink>,
    repo_dir: PathBuf,
    branch: String,
    requested_agent: AgentPreset,
    cols: u16,
    rows: u16,
) -> Result<(), RpcError> {
    let first_attempt = conn
        .call(ClientMessage::CreateWorktreeWorkspace {
            repo_dir: repo_dir.clone(),
            branch: branch.clone(),
            create_branch: true,
            env: HashMap::new(),
        })
        .await;

    let response = match first_attempt {
        Err(RpcError::Proto(ProtoError::InvalidRequest(ref msg)))
            if msg.contains("git worktree add failed") && msg.contains("already exists") =>
        {
            conn.call(ClientMessage::CreateWorktreeWorkspace {
                repo_dir,
                branch,
                create_branch: false,
                env: HashMap::new(),
            })
            .await?
        }
        other => other?,
    };

    let ws = match response {
        ResponsePayload::WorkspaceCreated(ws) => ws,
        other => {
            warn!(response = ?other, "unexpected response to CreateWorktreeWorkspace");
            return Ok(());
        }
    };

    populate_workspace_panes(conn, sink, ws, requested_agent, cols, rows).await
}

async fn populate_workspace_panes(
    conn: &RpcConnection,
    sink: &Arc<dyn UiEventSink>,
    ws: WorkspaceInfo,
    requested_agent: AgentPreset,
    cols: u16,
    rows: u16,
) -> Result<(), RpcError> {
    let workspace_id = ws.id;
    sink.emit(UiEvent::WorkspaceCreated(ws.clone()));
    let result = spawn_workspace_panes(conn, sink, &ws, requested_agent, cols, rows).await;
    sink.emit(UiEvent::WorkspacePanesEnsured {
        workspace_id,
        complete: result.is_ok(),
    });
    result
}

pub(super) async fn ensure_workspace_panes(
    conn: &RpcConnection,
    sink: &Arc<dyn UiEventSink>,
    workspace_id: WorkspaceId,
    default_agent: AgentPreset,
    manifest: Option<WorkspaceManifest>,
    cols: u16,
    rows: u16,
) -> Result<(), RpcError> {
    let result = async {
        let ResponsePayload::Workspaces(workspaces) =
            conn.call(ClientMessage::ListWorkspaces).await?
        else {
            return Err(RpcError::Proto(ProtoError::Internal(
                "unexpected ListWorkspaces response".into(),
            )));
        };
        let mut ws = workspaces
            .into_iter()
            .find(|ws| ws.id == workspace_id)
            .ok_or(RpcError::Proto(ProtoError::WorkspaceNotFound(workspace_id)))?;
        ws.manifest = ws.manifest.or(manifest);
        spawn_workspace_panes(conn, sink, &ws, default_agent, cols, rows).await
    }
    .await;
    sink.emit(UiEvent::WorkspacePanesEnsured {
        workspace_id,
        complete: result.is_ok(),
    });
    result
}

pub(super) async fn publish_pane(
    conn: &RpcConnection,
    sink: &Arc<dyn UiEventSink>,
    summary: daedalus_proto::PaneSummary,
    agent_preset: Option<AgentPreset>,
) -> Result<(), RpcError> {
    let pane_id = summary.id;
    sink.emit(UiEvent::PaneSpawned {
        summary,
        agent_preset,
    });
    if let ResponsePayload::Snapshot(snapshot) =
        conn.call(ClientMessage::GetSnapshot { pane_id }).await?
    {
        sink.emit(UiEvent::Snapshot(snapshot));
    }
    Ok(())
}

async fn spawn_workspace_panes(
    conn: &RpcConnection,
    sink: &Arc<dyn UiEventSink>,
    ws: &WorkspaceInfo,
    requested_agent: AgentPreset,
    cols: u16,
    rows: u16,
) -> Result<(), RpcError> {
    // Older daemons expose only the empty/nonempty distinction and the SpawnPane RPC.
    if !ws.needs_default_panes() {
        for pane in &ws.panes {
            publish_pane(conn, sink, pane.clone(), None).await?;
        }
        return Ok(());
    }
    for (role, command, mut agent_preset) in
        resolve_pane_launch_specs(requested_agent, ws.manifest.as_ref())
    {
        let ensure = |command| {
            conn.call(if ws.default_panes_pending.is_some() {
                ClientMessage::EnsureDefaultPane {
                    workspace_id: ws.id,
                    role,
                    command,
                    cols,
                    rows,
                }
            } else {
                ClientMessage::SpawnPane {
                    workspace_id: ws.id,
                    role,
                    command,
                    shell: None,
                    cols,
                    rows,
                    env: HashMap::new(),
                }
            })
        };
        let response = match ensure(command.clone()).await {
            Err(RpcError::Proto(ProtoError::SpawnFailed(err))) if command.is_some() => {
                sink.emit(UiEvent::Toast {
                    message: format!(
                        "Failed to start {role} command ({err}); falling back to shell"
                    ),
                    is_error: true,
                });
                if role == PaneRole::Agent {
                    agent_preset = Some(AgentPreset::Shell);
                }
                ensure(None).await?
            }
            other => other?,
        };
        let summary = match response {
            ResponsePayload::PaneSpawned(summary) => summary,
            ResponsePayload::DefaultPaneExisting(Some(summary)) => {
                // Another client may have chosen a different preset.
                agent_preset = None;
                summary
            }
            ResponsePayload::DefaultPaneExisting(None) => continue,
            _ => {
                return Err(RpcError::Proto(ProtoError::Internal(
                    "unexpected EnsureDefaultPane response".into(),
                )));
            }
        };
        publish_pane(conn, sink, summary, agent_preset).await?;
    }

    Ok(())
}

type PaneLaunchSpec = (PaneRole, Option<Vec<String>>, Option<AgentPreset>);

fn resolve_pane_launch_specs(
    requested_agent: AgentPreset,
    manifest: Option<&WorkspaceManifest>,
) -> [PaneLaunchSpec; 3] {
    let manifest_preset = manifest
        .and_then(|m| m.agent_preset.as_deref())
        .and_then(AgentPreset::from_cli_name);
    let base_preset = manifest_preset.unwrap_or(requested_agent);

    let (agent_cmd, effective_agent) = match manifest
        .and_then(|m| m.agent_command.as_ref())
        .filter(|cmd| !cmd.is_empty())
    {
        Some(cmd) => {
            let executable = cmd[0].rsplit(['/', '\\']).next().unwrap_or("");
            let inferred_preset = AgentPreset::from_cli_name(
                executable.trim_end_matches(".exe").trim_end_matches(".cmd"),
            )
            .or(manifest_preset);
            (Some(cmd.clone()), inferred_preset)
        }
        None => (base_preset.command(), Some(base_preset)),
    };

    let service_cmd = manifest
        .and_then(|m| m.service_command.clone())
        .filter(|c| !c.is_empty());
    let runner_cmd = manifest
        .and_then(|m| m.runner_command.clone())
        .filter(|c| !c.is_empty());

    [
        (PaneRole::Agent, agent_cmd, effective_agent),
        (PaneRole::Service, service_cmd, None),
        (PaneRole::Runner, runner_cmd, None),
    ]
}

pub(super) fn workspace_name_from_dir(dir: &Path) -> String {
    dir.file_name()
        .and_then(|name| name.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("workspace")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::super::{
        UiCommand, handle_command,
        test_support::{NoopSink, ipc_pair},
    };
    use super::*;
    use daedalus_proto::{Request, ServerMessage, read_frame, write_frame};
    use std::time::Duration;
    #[test]
    fn workspace_name_extracts_basename_or_fallback() {
        assert_eq!(
            workspace_name_from_dir(Path::new("/home/tim/daedalus")),
            "daedalus"
        );
        assert_eq!(workspace_name_from_dir(Path::new("/")), "workspace");
        assert_eq!(workspace_name_from_dir(Path::new("")), "workspace");
    }

    async fn check_provisioning_request(default_panes_pending: Option<bool>) {
        let (client, mut server) = ipc_pair().await;
        let sink: Arc<dyn UiEventSink> = Arc::new(NoopSink);
        let conn = RpcConnection::new(client, sink.clone());
        let command = vec!["agent-only-installed-on-daemon-82f52".to_owned()];
        let manifest = WorkspaceManifest {
            agent_command: Some(command.clone()),
            ..Default::default()
        };
        let ws = WorkspaceInfo {
            id: WorkspaceId::new(),
            name: "test".into(),
            root_dir: PathBuf::new(),
            slot_index: 0,
            injected_env: HashMap::new(),
            panes: vec![],
            port_offset: 0,
            compose_project_name: None,
            git_branch: None,
            git_status: None,
            bound_ports: vec![],
            containers: vec![],
            external_statuses: vec![],
            manifest: Some(manifest),
            project: Default::default(),
            setup: None,
            default_panes_pending,
        };
        let request = async {
            let req = read_frame::<_, Request>(&mut server)
                .await
                .unwrap()
                .unwrap();
            match req.payload {
                ClientMessage::EnsureDefaultPane {
                    role: PaneRole::Agent,
                    command: Some(argv),
                    ..
                } if default_panes_pending.is_some() => assert_eq!(argv, command),
                ClientMessage::SpawnPane {
                    role: PaneRole::Agent,
                    command: Some(argv),
                    ..
                } if default_panes_pending.is_none() => assert_eq!(argv, command),
                other => panic!("unexpected provisioning request {other:?}"),
            }
            drop(server);
        };
        let spawn = spawn_workspace_panes(&conn, &sink, &ws, AgentPreset::Shell, 80, 24);
        let (result, ()) = tokio::join!(spawn, request);
        assert!(matches!(result, Err(RpcError::Disconnected)));
        conn.close().await;
    }

    #[tokio::test]
    async fn provisioning_sends_daemon_only_executable() {
        check_provisioning_request(Some(true)).await;
    }

    #[tokio::test]
    async fn provisioning_uses_spawn_with_a_legacy_daemon() {
        check_provisioning_request(None).await;
    }

    #[tokio::test]
    async fn failed_replacement_sends_preset_and_preserves_old_pane() {
        let (client, mut server) = ipc_pair().await;
        let sink: Arc<dyn UiEventSink> = Arc::new(NoopSink);
        let conn = RpcConnection::new(client, sink.clone());
        let old_pane_id = PaneId::new();
        let cmd = UiCommand::ReplaceAgentPreset {
            workspace_id: WorkspaceId::new(),
            old_pane_id: Some(old_pane_id),
            preset: AgentPreset::Agy,
            cols: 80,
            rows: 24,
        };
        let peer = async {
            let req = tokio::time::timeout(
                Duration::from_secs(2),
                read_frame::<_, Request>(&mut server),
            )
            .await
            .expect("preset not sent to daemon")
            .unwrap()
            .unwrap();
            assert!(
                matches!(req.payload, ClientMessage::ReplaceAgentPane { old_pane_id: Some(expected), command: Some(argv), .. } if expected == old_pane_id && argv == ["agy"])
            );
            write_frame(
                &mut server,
                &ServerMessage::Response {
                    id: req.id,
                    result: Err(ProtoError::SpawnFailed("missing on daemon".into())),
                },
            )
            .await
            .unwrap();
            assert!(
                read_frame::<_, Request>(&mut server)
                    .await
                    .unwrap()
                    .is_none()
            );
        };
        let replace = async {
            assert!(matches!(
                handle_command(&conn, &sink, cmd).await,
                Err(RpcError::Proto(ProtoError::SpawnFailed(_)))
            ));
            conn.close().await;
        };
        tokio::join!(replace, peer);
    }

    #[test]
    fn resolve_pane_launch_specs_honors_manifest_commands() {
        let manifest = WorkspaceManifest {
            agent_preset: Some("claude".to_string()),
            agent_command: Some(vec!["claude".to_string(), "--fast".to_string()]),
            service_command: Some(vec![
                "docker".to_string(),
                "compose".to_string(),
                "up".to_string(),
            ]),
            runner_command: Some(vec!["cargo".to_string(), "test".to_string()]),
            actions: Vec::new(),
            ..Default::default()
        };

        let specs = resolve_pane_launch_specs(AgentPreset::Shell, Some(&manifest));
        assert_eq!(
            specs[0],
            (
                PaneRole::Agent,
                Some(vec!["claude".to_string(), "--fast".to_string()]),
                Some(AgentPreset::Claude),
            )
        );
        assert_eq!(
            specs[1],
            (
                PaneRole::Service,
                Some(vec![
                    "docker".to_string(),
                    "compose".to_string(),
                    "up".to_string()
                ]),
                None,
            )
        );
        assert_eq!(
            specs[2],
            (
                PaneRole::Runner,
                Some(vec!["cargo".to_string(), "test".to_string()]),
                None,
            )
        );
    }
}
