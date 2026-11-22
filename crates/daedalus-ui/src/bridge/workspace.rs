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

    if let Some(pane_id) = old_pane_id {
        sink.emit(UiEvent::PaneClosed { pane_id });
    }

    let pane_id = summary.id;
    sink.emit(UiEvent::PaneSpawned {
        summary,
        agent_preset: Some(preset),
    });

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
    let ws_id = ws.id;
    let manifest = ws.manifest.clone();
    sink.emit(UiEvent::WorkspaceCreated(ws));

    spawn_workspace_panes(
        conn,
        sink,
        ws_id,
        requested_agent,
        manifest.as_ref(),
        cols,
        rows,
    )
    .await
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
        let workspaces = match conn.call(ClientMessage::ListWorkspaces).await? {
            ResponsePayload::Workspaces(ws) => ws,
            _ => Vec::new(),
        };
        let Some(ws) = workspaces.into_iter().find(|w| w.id == workspace_id) else {
            return Ok(());
        };
        if !ws.panes.is_empty() {
            for pane in &ws.panes {
                sink.emit(UiEvent::PaneSpawned {
                    summary: pane.clone(),
                    agent_preset: None,
                });
                if let ResponsePayload::Snapshot(snapshot) = conn
                    .call(ClientMessage::GetSnapshot { pane_id: pane.id })
                    .await?
                {
                    sink.emit(UiEvent::Snapshot(snapshot));
                }
            }
            return Ok(());
        }
        let effective_manifest = ws.manifest.or(manifest);
        spawn_workspace_panes(
            conn,
            sink,
            workspace_id,
            default_agent,
            effective_manifest.as_ref(),
            cols,
            rows,
        )
        .await
    }
    .await;

    sink.emit(UiEvent::WorkspacePanesEnsured { workspace_id });
    result
}

async fn spawn_workspace_panes(
    conn: &RpcConnection,
    sink: &Arc<dyn UiEventSink>,
    ws_id: WorkspaceId,
    requested_agent: AgentPreset,
    manifest: Option<&WorkspaceManifest>,
    cols: u16,
    rows: u16,
) -> Result<(), RpcError> {
    for (role, command, mut agent_preset) in resolve_pane_launch_specs(requested_agent, manifest) {
        let spawn_result = conn
            .call(ClientMessage::SpawnPane {
                workspace_id: ws_id,
                role,
                command: command.clone(),
                shell: None,
                cols,
                rows,
                env: HashMap::new(),
            })
            .await;

        let summary = match spawn_result {
            Ok(ResponsePayload::PaneSpawned(summary)) => summary,
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
                match conn
                    .call(ClientMessage::SpawnPane {
                        workspace_id: ws_id,
                        role,
                        command: None,
                        shell: None,
                        cols,
                        rows,
                        env: HashMap::new(),
                    })
                    .await?
                {
                    ResponsePayload::PaneSpawned(summary) => summary,
                    other => {
                        warn!(role = ?role, response = ?other, "unexpected response to fallback SpawnPane");
                        continue;
                    }
                }
            }
            Err(err) => return Err(err),
            Ok(other) => {
                warn!(role = ?role, response = ?other, "unexpected response to SpawnPane");
                continue;
            }
        };

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
            let inferred_preset = manifest_preset
                .or_else(|| AgentPreset::from_cli_name(&cmd[0]))
                .unwrap_or(base_preset);
            (Some(cmd.clone()), inferred_preset)
        }
        None => (base_preset.command(), base_preset),
    };

    let service_cmd = manifest
        .and_then(|m| m.service_command.clone())
        .filter(|c| !c.is_empty());
    let runner_cmd = manifest
        .and_then(|m| m.runner_command.clone())
        .filter(|c| !c.is_empty());

    [
        (PaneRole::Agent, agent_cmd, Some(effective_agent)),
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

    #[tokio::test]
    async fn provisioning_sends_daemon_only_executable() {
        let (client, mut server) = ipc_pair().await;
        let sink: Arc<dyn UiEventSink> = Arc::new(NoopSink);
        let conn = RpcConnection::new(client, sink.clone());
        let command = vec!["agent-only-installed-on-daemon-82f52".to_owned()];
        let manifest = WorkspaceManifest {
            agent_command: Some(command.clone()),
            ..Default::default()
        };
        let request = async {
            let req = read_frame::<_, Request>(&mut server)
                .await
                .unwrap()
                .unwrap();
            assert!(matches!(req.payload, ClientMessage::SpawnPane {
                role: PaneRole::Agent, command: Some(argv), ..
            } if argv == command));
            drop(server);
        };
        let spawn = spawn_workspace_panes(
            &conn,
            &sink,
            WorkspaceId::new(),
            AgentPreset::Shell,
            Some(&manifest),
            80,
            24,
        );
        let (result, ()) = tokio::join!(spawn, request);
        assert!(matches!(result, Err(RpcError::Disconnected)));
        conn.close().await;
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
