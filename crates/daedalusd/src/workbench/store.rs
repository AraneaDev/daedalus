use super::documents::{validate_template, validate_workspace};
use crate::persistence::{PersistedWorkspaceSetup, write_json_atomic};
use daedalus_proto::workbench::{WidgetLibrary, WidgetTemplate, WorkbenchState, WorkspaceDocument};
use daedalus_proto::{ProtoError, WorkspaceId};
use serde::de::DeserializeOwned;
use std::collections::{HashMap, HashSet, VecDeque};
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

const DISK_CACHE_CAPACITY: usize = 128;

#[derive(Default)]
struct Memory {
    workspaces: HashMap<WorkspaceId, WorkbenchState>,
    insertion_order: VecDeque<WorkspaceId>,
    library: Option<WidgetLibrary>,
    #[cfg(test)]
    workspace_reads: usize,
}

impl Memory {
    fn forget(&mut self, id: WorkspaceId) {
        self.workspaces.remove(&id);
        self.insertion_order.retain(|entry| *entry != id);
    }
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
        is_open: impl FnOnce() -> bool + Send + 'static,
    ) -> Result<WorkbenchState, ProtoError> {
        self.access(move |store, memory| {
            if !is_open() {
                return Err(ProtoError::WorkspaceNotFound(id));
            }
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
            store.remember(memory, id, &state);
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
            memory.forget(id);
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

    pub(crate) fn evict_workspace(&self, id: WorkspaceId) {
        self.memory
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .forget(id);
    }

    // Daemon APIs own managed state while a workspace is open. Closing drops
    // its cache entry so a later registration reads the durable document again.
    fn remember(&self, memory: &mut Memory, id: WorkspaceId, state: &WorkbenchState) {
        // Without a directory the map itself is the authoritative store.
        if self.directory.is_some() && !memory.workspaces.contains_key(&id) {
            if memory.insertion_order.len() == DISK_CACHE_CAPACITY
                && let Some(oldest) = memory.insertion_order.pop_front()
            {
                memory.workspaces.remove(&oldest);
            }
            memory.insertion_order.push_back(id);
        }
        memory.workspaces.insert(id, state.clone());
    }

    fn read_workspace(
        &self,
        memory: &mut Memory,
        id: WorkspaceId,
        setup: Option<PersistedWorkspaceSetup>,
    ) -> Result<Option<WorkbenchState>, ProtoError> {
        let mut state = if let Some(state) = memory.workspaces.get(&id) {
            Some(state.clone())
        } else {
            #[cfg(test)]
            if self.directory.is_some() {
                memory.workspace_reads += 1;
            }
            let state = load_json(self.workspace_path(id).as_deref())?;
            if let Some(raw) = &state {
                self.remember(memory, id, raw);
            }
            state
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn restored_workbench_cache_discards_stale_scheduler_reads_on_close() {
        use crate::workbench::scheduler::WidgetCommands;
        let root = tempfile::tempdir().unwrap();
        let store = WorkbenchStore::new(Some(root.path().into()));
        let id = WorkspaceId::new();
        let raw = WorkbenchState {
            revision: 2,
            document: WorkspaceDocument::new("Fixture".into()),
            repo_baseline: None,
        };
        let path = store.workspace_path(id).unwrap();
        write_json_atomic(&path, &raw).unwrap();
        let registration = Arc::new(WidgetCommands::default());
        store
            .workspace(id, raw.document.clone(), None, || true)
            .await
            .unwrap();
        let pending = {
            let store = store.clone();
            let snapshot = registration.clone();
            let initial = raw.document.clone();
            let (resume, paused) = tokio::sync::oneshot::channel();
            let reader = tokio::spawn(async move {
                paused.await.unwrap();
                store
                    .workspace(id, initial, None, move || snapshot.is_open())
                    .await
            });
            (resume, reader)
        };
        registration.close();
        store.evict_workspace(id);
        pending.0.send(()).unwrap();
        assert!(
            matches!(pending.1.await.unwrap(), Err(ProtoError::WorkspaceNotFound(missing)) if missing == id)
        );
        {
            let memory = store.memory.lock().unwrap();
            assert!(!memory.workspaces.contains_key(&id));
            assert_eq!(memory.workspace_reads, 1);
        }
        let newer = WorkbenchState { revision: 8, ..raw };
        write_json_atomic(&path, &newer).unwrap();
        assert_eq!(
            store
                .workspace(id, newer.document.clone(), None, || true)
                .await
                .unwrap(),
            newer
        );
        assert_eq!(store.memory.lock().unwrap().workspace_reads, 2);
    }

    #[test]
    fn restored_workbench_cache_bounds_only_file_backed_entries() {
        let root = tempfile::tempdir().unwrap();
        for directory in [Some(root.path().to_owned()), None] {
            let persistent = directory.is_some();
            let store = WorkbenchStore::new(directory);
            let mut memory = store.memory.lock().unwrap();
            let first = WorkspaceId::new();
            let raw = WorkbenchState {
                revision: 1,
                document: WorkspaceDocument::new("Fixture".into()),
                repo_baseline: None,
            };
            store.remember(&mut memory, first, &raw);
            for _ in 0..DISK_CACHE_CAPACITY {
                store.remember(&mut memory, WorkspaceId::new(), &raw);
            }
            assert_eq!(memory.workspaces.contains_key(&first), !persistent);
            assert_eq!(
                memory.workspaces.len(),
                DISK_CACHE_CAPACITY + usize::from(!persistent)
            );
        }
    }

    #[tokio::test]
    async fn restored_workbench_cache_reads_raw_state_once_and_preserves_failed_updates() {
        let root = tempfile::tempdir().unwrap();
        let store = WorkbenchStore::new(Some(root.path().into()));
        let id = WorkspaceId::new();
        let path = store.workspace_path(id).unwrap();
        let raw = WorkbenchState {
            revision: 2,
            document: WorkspaceDocument::new("Fixture".into()),
            repo_baseline: None,
        };
        write_json_atomic(&path, &raw).unwrap();
        {
            let mut memory = store.memory.lock().unwrap();
            for revision in [3, 4, 2, 1] {
                let setup = PersistedWorkspaceSetup {
                    workspace_id: id,
                    revision,
                    assignments: vec![daedalus_proto::Assignment {
                        id: "runner".into(),
                        name: format!("Runner {revision}"),
                        role: daedalus_proto::PaneRole::Runner,
                        argv: vec!["test".into()],
                    }],
                };
                let observed = store
                    .read_workspace(&mut memory, id, Some(setup.clone()))
                    .unwrap()
                    .unwrap();
                if revision > raw.revision {
                    assert_eq!(observed.revision, revision);
                    assert_eq!(observed.document.assignments, setup.assignments);
                } else {
                    assert_eq!(observed, raw);
                }
            }
            assert_eq!(
                store.read_workspace(&mut memory, id, None).unwrap(),
                Some(raw.clone())
            );
            assert_eq!(memory.workspace_reads, 1);
        }
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(path).unwrap();
        assert!(matches!(
            store
                .update_workspace(id, Some(2), raw.document.clone(), RepoLink::Keep, None)
                .await,
            Err(ProtoError::IoError(_))
        ));
        let mut memory = store.memory.lock().unwrap();
        assert_eq!(
            store.read_workspace(&mut memory, id, None).unwrap(),
            Some(raw)
        );
        assert_eq!(memory.workspace_reads, 1);
    }
}
