use super::Registry;
use crate::workbench::documents::{parse_workspace, serialize_workspace, validate_workspace};
use crate::workbench::merge::merge_workspace;
use crate::workbench::store::{Change, RepoLink};
use daedalus_proto::workbench::{
    ConflictChoice, SHARED_WORKSPACE_FILE, WorkbenchState, WorkspaceCreationPreview,
    WorkspaceDocument, WorkspaceImportPreview, WorkspaceImportSource,
};
use daedalus_proto::{ProtoError, ServerEvent, WorkspaceId, WorkspaceInfo};
use std::collections::BTreeMap;

impl Registry {
    pub(crate) async fn preview_workspace_creation(
        &self,
        root_dir: std::path::PathBuf,
        source: WorkspaceImportSource,
        linked: bool,
    ) -> Result<WorkspaceCreationPreview, ProtoError> {
        let root_dir = std::path::absolute(root_dir)
            .map_err(|error| ProtoError::IoError(error.to_string()))?;
        if !root_dir.is_dir() {
            return Err(ProtoError::InvalidRequest(format!(
                "Workspace folder does not exist: {}",
                root_dir.display()
            )));
        }
        let incoming = match source {
            WorkspaceImportSource::Repo => {
                read_workspace_path(&root_dir.join(SHARED_WORKSPACE_FILE)).await?
            }
            WorkspaceImportSource::Text(text) if !linked => {
                parse_workspace(&text).map_err(document_error)?
            }
            WorkspaceImportSource::Text(_) => {
                return Err(ProtoError::InvalidRequest(
                    "A linked workspace must be imported from the repo".into(),
                ));
            }
        };
        Ok(WorkspaceCreationPreview {
            root_dir,
            incoming,
            linked,
        })
    }

    pub(crate) async fn apply_workspace_creation(
        &self,
        preview: WorkspaceCreationPreview,
    ) -> Result<WorkspaceInfo, ProtoError> {
        validate_workspace(&preview.incoming).map_err(document_error)?;
        if preview.linked
            && read_workspace_path(&preview.root_dir.join(SHARED_WORKSPACE_FILE)).await?
                != preview.incoming
        {
            return Err(ProtoError::InvalidRequest(
                "The repo workspace file changed. Preview the import again.".into(),
            ));
        }
        let create_guard = self.create_lock.lock().await;
        let record = self
            .prepare_workspace_record(
                preview.incoming.name.clone(),
                preview.root_dir,
                Default::default(),
                false,
                None,
            )
            .await?;
        let link = if preview.linked {
            RepoLink::Link(Box::new(preview.incoming.clone()))
        } else {
            RepoLink::Clear
        };
        // Write the first revision by its private ID. A failed write drops the
        // prepared record without adding membership, reserving a slot, or
        // recording a recent project.
        let Change::Applied(state) = self
            .workbench_store
            .update_workspace(record.id, Some(0), preview.incoming, link, None)
            .await?
        else {
            return Err(ProtoError::Internal(
                "New workspace ID already has a workbench revision".into(),
            ));
        };
        record.widget_commands.reconcile(&state);
        let id = record.id;
        let mut workspace = match self.publish_workspace(record).await {
            Ok(workspace) => workspace,
            Err(error) => {
                if let Err(cleanup) = self.workbench_store.discard_unpublished_workspace(id).await {
                    return Err(ProtoError::IoError(format!(
                        "{error}. Could not remove the unpublished workbench document for {id}: {cleanup}"
                    )));
                }
                return Err(error);
            }
        };
        let _ = self.event_tx.send(ServerEvent::WorkbenchChanged {
            workspace_id: workspace.id,
            revision: state.revision,
        });
        drop(create_guard);
        workspace.setup = Some(state.document.assignments);
        Ok(workspace)
    }
    pub(crate) async fn workspace_repo_status(
        &self,
        id: WorkspaceId,
    ) -> Result<Option<bool>, ProtoError> {
        let path = self.shared_workbench_path(id).await?;
        let text = match tokio::fs::read_to_string(path).await {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(ProtoError::IoError(error.to_string())),
        };
        let state = self.workbench(id).await?;
        // Even a malformed file should be offered for inspection, where the parser can explain it.
        Ok(Some(parse_workspace(&text).map_or(true, |incoming| {
            state.repo_baseline.as_ref() != Some(&incoming)
        })))
    }
    pub(crate) async fn preview_workspace_import(
        &self,
        id: WorkspaceId,
        source: WorkspaceImportSource,
        linked: bool,
    ) -> Result<WorkspaceImportPreview, ProtoError> {
        let state = self.workbench(id).await?;
        let incoming = match source {
            WorkspaceImportSource::Repo => self.read_repo_workbench(id).await?,
            WorkspaceImportSource::Text(text) if !linked => {
                parse_workspace(&text).map_err(document_error)?
            }
            WorkspaceImportSource::Text(_) => {
                return Err(ProtoError::InvalidRequest(
                    "A linked workspace must be imported from the repo".into(),
                ));
            }
        };
        import_preview(id, state, incoming, linked, &BTreeMap::new())
    }

    pub(crate) async fn resolve_workspace_import(
        &self,
        id: WorkspaceId,
        revision: u64,
        incoming: WorkspaceDocument,
        linked: bool,
        resolutions: BTreeMap<String, ConflictChoice>,
    ) -> Result<WorkspaceImportPreview, ProtoError> {
        let state = self.workbench(id).await?;
        if state.revision != revision {
            return Err(ProtoError::InvalidRequest(
                "The workspace changed. Preview the import again.".into(),
            ));
        }
        self.check_linked_import(id, &incoming, linked).await?;
        import_preview(id, state, incoming, linked, &resolutions)
    }

    async fn check_linked_import(
        &self,
        id: WorkspaceId,
        incoming: &WorkspaceDocument,
        linked: bool,
    ) -> Result<(), ProtoError> {
        if linked && self.read_repo_workbench(id).await? != *incoming {
            return Err(ProtoError::InvalidRequest(
                "The repo workspace file changed. Preview the import again.".into(),
            ));
        }
        Ok(())
    }

    pub(crate) async fn apply_workspace_import(
        &self,
        id: WorkspaceId,
        revision: u64,
        incoming: WorkspaceDocument,
        linked: bool,
        resolutions: BTreeMap<String, ConflictChoice>,
    ) -> Result<Change<WorkbenchState>, ProtoError> {
        let state = self.workbench(id).await?;
        if state.revision != revision {
            return Ok(Change::Conflict(state));
        }
        self.check_linked_import(id, &incoming, linked).await?;
        let (document, conflicts) = reconcile(&state, &incoming, linked, &resolutions)?;
        if !conflicts.is_empty() {
            let fields = conflicts
                .iter()
                .map(|conflict| conflict.path.as_str())
                .collect::<Vec<_>>();
            return Err(ProtoError::InvalidRequest(format!(
                "Resolve workspace conflicts at {}",
                fields.join(", ")
            )));
        }
        self.commit_workbench(
            id,
            Some(revision),
            document,
            if linked {
                RepoLink::Link(Box::new(incoming))
            } else {
                RepoLink::Clear
            },
        )
        .await
    }

    pub(crate) async fn export_workbench(&self, id: WorkspaceId) -> Result<String, ProtoError> {
        let state = self.workbench(id).await?;
        serialize_workspace(&state.document).map_err(document_error)
    }

    async fn read_repo_workbench(&self, id: WorkspaceId) -> Result<WorkspaceDocument, ProtoError> {
        let path = self.shared_workbench_path(id).await?;
        read_workspace_path(&path).await
    }

    async fn shared_workbench_path(
        &self,
        id: WorkspaceId,
    ) -> Result<std::path::PathBuf, ProtoError> {
        Ok(self
            .workspaces
            .read()
            .await
            .get(&id)
            .ok_or(ProtoError::WorkspaceNotFound(id))?
            .root_dir
            .join(SHARED_WORKSPACE_FILE))
    }
}

async fn read_workspace_path(path: &std::path::Path) -> Result<WorkspaceDocument, ProtoError> {
    let text = tokio::fs::read_to_string(path)
        .await
        .map_err(|error| ProtoError::IoError(format!("{}: {error}", path.display())))?;
    parse_workspace(&text).map_err(document_error)
}

fn import_preview(
    id: WorkspaceId,
    state: WorkbenchState,
    incoming: WorkspaceDocument,
    linked: bool,
    resolutions: &BTreeMap<String, ConflictChoice>,
) -> Result<WorkspaceImportPreview, ProtoError> {
    let (proposed, conflicts) = reconcile(&state, &incoming, linked, resolutions)?;
    Ok(WorkspaceImportPreview {
        workspace_id: id,
        revision: state.revision,
        incoming,
        current: state.document,
        proposed,
        linked,
        conflicts,
    })
}

fn reconcile(
    state: &WorkbenchState,
    incoming: &WorkspaceDocument,
    linked: bool,
    resolutions: &BTreeMap<String, ConflictChoice>,
) -> Result<
    (
        WorkspaceDocument,
        Vec<daedalus_proto::workbench::MergeConflict>,
    ),
    ProtoError,
> {
    let (document, conflicts) = if linked && let Some(base) = &state.repo_baseline {
        merge_workspace(base, &state.document, incoming, resolutions).map_err(|error| {
            ProtoError::InvalidRequest(format!("Merged workspace is invalid: {error}"))
        })?
    } else {
        (incoming.clone(), Vec::new())
    };
    validate_workspace(&document).map_err(document_error)?;
    Ok((document, conflicts))
}

pub(crate) fn document_error(error: crate::workbench::documents::DocumentError) -> ProtoError {
    ProtoError::InvalidRequest(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::StateStore;
    use crate::shell_integration::ShellIntegration;
    use crate::supervisor::{IsolationMode, Supervisor};
    use std::path::Path;
    use tempfile::TempDir;
    use tokio::sync::broadcast;

    fn registry(root: &Path) -> Registry {
        #[cfg(unix)]
        let isolation = IsolationMode::PgidFallback;
        #[cfg(windows)]
        let isolation = IsolationMode::WindowsProcessTree;
        Registry::new_with_state_store(
            Supervisor::with_mode(isolation),
            ShellIntegration::prepare(root).unwrap(),
            broadcast::channel(16).0,
            Some(StateStore::new(root.join("state.json"))),
        )
    }

    fn preview(root: &Path) -> WorkspaceCreationPreview {
        WorkspaceCreationPreview {
            root_dir: root.to_owned(),
            incoming: WorkspaceDocument::new("Reviewed import".into()),
            linked: false,
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn imported_assignment_is_ready_when_membership_is_published() {
        use daedalus_proto::{Assignment, PaneRole, ResponsePayload};
        use std::sync::Arc;
        use std::time::Duration;

        let dir = TempDir::new().unwrap();
        let registry = Arc::new(registry(dir.path()));
        let mut preview = preview(dir.path());
        preview.incoming.assignments.push(Assignment {
            id: "reviewed".into(),
            name: "Reviewed runner".into(),
            role: PaneRole::Runner,
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                "printf reviewed > marker".into(),
            ],
        });
        let assignments = preview.incoming.assignments.clone();
        let mut events = registry.subscribe_events();

        // Membership must wait for persistence and expose the reviewed assignment.
        let hold = registry.persist_lock.clone().lock_owned().await;
        let apply = tokio::spawn({
            let registry = registry.clone();
            async move { registry.apply_workspace_creation(preview).await }
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(50), events.recv())
                .await
                .is_err()
        );
        assert!(registry.list_workspaces().await.is_empty());
        drop(hold);
        let event = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(event, ServerEvent::WorkspaceMembershipChanged);
        let listed = registry.list_workspaces().await;
        assert_eq!(listed.len(), 1);
        let id = listed[0].id;
        assert_eq!(registry.workbench(id).await.unwrap().revision, 1);
        assert_eq!(listed[0].setup.as_deref(), Some(assignments.as_slice()));
        let pane = registry
            .ensure_default_pane(
                id,
                PaneRole::Runner,
                Some(vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "printf fallback > marker".into(),
                ]),
                80,
                24,
            )
            .await
            .unwrap();
        assert!(matches!(pane, ResponsePayload::PaneSpawned(_)));
        let marker = dir.path().join("marker");
        let data = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(data) = std::fs::read_to_string(&marker)
                    && !data.is_empty()
                {
                    break data;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let imported = apply.await.unwrap().unwrap();
        assert_eq!(imported.id, id);
        assert_eq!(imported.setup, Some(assignments));
        registry.close_all().await;
        assert_eq!(data, "reviewed");
    }

    #[tokio::test]
    async fn failed_registry_snapshot_does_not_publish_an_import_or_keep_its_document() {
        let dir = TempDir::new().unwrap();
        let registry = registry(dir.path());
        let known = registry
            .create_workspace("Known".into(), dir.path().into(), Default::default())
            .await
            .unwrap();
        let state_path = dir.path().join("state.json");
        std::fs::rename(&state_path, dir.path().join("state.backup")).unwrap();
        std::fs::create_dir(state_path).unwrap();
        let mut events = registry.subscribe_events();

        let error = registry
            .apply_workspace_creation(preview(dir.path()))
            .await
            .unwrap_err();
        assert!(matches!(error, ProtoError::IoError(_)), "{error:?}");
        assert_eq!(registry.list_workspaces().await.len(), 1);
        assert_eq!(registry.list_workspaces().await[0].id, known.id);
        assert!(
            std::fs::read_dir(dir.path().join("workbench"))
                .unwrap()
                .next()
                .is_none()
        );
        assert!(matches!(
            events.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn failed_import_write_leaves_no_workspace_or_recent_project() {
        let dir = TempDir::new().unwrap();
        let registry = registry(dir.path());
        let mut events = registry.subscribe_events();
        let blocking_file = dir.path().join("workbench");
        std::fs::write(&blocking_file, b"workbench directory blocked").unwrap();

        let error = registry
            .apply_workspace_creation(preview(dir.path()))
            .await
            .unwrap_err();
        assert!(matches!(error, ProtoError::IoError(_)), "{error:?}");
        assert!(registry.list_workspaces().await.is_empty());
        assert!(registry.recent_projects().await.is_empty());
        assert!(matches!(
            events.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));

        std::fs::remove_file(blocking_file).unwrap();
        let workspace = registry
            .apply_workspace_creation(preview(dir.path()))
            .await
            .unwrap();
        assert_eq!(workspace.slot_index, 0);
        assert_eq!(registry.workbench(workspace.id).await.unwrap().revision, 1);
    }
}
