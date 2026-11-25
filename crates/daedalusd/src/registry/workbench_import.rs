use super::Registry;
use crate::workbench::documents::{parse_workspace, serialize_workspace, validate_workspace};
use crate::workbench::merge::merge_workspace;
use crate::workbench::store::{Change, RepoLink};
use daedalus_proto::workbench::{
    ConflictChoice, SHARED_WORKSPACE_FILE, WorkbenchState, WorkspaceCreationPreview,
    WorkspaceDocument, WorkspaceImportPreview, WorkspaceImportSource,
};
use daedalus_proto::{ProtoError, WorkspaceId, WorkspaceInfo};
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
        let mut workspace = self
            .create_workspace(
                preview.incoming.name.clone(),
                preview.root_dir,
                Default::default(),
            )
            .await?;
        let link = if preview.linked {
            RepoLink::Link(Box::new(preview.incoming.clone()))
        } else {
            RepoLink::Clear
        };
        match self
            .commit_workbench(workspace.id, Some(0), preview.incoming, link)
            .await
        {
            Ok(Change::Applied(state)) => {
                workspace.setup = Some(state.document.assignments);
                Ok(workspace)
            }
            Err(error) => {
                let _ = self
                    .close_workspace(workspace.id, false, false, false)
                    .await;
                Err(error)
            }
            Ok(Change::Conflict(_)) => Err(ProtoError::InvalidRequest(
                "The new workspace changed before import completed.".into(),
            )),
        }
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
