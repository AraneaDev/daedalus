use super::Registry;
use crate::workbench::store::{Change, RepoLink};
use daedalus_proto::workbench::{PaneWidgets, WorkbenchState, WorkspaceDocument};
use daedalus_proto::{Assignment, PaneRole, ProtoError, ServerEvent, WorkspaceId};

impl Registry {
    pub(crate) async fn workbench(&self, id: WorkspaceId) -> Result<WorkbenchState, ProtoError> {
        let initial = self.initial_workbench(id).await?;
        self.workbench_store.workspace(id, initial).await
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

    pub(super) async fn commit_workbench(
        &self,
        id: WorkspaceId,
        revision: Option<u64>,
        document: WorkspaceDocument,
        repo_link: RepoLink,
    ) -> Result<Change<WorkbenchState>, ProtoError> {
        let lifecycle = self
            .workspaces
            .read()
            .await
            .get(&id)
            .ok_or(ProtoError::WorkspaceNotFound(id))?
            .lifecycle
            .clone();
        let _guard = lifecycle.lock().await;
        if !self.workspaces.read().await.contains_key(&id) {
            return Err(ProtoError::WorkspaceNotFound(id));
        }
        let result = self
            .workbench_store
            .update_workspace(id, revision, document, repo_link)
            .await?;
        if let Change::Applied(state) = &result {
            let _ = self.event_tx.send(ServerEvent::WorkbenchChanged {
                workspace_id: id,
                revision: state.revision,
            });
        }
        Ok(result)
    }

    async fn initial_workbench(&self, id: WorkspaceId) -> Result<WorkspaceDocument, ProtoError> {
        let (mut document, root, manifest) = {
            let workspaces = self.workspaces.read().await;
            let workspace = workspaces
                .get(&id)
                .ok_or(ProtoError::WorkspaceNotFound(id))?;
            (
                WorkspaceDocument::new(workspace.name.clone()),
                workspace.root_dir.clone(),
                workspace.manifest.clone(),
            )
        };
        if let Some(assignments) = self.project_setups.read().await.get(&root) {
            document.assignments.clone_from(assignments);
        }
        if let Some(manifest) = manifest {
            if manifest.service_widget.is_some() || manifest.runner_widget.is_some() {
                document.pane_widgets = Some(PaneWidgets {
                    service: manifest.service_widget.unwrap_or_default(),
                    runner: manifest.runner_widget.unwrap_or_default(),
                    ..PaneWidgets::default()
                });
            }
            for (role, key, name, argv) in [
                (
                    PaneRole::Agent,
                    "agent",
                    "Agent",
                    manifest
                        .agent_command
                        .or_else(|| manifest.agent_preset.map(|preset| vec![preset])),
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
            ] {
                if !document
                    .assignments
                    .iter()
                    .any(|assignment| assignment.role == role)
                    && let Some(argv) = argv
                {
                    let mut id = format!("manifest-{key}");
                    while document
                        .assignments
                        .iter()
                        .any(|assignment| assignment.id == id)
                    {
                        id.push('-');
                    }
                    document.assignments.push(Assignment {
                        id,
                        name: name.into(),
                        role,
                        argv,
                    });
                }
            }
        }
        Ok(document)
    }
}
