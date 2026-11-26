use super::Registry;
use crate::persistence::{PersistedProjectSetup, PersistedState, PersistedWorkspaceSetup};
use crate::workbench::store::next_revision;
use daedalus_proto::{Assignment, ProjectSetupBaseline, ProtoError, WorkspaceId, WorkspaceInfo};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Default)]
pub(in crate::registry) struct ProjectSetupState {
    folders: BTreeMap<PathBuf, Vec<Assignment>>,
    workspaces: HashMap<WorkspaceId, PersistedWorkspaceSetup>,
}

impl ProjectSetupState {
    pub(in crate::registry) fn folder(
        &self,
        root: &Path,
        canonical_root: &Path,
    ) -> Option<&Vec<Assignment>> {
        self.folders
            .get(canonical_root)
            .or_else(|| self.folders.get(root))
    }

    pub(in crate::registry) fn workspace(
        &self,
        id: WorkspaceId,
    ) -> Option<&PersistedWorkspaceSetup> {
        self.workspaces.get(&id)
    }

    pub(in crate::registry) fn confirm(
        &mut self,
        root: PathBuf,
        assignments: Vec<Assignment>,
        workspace: Option<PersistedWorkspaceSetup>,
    ) {
        self.folders.insert(root, assignments);
        if let Some(workspace) = workspace {
            self.workspaces.insert(workspace.workspace_id, workspace);
        }
    }

    pub(in crate::registry) fn save_into(&self, state: &mut PersistedState) {
        state.project_setups = self
            .folders
            .iter()
            .map(|(root, assignments)| PersistedProjectSetup {
                root_dir: root.clone(),
                assignments: assignments.clone(),
            })
            .collect();
        let registered: HashSet<_> = state.workspaces.iter().map(|ws| ws.id).collect();
        state.workspace_setups = self
            .workspaces
            .values()
            .filter(|setup| registered.contains(&setup.workspace_id))
            .cloned()
            .collect();
        state
            .workspace_setups
            .sort_by_key(|setup| setup.workspace_id.0);
    }

    pub(super) fn restore(&mut self, state: &mut PersistedState) {
        self.folders = std::mem::take(&mut state.project_setups)
            .into_iter()
            .map(|setup| (setup.root_dir, setup.assignments))
            .collect();
        self.workspaces = std::mem::take(&mut state.workspace_setups)
            .into_iter()
            .map(|setup| (setup.workspace_id, setup))
            .collect();
    }

    pub(in crate::registry) fn forget_workspace(&mut self, id: WorkspaceId) {
        self.workspaces.remove(&id);
    }
}

pub(super) fn canonical_setup_root(root: &Path) -> PathBuf {
    daedalus_proto::strip_windows_verbatim_prefix(
        std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf()),
    )
}

/// Promote available legacy literal keys. Prefer an existing canonical record
/// when an older snapshot saved different values under both spellings.
pub(super) async fn normalize_setup_roots(state: &mut PersistedState) {
    let mut entries: BTreeMap<_, _> = std::mem::take(&mut state.project_setups)
        .into_iter()
        .map(|setup| (setup.root_dir, setup.assignments))
        .collect();
    let mut roots = entries.keys().cloned();
    let mut checks = tokio::task::JoinSet::new();
    let check = |checks: &mut tokio::task::JoinSet<_>, root: PathBuf| {
        checks.spawn_blocking(move || {
            let canonical = canonical_setup_root(&root);
            (root, canonical)
        });
    };
    for root in roots.by_ref().take(8) {
        check(&mut checks, root);
    }
    // Old closed projects may live on a stale mount. Bound startup and retain
    // any literal keys whose canonical check has not completed by the deadline.
    let deadline = tokio::time::Instant::now() + super::DIR_CHECK_TIMEOUT;
    let mut aliases = BTreeMap::new();
    while let Ok(Some(result)) = tokio::time::timeout_at(deadline, checks.join_next()).await {
        if let Ok((root, canonical)) = result
            && root != canonical
        {
            aliases.insert(root, canonical);
        }
        if let Some(root) = roots.next() {
            check(&mut checks, root);
        }
    }
    for (alias, canonical) in aliases {
        if let Some(assignments) = entries.remove(&alias) {
            entries.entry(canonical).or_insert(assignments);
        }
    }
    state.project_setups = entries
        .into_iter()
        .map(|(root_dir, assignments)| PersistedProjectSetup {
            root_dir,
            assignments,
        })
        .collect();
}

impl Registry {
    /// Saves an explicit setup as the folder default and, for a versioned workspace,
    /// records one workspace revision with the same assignments.
    pub async fn set_project_setup(
        &self,
        workspace_id: WorkspaceId,
        assignments: Vec<Assignment>,
    ) -> Result<(), ProtoError> {
        self.try_set_project_setup(workspace_id, assignments, None)
            .await
            .map(|_| ())
    }

    pub(crate) async fn try_set_project_setup(
        &self,
        workspace_id: WorkspaceId,
        assignments: Vec<Assignment>,
        expected: Option<ProjectSetupBaseline>,
    ) -> Result<Option<ProjectSetupBaseline>, ProtoError> {
        daedalus_proto::validate_assignments(&assignments).map_err(ProtoError::InvalidRequest)?;
        let lifecycle = self.lock_workspace_lifecycle(workspace_id).await?;
        // Two workspace IDs can refer to the same folder. Serialize the
        // baseline check with other writes of that folder's default.
        let mode = self.persist_lock.clone().lock_owned().await;
        let mut state = self.workbench(workspace_id).await?;
        let (root_dir, canonical_root) = {
            let workspaces = self.workspaces.read().await;
            let workspace = workspaces
                .get(&workspace_id)
                .ok_or(ProtoError::WorkspaceNotFound(workspace_id))?;
            (
                workspace.root_dir.clone(),
                workspace.canonical_root_dir.clone(),
            )
        };
        if let Some(expected) = expected {
            let folder = self
                .project_setups
                .read()
                .await
                .folder(&root_dir, &canonical_root)
                .cloned();
            let current = ProjectSetupBaseline {
                assignments: if state.revision > 0 {
                    Some(state.document.assignments.clone())
                } else {
                    folder.clone()
                },
            };
            if current != expected {
                // An unrelated edit can create the first workbench document
                // and materialize the folder defaults without changing setup.
                let unchanged_defaults = state.revision > 0
                    && expected.assignments == folder
                    && state.document.assignments
                        == self.initial_workbench(workspace_id).await?.assignments;
                if !unchanged_defaults {
                    return Ok(Some(current));
                }
            }
        }
        let versioned = if state.revision > 0 {
            state.revision = next_revision(state.revision)?;
            state.document.assignments.clone_from(&assignments);
            Some((workspace_id, state))
        } else {
            None
        };
        self.persist_project_setup(
            root_dir,
            canonical_root,
            assignments,
            versioned,
            lifecycle,
            mode,
        )
        .await?;
        Ok(None)
    }

    pub(crate) async fn attach_project_setup(&self, info: &mut WorkspaceInfo) {
        if let Ok(state) = self.workbench(info.id).await
            && state.revision > 0
        {
            info.name = state.document.name;
            info.setup = Some(state.document.assignments);
            return;
        }
        let canonical_root = self
            .workspaces
            .read()
            .await
            .get(&info.id)
            .map(|workspace| workspace.canonical_root_dir.clone());
        info.setup = self
            .project_setups
            .read()
            .await
            .folder(
                &info.root_dir,
                canonical_root.as_deref().unwrap_or(&info.root_dir),
            )
            .cloned();
    }
}
