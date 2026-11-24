//! One place that decides where a workspace's pane setup lives: on the daemon, per project
//! folder, when it supports that, and in local files otherwise.

use crate::assignments::{self, Assignment};
use crate::bridge::UiCommand;
use daedalus_proto::{WorkspaceId, types::Workspace, validate_assignments};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub struct ProjectSetups {
    /// Directory of the local `<id>.toml` files. `None` follows the configured location.
    dir: Option<PathBuf>,
    daemon_backed: bool,
    cache: HashMap<WorkspaceId, Vec<Assignment>>,
    roots: HashMap<WorkspaceId, PathBuf>,
    migrated: HashSet<WorkspaceId>,
    migrated_roots: HashSet<PathBuf>,
}

impl ProjectSetups {
    pub fn in_dir(dir: PathBuf) -> Self {
        Self {
            dir: Some(dir),
            ..Self::default()
        }
    }

    fn local_path(&self, id: WorkspaceId) -> PathBuf {
        match &self.dir {
            Some(dir) => assignments::file_in(dir, id),
            None => assignments::file_in(&assignments::assignments_dir(), id),
        }
    }

    /// Called once per connection. A new connection starts from what the daemon reports.
    pub fn set_daemon_backed(&mut self, on: bool) {
        self.daemon_backed = on;
        self.cache.clear();
        self.roots.clear();
        self.migrated.clear();
        self.migrated_roots.clear();
    }

    pub fn absorb<P>(&mut self, workspaces: &[Workspace<P>]) {
        for info in workspaces {
            self.roots.insert(info.id, info.root_dir.clone());
            if let Some(setup) = &info.setup {
                self.cache.insert(info.id, setup.clone());
            }
        }
    }

    pub fn changed<P>(
        &mut self,
        root_dir: &Path,
        assignments: Vec<Assignment>,
        workspaces: &[Workspace<P>],
    ) {
        self.absorb(workspaces);
        let ids: Vec<_> = self
            .roots
            .iter()
            .filter(|(_, root)| root.as_path() == root_dir)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.cache.insert(id, assignments.clone());
        }
    }

    pub fn load(&self, id: WorkspaceId) -> Result<Vec<Assignment>, String> {
        self.load_or(id, Vec::new())
    }

    pub fn load_or(
        &self,
        id: WorkspaceId,
        defaults: Vec<Assignment>,
    ) -> Result<Vec<Assignment>, String> {
        if self.daemon_backed {
            return Ok(self.cache.get(&id).cloned().unwrap_or(defaults));
        }
        assignments::load_from(&self.local_path(id), defaults)
    }

    /// Daemon-backed saves return the command to send; local saves are written immediately.
    pub fn save(
        &mut self,
        id: WorkspaceId,
        entries: Vec<Assignment>,
    ) -> Result<Option<UiCommand>, String> {
        if !self.daemon_backed {
            assignments::save_to(&self.local_path(id), &entries)?;
            return Ok(None);
        }
        validate_assignments(&entries)?;
        self.store(id, entries.clone());
        Ok(Some(UiCommand::SaveProjectSetup {
            workspace_id: id,
            assignments: entries,
        }))
    }

    fn store(&mut self, id: WorkspaceId, entries: Vec<Assignment>) {
        let siblings: Vec<_> = match self.roots.get(&id) {
            Some(root) => self
                .roots
                .iter()
                .filter(|(_, other)| *other == root)
                .map(|(other, _)| *other)
                .collect(),
            None => Vec::new(),
        };
        for sibling in siblings {
            self.cache.insert(sibling, entries.clone());
        }
        self.cache.insert(id, entries);
    }

    /// Local setups the daemon does not have yet. Each workspace, and each project folder, is
    /// offered once per connection, and local files are left untouched.
    pub fn migrations<P>(
        &mut self,
        workspaces: &[Workspace<P>],
    ) -> Vec<(WorkspaceId, Vec<Assignment>)> {
        if !self.daemon_backed {
            return Vec::new();
        }
        let mut found = Vec::new();
        for info in workspaces {
            if info.setup.is_some()
                || self.migrated.contains(&info.id)
                || self.migrated_roots.contains(&info.root_dir)
            {
                continue;
            }
            let path = self.local_path(info.id);
            if !path.exists() {
                continue;
            }
            let Ok(entries) = assignments::load_from(&path, Vec::new()) else {
                continue;
            };
            self.migrated.insert(info.id);
            self.migrated_roots.insert(info.root_dir.clone());
            self.roots.insert(info.id, info.root_dir.clone());
            self.cache.insert(info.id, entries.clone());
            found.push((info.id, entries));
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daedalus_proto::{PaneRole, WorkspaceInfo};

    fn entry(id: &str) -> Assignment {
        Assignment {
            id: id.into(),
            name: format!("Run {id}"),
            role: PaneRole::Runner,
            argv: vec!["cargo".into(), "test".into()],
        }
    }

    fn info(root: &str, setup: Option<Vec<Assignment>>) -> WorkspaceInfo {
        WorkspaceInfo {
            id: WorkspaceId::new(),
            name: "w".into(),
            root_dir: root.into(),
            slot_index: 0,
            injected_env: Default::default(),
            panes: vec![],
            default_panes_pending: None,
            port_offset: 0,
            compose_project_name: None,
            git_branch: None,
            git_status: None,
            project: Default::default(),
            setup,
            bound_ports: vec![],
            containers: vec![],
            external_statuses: vec![],
            manifest: None,
        }
    }

    fn store() -> (tempfile::TempDir, ProjectSetups) {
        let dir = tempfile::tempdir().unwrap();
        let store = ProjectSetups::in_dir(dir.path().join("assignments"));
        (dir, store)
    }

    #[test]
    fn file_backed_store_matches_todays_behaviour() {
        let (dir, mut store) = store();
        let id = WorkspaceId::new();
        store.set_daemon_backed(false);
        assert_eq!(
            store.load_or(id, vec![entry("d")]).unwrap(),
            vec![entry("d")]
        );
        assert!(store.save(id, vec![entry("a")]).unwrap().is_none());
        assert_eq!(store.load(id).unwrap(), vec![entry("a")]);
        assert!(dir.path().join(format!("assignments/{id}.toml")).exists());
        assert!(store.save(id, vec![entry("a"), entry("a")]).is_err());
        assert_eq!(store.load(id).unwrap(), vec![entry("a")]);
    }

    #[test]
    fn daemon_backed_load_uses_workspace_info() {
        let (dir, mut store) = store();
        let known = info("/p", Some(vec![entry("a")]));
        let unknown = info("/q", None);
        store.set_daemon_backed(true);
        store.absorb(&[known.clone(), unknown.clone()]);
        assert_eq!(store.load(known.id).unwrap(), vec![entry("a")]);
        assert!(store.load(unknown.id).unwrap().is_empty());
        assert_eq!(
            store.load_or(unknown.id, vec![entry("d")]).unwrap(),
            vec![entry("d")]
        );
        assert!(!dir.path().join("assignments").exists());
    }

    #[test]
    fn daemon_backed_save_updates_cache_and_returns_command() {
        let (dir, mut store) = store();
        let ws = info("/p", None);
        store.set_daemon_backed(true);
        store.absorb(std::slice::from_ref(&ws));
        let command = store.save(ws.id, vec![entry("a")]).unwrap();
        assert!(
            matches!(command, Some(UiCommand::SaveProjectSetup { workspace_id, assignments })
            if workspace_id == ws.id && assignments == vec![entry("a")])
        );
        assert_eq!(store.load(ws.id).unwrap(), vec![entry("a")]);
        assert!(store.save(ws.id, vec![entry("b"), entry("b")]).is_err());
        assert_eq!(store.load(ws.id).unwrap(), vec![entry("a")]);
        assert!(!dir.path().join("assignments").exists());
    }

    #[test]
    fn change_event_updates_every_workspace_on_that_folder() {
        let (_dir, mut store) = store();
        let a = info("/p", Some(vec![entry("a")]));
        let b = info("/p", Some(vec![entry("a")]));
        let other = info("/q", Some(vec![entry("o")]));
        let all = [a.clone(), b.clone(), other.clone()];
        store.set_daemon_backed(true);
        store.absorb(&all);
        store.changed(Path::new("/p"), vec![entry("n")], &all);
        assert_eq!(store.load(a.id).unwrap(), vec![entry("n")]);
        assert_eq!(store.load(b.id).unwrap(), vec![entry("n")]);
        assert_eq!(store.load(other.id).unwrap(), vec![entry("o")]);
    }

    #[test]
    fn local_setup_is_migrated_once() {
        let (dir, mut store) = store();
        let ws = info("/p", None);
        let file = dir.path().join(format!("assignments/{}.toml", ws.id));
        store.set_daemon_backed(false);
        store.save(ws.id, vec![entry("a")]).unwrap();
        assert!(store.migrations(std::slice::from_ref(&ws)).is_empty());
        store.set_daemon_backed(true);
        let before = std::fs::read(&file).unwrap();
        let first = store.migrations(std::slice::from_ref(&ws));
        assert_eq!(first, vec![(ws.id, vec![entry("a")])]);
        assert!(store.migrations(std::slice::from_ref(&ws)).is_empty());
        assert_eq!(store.load(ws.id).unwrap(), vec![entry("a")]);
        let mut done = ws.clone();
        done.setup = Some(vec![entry("a")]);
        store.absorb(&[done.clone()]);
        assert!(store.migrations(&[done]).is_empty());
        assert_eq!(std::fs::read(&file).unwrap(), before);
    }

    #[test]
    fn migration_is_offered_once_per_folder_and_again_after_reconnect() {
        let (dir, mut store) = store();
        let a = info("/p", None);
        let b = info("/p", None);
        store.set_daemon_backed(false);
        store.save(a.id, vec![entry("a")]).unwrap();
        store.save(b.id, vec![entry("b")]).unwrap();
        store.set_daemon_backed(true);
        let all = [a.clone(), b];
        assert_eq!(store.migrations(&all).len(), 1);
        assert!(store.migrations(&all).is_empty());
        store.set_daemon_backed(true);
        assert_eq!(store.migrations(&all).len(), 1);
        assert!(
            dir.path()
                .join(format!("assignments/{}.toml", a.id))
                .exists()
        );
    }
}
