use super::documents::{validate_template, validate_workspace};
use crate::persistence::{PersistedWorkspaceSetup, write_json_atomic};
use daedalus_proto::workbench::{WidgetLibrary, WidgetTemplate, WorkbenchState, WorkspaceDocument};
use daedalus_proto::{ProtoError, WorkspaceId};
use serde::de::DeserializeOwned;
use std::collections::{HashMap, HashSet};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub(crate) enum Change<T> {
    Applied(T),
    Conflict(T),
}

pub(crate) enum RepoLink {
    Keep,
    Clear,
    Link(Box<WorkspaceDocument>),
}

#[derive(Default)]
struct Memory {
    workspaces: HashMap<WorkspaceId, WorkbenchState>,
    library: Option<WidgetLibrary>,
}

#[derive(Clone)]
pub(crate) struct WorkbenchStore {
    directory: Option<PathBuf>,
    memory: Arc<Mutex<Memory>>,
}

impl WorkbenchStore {
    pub fn new(directory: Option<PathBuf>) -> Self {
        Self {
            directory,
            memory: Default::default(),
        }
    }

    pub async fn workspace(
        &self,
        id: WorkspaceId,
        initial: WorkspaceDocument,
        setup: Option<PersistedWorkspaceSetup>,
    ) -> Result<WorkbenchState, ProtoError> {
        self.access(move |store, memory| {
            Ok(store
                .read_workspace(memory, id, setup)?
                .unwrap_or(WorkbenchState {
                    revision: 0,
                    document: initial,
                    repo_baseline: None,
                }))
        })
        .await
    }

    pub async fn update_workspace(
        &self,
        id: WorkspaceId,
        revision: Option<u64>,
        document: WorkspaceDocument,
        repo_link: RepoLink,
        setup: Option<PersistedWorkspaceSetup>,
    ) -> Result<Change<WorkbenchState>, ProtoError> {
        validate_workspace(&document)
            .map_err(|error| ProtoError::InvalidRequest(error.to_string()))?;
        self.access(move |store, memory| {
            let current = store.read_workspace(memory, id, setup)?;
            let actual = current.as_ref().map_or(0, |state| state.revision);
            if revision.is_some_and(|expected| actual != expected)
                || revision.is_none() && actual != 0
            {
                let current = match current {
                    Some(state) => state,
                    None => {
                        return Err(ProtoError::InvalidRequest(
                            "workbench has no saved revision".into(),
                        ));
                    }
                };
                return Ok(Change::Conflict(current));
            }
            let state = WorkbenchState {
                revision: next_revision(actual)?,
                document,
                repo_baseline: match repo_link {
                    RepoLink::Keep => current.and_then(|state| state.repo_baseline),
                    RepoLink::Clear => None,
                    RepoLink::Link(baseline) => Some(*baseline),
                },
            };
            if let Some(path) = store.workspace_path(id) {
                write_json_atomic(&path, &state).map_err(io_error)?;
            }
            memory.workspaces.insert(id, state.clone());
            Ok(Change::Applied(state))
        })
        .await
    }

    pub async fn library(&self) -> Result<WidgetLibrary, ProtoError> {
        self.access(|store, memory| store.read_library(memory))
            .await
    }

    pub(crate) async fn discard_unpublished_workspace(
        &self,
        id: WorkspaceId,
    ) -> Result<(), ProtoError> {
        self.access(move |store, memory| {
            memory.workspaces.remove(&id);
            if let Some(path) = store.workspace_path(id)
                && let Err(error) = std::fs::remove_file(&path)
                && error.kind() != ErrorKind::NotFound
            {
                return Err(io_error(error));
            }
            Ok(())
        })
        .await
    }

    pub async fn update_library(
        &self,
        revision: u64,
        templates: Vec<WidgetTemplate>,
    ) -> Result<Change<WidgetLibrary>, ProtoError> {
        let mut ids = HashSet::new();
        for template in &templates {
            validate_template(template)
                .map_err(|error| ProtoError::InvalidRequest(error.to_string()))?;
            if !ids.insert(&template.id) {
                return Err(ProtoError::InvalidRequest(format!(
                    "duplicate template ID '{}'",
                    template.id
                )));
            }
        }
        self.access(move |store, memory| {
            let current = store.read_library(memory)?;
            if current.revision != revision {
                return Ok(Change::Conflict(current));
            }
            let library = WidgetLibrary {
                revision: next_revision(revision)?,
                templates,
            };
            if let Some(path) = store.library_path() {
                write_json_atomic(&path, &library).map_err(io_error)?;
            }
            memory.library = Some(library.clone());
            Ok(Change::Applied(library))
        })
        .await
    }

    async fn access<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&Self, &mut Memory) -> Result<T, ProtoError> + Send + 'static,
    ) -> Result<T, ProtoError> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut memory = store
                .memory
                .lock()
                .map_err(|_| ProtoError::Internal("workbench store lock poisoned".into()))?;
            operation(&store, &mut memory)
        })
        .await
        .map_err(|error| ProtoError::Internal(error.to_string()))?
    }

    fn read_workspace(
        &self,
        memory: &Memory,
        id: WorkspaceId,
        setup: Option<PersistedWorkspaceSetup>,
    ) -> Result<Option<WorkbenchState>, ProtoError> {
        let mut state = if let Some(state) = memory.workspaces.get(&id) {
            Some(state.clone())
        } else {
            load_json(self.workspace_path(id).as_deref())?
        };
        if let (Some(state), Some(setup)) = (&mut state, setup)
            && state.revision < setup.revision
        {
            state.revision = setup.revision;
            state.document.assignments = setup.assignments;
        }
        Ok(state)
    }

    fn read_library(&self, memory: &Memory) -> Result<WidgetLibrary, ProtoError> {
        if let Some(library) = &memory.library {
            return Ok(library.clone());
        }
        Ok(
            load_json(self.library_path().as_deref())?.unwrap_or_else(|| WidgetLibrary {
                revision: 0,
                templates: super::templates::builtins(),
            }),
        )
    }

    fn workspace_path(&self, id: WorkspaceId) -> Option<PathBuf> {
        self.directory
            .as_ref()
            .map(|dir| dir.join(format!("{id}.json")))
    }

    fn library_path(&self) -> Option<PathBuf> {
        self.directory
            .as_ref()
            .map(|dir| dir.join("templates.json"))
    }
}

fn load_json<T: DeserializeOwned>(path: Option<&Path>) -> Result<Option<T>, ProtoError> {
    let Some(path) = path else { return Ok(None) };
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(io_error(err)),
    };
    serde_json::from_slice(&bytes).map(Some).map_err(|error| {
        ProtoError::IoError(format!(
            "invalid workbench data in {}: {error}",
            path.display()
        ))
    })
}

pub(crate) fn next_revision(revision: u64) -> Result<u64, ProtoError> {
    revision
        .checked_add(1)
        .ok_or_else(|| ProtoError::Internal("workbench revision exhausted".into()))
}

fn io_error(error: std::io::Error) -> ProtoError {
    ProtoError::IoError(error.to_string())
}
