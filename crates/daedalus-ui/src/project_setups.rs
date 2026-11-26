//! One place that decides where a workspace's pane setup lives: on the daemon, per project
//! folder, when it supports that, and in local files otherwise.

use crate::assignments::{self, Assignment};
use daedalus_proto::{WorkspaceId, types::Workspace, validate_assignments};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupMigration {
    pub workspace_id: WorkspaceId,
    pub request_id: u64,
    pub assignments: Vec<Assignment>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MigrationState {
    Pending {
        workspace_id: WorkspaceId,
        request_id: u64,
    },
    Failed,
    Confirmed,
}

#[derive(Debug, Default)]
pub struct ProjectSetups {
    /// Directory of the local `<id>.toml` files. `None` follows the configured location.
    dir: Option<PathBuf>,
    daemon_backed: bool,
    cache: HashMap<WorkspaceId, Vec<Assignment>>,
    roots: HashMap<WorkspaceId, PathBuf>,
    /// Last setup the daemon confirmed for each workspace.
    confirmed: HashMap<WorkspaceId, Vec<Assignment>>,
    migrations_by_root: HashMap<PathBuf, MigrationState>,
    next_migration: u64,
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
        self.confirmed.clear();
        self.migrations_by_root.clear();
    }

    pub fn absorb<P>(&mut self, workspaces: &[Workspace<P>]) {
        for info in workspaces {
            self.roots.insert(info.id, info.root_dir.clone());
            if let Some(setup) = &info.setup {
                self.workspace_changed(info.id, setup.clone());
            }
        }
    }

    pub fn changed<P>(
        &mut self,
        root_dir: &Path,
        assignments: Vec<Assignment>,
        workspaces: &[Workspace<P>],
    ) {
        // Only learn which workspace lives where: `setup` on these snapshots may be stale.
        for info in workspaces {
            self.roots.insert(info.id, info.root_dir.clone());
        }
        let ids: Vec<_> = self
            .roots
            .iter()
            .filter(|(_, root)| root.as_path() == root_dir)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.workspace_changed(id, assignments.clone());
        }
    }

    pub fn workspace_changed(&mut self, id: WorkspaceId, assignments: Vec<Assignment>) {
        self.confirmed.insert(id, assignments.clone());
        self.cache.insert(id, assignments);
    }

    /// A rejected legacy migration restores only the workspace it was offered for.
    pub fn rejected(&mut self, id: WorkspaceId) {
        if let Some(setup) = self.confirmed.get(&id) {
            self.cache.insert(id, setup.clone());
        } else {
            self.cache.remove(&id);
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

    /// Daemon-backed saves return validated assignments to send without changing the cache.
    /// Older daemons use local files, which are saved synchronously.
    pub fn save(
        &self,
        id: WorkspaceId,
        entries: Vec<Assignment>,
    ) -> Result<Option<Vec<Assignment>>, String> {
        if !self.daemon_backed {
            assignments::save_to(&self.local_path(id), &entries)?;
            return Ok(None);
        }
        validate_assignments(&entries)?;
        Ok(Some(entries))
    }

    /// Offer each folder's legacy setup once, leaving failed attempts for an explicit retry.
    pub fn migrations<P>(&mut self, workspaces: &[Workspace<P>]) -> Vec<SetupMigration> {
        if !self.daemon_backed {
            return Vec::new();
        }
        workspaces
            .iter()
            .filter_map(|info| {
                if self.migrations_by_root.contains_key(&info.root_dir) {
                    return None;
                }
                self.offer_migration(info)
            })
            .collect()
    }

    pub fn retry_migration<P>(
        &mut self,
        root: &Path,
        workspaces: &[Workspace<P>],
    ) -> Option<SetupMigration> {
        if self.migrations_by_root.get(root) != Some(&MigrationState::Failed) {
            return None;
        }
        workspaces
            .iter()
            .filter(|info| info.root_dir == root)
            .find_map(|info| self.offer_migration(info))
    }

    fn offer_migration<P>(&mut self, info: &Workspace<P>) -> Option<SetupMigration> {
        if info.setup.is_some() || self.confirmed.contains_key(&info.id) {
            return None;
        }
        let path = self.local_path(info.id);
        if !path.exists() {
            return None;
        }
        let assignments = assignments::load_from(&path, Vec::new()).ok()?;
        self.next_migration = self.next_migration.wrapping_add(1);
        let request_id = self.next_migration;
        self.migrations_by_root.insert(
            info.root_dir.clone(),
            MigrationState::Pending {
                workspace_id: info.id,
                request_id,
            },
        );
        self.roots.insert(info.id, info.root_dir.clone());
        self.cache.insert(info.id, assignments.clone());
        Some(SetupMigration {
            workspace_id: info.id,
            request_id,
            assignments,
        })
    }

    /// Return whether the reply belongs to the current attempt for the folder.
    pub fn finish_migration(
        &mut self,
        workspace_id: WorkspaceId,
        request_id: u64,
        saved: bool,
    ) -> bool {
        let Some(status) = self
            .roots
            .get(&workspace_id)
            .and_then(|root| self.migrations_by_root.get_mut(root))
        else {
            return false;
        };
        if *status
            != (MigrationState::Pending {
                workspace_id,
                request_id,
            })
        {
            return false;
        }
        if saved {
            *status = MigrationState::Confirmed;
            if let Some(entries) = self.cache.get(&workspace_id) {
                self.confirmed.insert(workspace_id, entries.clone());
            }
        } else {
            *status = MigrationState::Failed;
            self.rejected(workspace_id);
        }
        true
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
    fn daemon_backed_save_waits_for_confirmation_before_updating_cache() {
        let (dir, mut store) = store();
        let ws = info("/p", None);
        store.set_daemon_backed(true);
        store.absorb(std::slice::from_ref(&ws));
        let pending = store.save(ws.id, vec![entry("a")]).unwrap();
        assert_eq!(pending, Some(vec![entry("a")]));
        assert!(store.load(ws.id).unwrap().is_empty());
        assert!(store.save(ws.id, vec![entry("b"), entry("b")]).is_err());
        store.changed(Path::new("/p"), pending.unwrap(), std::slice::from_ref(&ws));
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
        assert_eq!(first[0].workspace_id, ws.id);
        assert_eq!(first[0].assignments, vec![entry("a")]);
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

    #[test]
    fn change_event_for_one_folder_keeps_newer_setups_on_other_folders() {
        let (_dir, mut store) = store();
        let p = info("/p", Some(vec![entry("old-p")]));
        let q = info("/q", Some(vec![entry("old-q")]));
        let stale = [p.clone(), q.clone()];
        store.set_daemon_backed(true);
        store.absorb(&stale);
        store.save(q.id, vec![entry("new-q")]).unwrap();
        store.changed(Path::new("/q"), vec![entry("new-q")], &stale);
        store.changed(Path::new("/p"), vec![entry("new-p")], &stale);
        assert_eq!(store.load(p.id).unwrap(), vec![entry("new-p")]);
        assert_eq!(store.load(q.id).unwrap(), vec![entry("new-q")]);
    }

    #[test]
    fn rejected_save_preserves_the_confirmed_setup_for_each_workspace() {
        let (_dir, mut store) = store();
        let a = info("/p", Some(vec![entry("kept")]));
        let b = info("/p", Some(vec![entry("kept")]));
        let fresh = info("/q", None);
        let all = [a.clone(), b.clone(), fresh.clone()];
        store.set_daemon_backed(true);
        store.absorb(&all);
        store.save(a.id, vec![entry("denied")]).unwrap();
        assert_eq!(store.load(b.id).unwrap(), vec![entry("kept")]);
        store.rejected(a.id);
        assert_eq!(store.load(a.id).unwrap(), vec![entry("kept")]);
        assert_eq!(store.load(b.id).unwrap(), vec![entry("kept")]);
        store.save(fresh.id, vec![entry("x")]).unwrap();
        store.rejected(fresh.id);
        assert_eq!(
            store.load_or(fresh.id, vec![entry("d")]).unwrap(),
            vec![entry("d")]
        );
    }

    #[test]
    fn an_echo_moves_what_a_later_rejection_falls_back_to() {
        let (_dir, mut store) = store();
        let a = info("/p", Some(vec![entry("one")]));
        store.set_daemon_backed(true);
        store.absorb(std::slice::from_ref(&a));
        store.changed(
            Path::new("/p"),
            vec![entry("two")],
            std::slice::from_ref(&a),
        );
        store.save(a.id, vec![entry("three")]).unwrap();
        store.rejected(a.id);
        assert_eq!(store.load(a.id).unwrap(), vec![entry("two")]);
    }

    #[test]
    fn migration_that_cannot_be_sent_is_rolled_back() {
        let (_dir, mut store) = store();
        let ws = info("/p", None);
        store.set_daemon_backed(false);
        store.save(ws.id, vec![entry("a")]).unwrap();
        store.set_daemon_backed(true);
        let migration = store.migrations(std::slice::from_ref(&ws)).pop().unwrap();
        assert!(store.finish_migration(ws.id, migration.request_id, false));
        assert!(store.load(ws.id).unwrap().is_empty());
    }

    #[test]
    fn failed_migration_retries_once_per_folder_on_explicit_refresh() {
        let (_dir, mut store) = store();
        let first = info("/p", None);
        let second = info("/p", None);
        store.save(first.id, vec![entry("first")]).unwrap();
        store.save(second.id, vec![entry("second")]).unwrap();
        store.set_daemon_backed(true);
        let workspaces = [first.clone(), second.clone()];
        let offer = store.migrations(&workspaces).pop().unwrap();
        assert_eq!(offer.workspace_id, first.id);
        assert!(store.finish_migration(offer.workspace_id, offer.request_id, false));
        assert!(store.migrations(&workspaces).is_empty());
        let retry = store.retry_migration(Path::new("/p"), &[second]).unwrap();
        assert_ne!(retry.workspace_id, first.id);
        assert!(!store.finish_migration(offer.workspace_id, offer.request_id, false));
        assert!(
            store
                .retry_migration(Path::new("/p"), &workspaces)
                .is_none()
        );
        assert!(store.finish_migration(retry.workspace_id, retry.request_id, true));
        assert!(
            store
                .retry_migration(Path::new("/p"), &workspaces)
                .is_none()
        );
    }
}
