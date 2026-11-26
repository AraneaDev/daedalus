use super::Registry;
use crate::persistence::{PersistedProjectSetup, PersistedState, PersistedWorkspaceSetup};
use crate::workbench::store::next_revision;
use daedalus_proto::{Assignment, ProtoError, WorkspaceId, WorkspaceInfo};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Default)]
pub(in crate::registry) struct ProjectSetupState {
    folders: BTreeMap<PathBuf, Vec<Assignment>>,
    workspaces: HashMap<WorkspaceId, PersistedWorkspaceSetup>,
}

impl ProjectSetupState {
    pub(in crate::registry) fn folder(&self, root: &Path) -> Option<&Vec<Assignment>> {
        self.folders.get(root)
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

impl Registry {
    /// Saves an explicit setup as the folder default and, for a versioned workspace,
    /// records one workspace revision with the same assignments.
    pub async fn set_project_setup(
        &self,
        workspace_id: WorkspaceId,
        assignments: Vec<Assignment>,
    ) -> Result<(), ProtoError> {
        daedalus_proto::validate_assignments(&assignments).map_err(ProtoError::InvalidRequest)?;
        let lifecycle = self.lock_workspace_lifecycle(workspace_id).await?;
        let mut state = self.workbench(workspace_id).await?;
        let root_dir = self
            .workspaces
            .read()
            .await
            .get(&workspace_id)
            .ok_or(ProtoError::WorkspaceNotFound(workspace_id))?
            .root_dir
            .clone();
        let versioned = if state.revision > 0 {
            state.revision = next_revision(state.revision)?;
            state.document.assignments.clone_from(&assignments);
            Some((workspace_id, state))
        } else {
            None
        };
        self.persist_project_setup(root_dir, assignments, versioned, lifecycle)
            .await
    }

    pub(crate) async fn attach_project_setup(&self, info: &mut WorkspaceInfo) {
        if let Ok(state) = self.workbench(info.id).await
            && state.revision > 0
        {
            info.name = state.document.name;
            info.setup = Some(state.document.assignments);
            return;
        }
        info.setup = self
            .project_setups
            .read()
            .await
            .folder(&info.root_dir)
            .cloned();
    }
}
