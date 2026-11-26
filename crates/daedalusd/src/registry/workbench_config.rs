use super::Registry;
use crate::workbench::scheduler::WidgetCommands;
use crate::workbench::store::{Change, RepoLink};
use daedalus_proto::workbench::{PaneWidgets, WorkbenchState, WorkspaceDocument};
use daedalus_proto::{Assignment, PaneRole, ProtoError, ServerEvent, WorkspaceId};
use std::sync::Arc;

impl Registry {
    pub(crate) async fn workbench(&self, id: WorkspaceId) -> Result<WorkbenchState, ProtoError> {
        let initial = self.initial_workbench(id).await?;
        let setup = self.project_setups.read().await.workspace(id).cloned();
        self.workbench_store.workspace(id, initial, setup).await
    }

    pub(crate) async fn update_workbench(
        &self,
        id: WorkspaceId,
        revision: Option<u64>,
        document: WorkspaceDocument,
    ) -> Result<Change<WorkbenchState>, ProtoError> {
        self.commit_workbench(id, revision, document, RepoLink::Keep)
            .await
    }

    pub(crate) async fn edit_workbench(
        &self,
        id: WorkspaceId,
        revision: Option<u64>,
        edit: impl FnOnce(&mut WorkspaceDocument),
    ) -> Result<Change<WorkbenchState>, ProtoError> {
        let _guard = self.lock_workspace_lifecycle(id).await?;
        let mut state = self.workbench(id).await?;
        if state.revision != revision.unwrap_or(0) {
            return Ok(Change::Conflict(state));
        }
        edit(&mut state.document);
        self.commit_workbench_locked(id, revision, state.document, RepoLink::Keep)
            .await
    }

    pub(super) async fn commit_workbench(
        &self,
        id: WorkspaceId,
        revision: Option<u64>,
        document: WorkspaceDocument,
        repo_link: RepoLink,
    ) -> Result<Change<WorkbenchState>, ProtoError> {
        let _guard = self.lock_workspace_lifecycle(id).await?;
        self.commit_workbench_locked(id, revision, document, repo_link)
            .await
    }

    pub(super) async fn commit_workbench_locked(
        &self,
        id: WorkspaceId,
        revision: Option<u64>,
        document: WorkspaceDocument,
        repo_link: RepoLink,
    ) -> Result<Change<WorkbenchState>, ProtoError> {
        let widget_commands = self.workbench_widget_commands(id).await?;
        let setup = self.project_setups.read().await.workspace(id).cloned();
        let result = self
            .workbench_store
            .update_workspace(id, revision, document, repo_link, setup)
            .await?;
        if let Change::Applied(state) = &result {
            self.project_setups.write().await.forget_workspace(id);
            Self::publish_workbench_change(&self.event_tx, id, &widget_commands, state);
        }
        Ok(result)
    }

    pub(super) async fn workbench_widget_commands(
        &self,
        id: WorkspaceId,
    ) -> Result<Arc<WidgetCommands>, ProtoError> {
        Ok(self
            .workspaces
            .read()
            .await
            .get(&id)
            .ok_or(ProtoError::WorkspaceNotFound(id))?
            .widget_commands
            .clone())
    }

    pub(super) fn publish_workbench_change(
        events: &tokio::sync::broadcast::Sender<ServerEvent>,
        id: WorkspaceId,
        widget_commands: &WidgetCommands,
        state: &WorkbenchState,
    ) {
        widget_commands.reconcile(state);
        let _ = events.send(ServerEvent::WorkbenchChanged {
            workspace_id: id,
            revision: state.revision,
        });
    }

    pub(super) async fn initial_workbench(
        &self,
        id: WorkspaceId,
    ) -> Result<WorkspaceDocument, ProtoError> {
        let (mut document, root, canonical_root, manifest) = {
            let workspaces = self.workspaces.read().await;
            let workspace = workspaces
                .get(&id)
                .ok_or(ProtoError::WorkspaceNotFound(id))?;
            (
                WorkspaceDocument::new(workspace.name.clone()),
                workspace.root_dir.clone(),
                workspace.canonical_root_dir.clone(),
                workspace.manifest.clone(),
            )
        };
        let saved = self
            .project_setups
            .read()
            .await
            .folder(&root, &canonical_root)
            .cloned();
        if let Some(manifest) = manifest {
            if manifest.service_widget.is_some() || manifest.runner_widget.is_some() {
                document.pane_widgets = Some(PaneWidgets {
                    service: manifest.service_widget.unwrap_or_default(),
                    runner: manifest.runner_widget.unwrap_or_default(),
                    ..PaneWidgets::default()
                });
            }
            let agent_key = if manifest.agent_command.as_deref().is_some_and(|argv| {
                matches!(argv, [program] if daedalus_proto::canonical_agent_preset(program) == Some("shell"))
            }) {
                "agent-command"
            } else {
                "agent"
            };
            if saved.is_none() {
                document.assignments = [
                    (
                        PaneRole::Agent,
                        agent_key,
                        "Agent",
                        manifest.agent_command.or_else(|| {
                            let preset = daedalus_proto::canonical_agent_preset(
                                manifest.agent_preset.as_deref()?,
                            )?;
                            Some(vec![preset.to_owned()])
                        }),
                    ),
                    (
                        PaneRole::Service,
                        "service",
                        "Service",
                        manifest.service_command,
                    ),
                    (
                        PaneRole::Runner,
                        "runner",
                        "Runner",
                        manifest.runner_command,
                    ),
                ]
                .into_iter()
                .filter_map(|(role, key, name, argv)| {
                    Some(Assignment {
                        id: format!("manifest-{key}"),
                        name: name.into(),
                        role,
                        argv: argv?,
                    })
                })
                .collect();
            }
        }
        if let Some(assignments) = saved {
            document.assignments = assignments;
        }
        Ok(document)
    }
}
