use super::Registry;
use crate::persistence::{PersistedProjectSetup, PersistedRecentProject, PersistedState};
use daedalus_proto::{Assignment, ProtoError, RecentProject, WorkspaceId, WorkspaceInfo};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How many recent projects the daemon remembers.
const MAX_RECENT_PROJECTS: usize = 20;

/// How long one `recent_projects` request waits for folder checks before it drops the
/// folders that have not answered yet.
const DIR_CHECK_TIMEOUT: Duration = Duration::from_secs(2);

type DirCheck = Arc<dyn Fn(&Path) -> bool + Send + Sync>;

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Registry {
    /// Projects seen before, most recent first. Folders that no longer exist and folders
    /// that are open as a workspace right now are left out, but stay stored.
    pub async fn recent_projects(&self) -> Vec<RecentProject> {
        self.recent_projects_with(Arc::new(|path: &Path| path.is_dir()), DIR_CHECK_TIMEOUT)
            .await
    }

    /// `recent_projects` with an injectable folder check and deadline.
    async fn recent_projects_with(&self, check: DirCheck, timeout: Duration) -> Vec<RecentProject> {
        let open: Vec<PathBuf> = {
            let ws_map = self.workspaces.read().await;
            ws_map.values().map(|ws| ws.root_dir.clone()).collect()
        };
        // Copy what is needed and release the locks: the folder checks below can hang on a
        // stale mount, and must not hold up writers or other requests.
        let candidates: Vec<RecentProject> = {
            let recent = self.recent_projects.read().await;
            let setups = self.project_setups.read().await;
            recent
                .iter()
                .filter(|entry| !open.contains(&entry.root_dir))
                .map(|entry| RecentProject {
                    name: entry.name.clone(),
                    root_dir: entry.root_dir.clone(),
                    last_used: entry.last_used,
                    has_setup: setups.contains_key(&entry.root_dir),
                })
                .collect()
        };

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        for (index, entry) in candidates.iter().enumerate() {
            let check = check.clone();
            let path = entry.root_dir.clone();
            let tx = tx.clone();
            tokio::task::spawn_blocking(move || {
                let _ = tx.send((index, check(&path)));
            });
        }
        drop(tx);

        let mut exists = vec![false; candidates.len()];
        let deadline = tokio::time::Instant::now() + timeout;
        // Folders whose check has not answered by the deadline are left out of this reply.
        while let Ok(Some((index, is_dir))) = tokio::time::timeout_at(deadline, rx.recv()).await {
            exists[index] = is_dir;
        }
        candidates
            .into_iter()
            .zip(exists)
            .filter_map(|(entry, is_dir)| is_dir.then_some(entry))
            .collect()
    }

    /// Stores the pane setup for the workspace's folder, persists it and announces it.
    pub async fn set_project_setup(
        &self,
        workspace_id: WorkspaceId,
        assignments: Vec<Assignment>,
    ) -> Result<(), ProtoError> {
        daedalus_proto::validate_assignments(&assignments).map_err(ProtoError::InvalidRequest)?;
        let root_dir = {
            let ws_map = self.workspaces.read().await;
            ws_map
                .get(&workspace_id)
                .ok_or(ProtoError::WorkspaceNotFound(workspace_id))?
                .root_dir
                .clone()
        };
        self.persist_project_setup(root_dir, assignments).await
    }

    pub(crate) async fn attach_project_setup(&self, info: &mut WorkspaceInfo) {
        info.setup = self
            .project_setups
            .read()
            .await
            .get(&info.root_dir)
            .cloned();
    }

    /// Moves the folder to the front of the recent list and trims the list.
    pub(super) async fn record_recent_project(&self, name: &str, root_dir: &Path) {
        let mut recent = self.recent_projects.write().await;
        recent.retain(|entry| entry.root_dir != root_dir);
        recent.insert(
            0,
            PersistedRecentProject {
                name: name.to_string(),
                root_dir: root_dir.to_path_buf(),
                last_used: unix_now(),
            },
        );
        recent.truncate(MAX_RECENT_PROJECTS);
    }

    pub(super) async fn load_project_lists(&self, state: &mut PersistedState) {
        *self.recent_projects.write().await = std::mem::take(&mut state.recent_projects);
        *self.project_setups.write().await = std::mem::take(&mut state.project_setups)
            .into_iter()
            .map(|setup| (setup.root_dir, setup.assignments))
            .collect();
    }

    pub(super) async fn project_lists_for_persist(
        &self,
    ) -> (Vec<PersistedRecentProject>, Vec<PersistedProjectSetup>) {
        let recent = self.recent_projects.read().await.clone();
        let mut setups: Vec<PersistedProjectSetup> = self
            .project_setups
            .read()
            .await
            .iter()
            .map(|(root_dir, assignments)| PersistedProjectSetup {
                root_dir: root_dir.clone(),
                assignments: assignments.clone(),
            })
            .collect();
        setups.sort_by(|a, b| a.root_dir.cmp(&b.root_dir));
        (recent, setups)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::StateStore;
    use crate::shell_integration::ShellIntegration;
    use crate::supervisor::{IsolationMode, Supervisor};
    use daedalus_proto::{PaneRole, ServerEvent};
    use std::collections::HashMap;
    use tempfile::TempDir;
    use tokio::sync::broadcast;

    fn registry_at(runtime: &Path, state_path: PathBuf) -> Registry {
        #[cfg(windows)]
        let supervisor = Supervisor::with_mode(IsolationMode::WindowsProcessTree);
        #[cfg(not(windows))]
        let supervisor = Supervisor::with_mode(IsolationMode::PgidFallback);
        Registry::new_with_state_store(
            supervisor,
            ShellIntegration::prepare(runtime).unwrap(),
            broadcast::channel(64).0,
            Some(StateStore::new(state_path)),
        )
    }

    fn folder(base: &Path, name: &str) -> PathBuf {
        let dir = base.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn assignment(id: &str) -> Assignment {
        Assignment {
            id: id.to_string(),
            name: format!("pane {id}"),
            role: PaneRole::Agent,
            argv: vec!["claude".to_string()],
        }
    }

    async fn open(reg: &Registry, name: &str, dir: &Path) -> WorkspaceInfo {
        reg.create_workspace(name.to_string(), dir.to_path_buf(), HashMap::new())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn create_and_close_put_project_first() {
        let tmp = TempDir::new().unwrap();
        let reg = registry_at(tmp.path(), tmp.path().join("state.json"));
        let a = open(&reg, "a", &folder(tmp.path(), "a")).await;
        let b = open(&reg, "b", &folder(tmp.path(), "b")).await;
        // While both are open nothing is listed.
        assert!(reg.recent_projects().await.is_empty());

        // Create recorded a then b, so b is ahead until the closes reorder them.
        reg.close_workspace(b.id, false, false, false)
            .await
            .unwrap();
        reg.close_workspace(a.id, false, false, false)
            .await
            .unwrap();

        let recent = reg.recent_projects().await;
        let names: Vec<&str> = recent.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["a", "b"]);
        assert!(recent[0].last_used > 0);
    }

    #[tokio::test]
    async fn recent_list_keeps_twenty_newest() {
        let tmp = TempDir::new().unwrap();
        let reg = registry_at(tmp.path(), tmp.path().join("state.json"));
        for i in 0..21 {
            let dir = folder(tmp.path(), &format!("p{i}"));
            reg.record_recent_project(&format!("p{i}"), &dir).await;
        }
        let recent = reg.recent_projects().await;
        assert_eq!(recent.len(), 20);
        assert_eq!(recent[0].name, "p20");
        assert!(recent.iter().all(|r| r.name != "p0"));
    }

    #[tokio::test]
    async fn open_workspace_folder_is_not_listed() {
        let tmp = TempDir::new().unwrap();
        let reg = registry_at(tmp.path(), tmp.path().join("state.json"));
        open(&reg, "a", &folder(tmp.path(), "a")).await;
        assert!(reg.recent_projects().await.is_empty());
    }

    #[tokio::test]
    async fn missing_folder_is_hidden_but_kept() {
        let tmp = TempDir::new().unwrap();
        let state_path = tmp.path().join("state.json");
        let reg = registry_at(tmp.path(), state_path.clone());
        let dir = folder(tmp.path(), "gone");
        let ws = open(&reg, "gone", &dir).await;
        reg.close_workspace(ws.id, false, false, false)
            .await
            .unwrap();
        assert_eq!(reg.recent_projects().await.len(), 1);

        std::fs::remove_dir_all(&dir).unwrap();
        assert!(reg.recent_projects().await.is_empty());
        let saved = StateStore::new(state_path).load().unwrap();
        assert_eq!(saved.recent_projects.len(), 1);
        assert_eq!(saved.recent_projects[0].root_dir, ws.root_dir);

        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(reg.recent_projects().await.len(), 1);
    }

    #[tokio::test]
    async fn invalid_setup_is_rejected_without_storing_or_broadcasting() {
        let tmp = TempDir::new().unwrap();
        let reg = registry_at(tmp.path(), tmp.path().join("state.json"));
        let dir = folder(tmp.path(), "a");
        let ws = open(&reg, "a", &dir).await;
        let mut events = reg.subscribe_events();

        let err = reg
            .set_project_setup(ws.id, vec![assignment("dup"), assignment("dup")])
            .await
            .unwrap_err();
        assert!(matches!(err, ProtoError::InvalidRequest(_)));
        assert_eq!(reg.list_workspaces().await[0].setup, None);
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn setup_is_attached_and_survives_close() {
        let tmp = TempDir::new().unwrap();
        let reg = registry_at(tmp.path(), tmp.path().join("state.json"));
        let dir = folder(tmp.path(), "a");
        let ws = open(&reg, "a", &dir).await;
        assert_eq!(ws.setup, None);

        let setup = vec![assignment("a1"), assignment("a2")];
        reg.set_project_setup(ws.id, setup.clone()).await.unwrap();
        let listed = reg.list_workspaces().await;
        assert_eq!(listed[0].setup.as_ref(), Some(&setup));
        let by_dir = reg.workspace_for_directory(dir.clone()).await.unwrap();
        assert_eq!(by_dir.setup.as_ref(), Some(&setup));

        reg.close_workspace(ws.id, false, false, false)
            .await
            .unwrap();
        let recent = reg.recent_projects().await;
        assert_eq!(recent.len(), 1);
        assert!(recent[0].has_setup);

        let reopened = open(&reg, "a", &dir).await;
        assert_eq!(reopened.setup.as_ref(), Some(&setup));

        let err = reg
            .set_project_setup(WorkspaceId::new(), Vec::new())
            .await
            .unwrap_err();
        assert!(matches!(err, ProtoError::WorkspaceNotFound(_)));
    }

    #[tokio::test]
    async fn setup_change_is_broadcast() {
        let tmp = TempDir::new().unwrap();
        let reg = registry_at(tmp.path(), tmp.path().join("state.json"));
        let ws = open(&reg, "a", &folder(tmp.path(), "a")).await;
        let mut rx = reg.subscribe_events();
        let setup = vec![assignment("a1")];
        reg.set_project_setup(ws.id, setup.clone()).await.unwrap();
        match rx.recv().await.unwrap() {
            ServerEvent::ProjectSetupChanged {
                root_dir,
                assignments,
            } => {
                assert_eq!(root_dir, ws.root_dir);
                assert_eq!(assignments, setup);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn failed_setup_persistence_keeps_the_confirmed_setup() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("state.json");
        let reg = registry_at(tmp.path(), path.clone());
        let ws = open(&reg, "a", &folder(tmp.path(), "a")).await;
        let confirmed = vec![assignment("confirmed")];
        reg.set_project_setup(ws.id, confirmed.clone())
            .await
            .unwrap();

        let backup = tmp.path().join("state.backup");
        std::fs::rename(&path, &backup).unwrap();
        std::fs::create_dir(&path).unwrap();
        let mut events = reg.subscribe_events();
        assert!(matches!(
            reg.set_project_setup(ws.id, vec![assignment("never-written")])
                .await,
            Err(ProtoError::IoError(_))
        ));
        assert!(events.try_recv().is_err());
        assert_eq!(
            reg.list_workspaces().await[0].setup.as_ref(),
            Some(&confirmed)
        );
        std::fs::remove_dir(&path).unwrap();
        std::fs::rename(backup, &path).unwrap();

        let restarted = registry_at(tmp.path(), path);
        assert_eq!(restarted.restore_persisted_workspaces().await.unwrap(), 1);
        assert_eq!(
            restarted.list_workspaces().await[0].setup.as_ref(),
            Some(&confirmed)
        );
    }

    #[tokio::test]
    async fn concurrent_setups_broadcast_in_persisted_order() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("state.json");
        let reg = registry_at(tmp.path(), path.clone());
        let ws = open(&reg, "a", &folder(tmp.path(), "a")).await;
        let mut events = reg.subscribe_events();
        let (first, second) = tokio::join!(
            reg.set_project_setup(ws.id, vec![assignment("first")]),
            reg.set_project_setup(ws.id, vec![assignment("second")])
        );
        first.unwrap();
        second.unwrap();
        assert!(matches!(
            events.recv().await.unwrap(),
            ServerEvent::ProjectSetupChanged { .. }
        ));
        let ServerEvent::ProjectSetupChanged { assignments, .. } = events.recv().await.unwrap()
        else {
            panic!("expected the last committed setup");
        };
        let persisted = StateStore::new(path).load().unwrap();
        assert_eq!(persisted.project_setups.len(), 1);
        assert_eq!(persisted.project_setups[0].assignments, assignments);
        assert_eq!(reg.list_workspaces().await[0].setup, Some(assignments));
    }

    #[tokio::test]
    async fn restart_restores_lists() {
        let tmp = TempDir::new().unwrap();
        let state_path = tmp.path().join("state.json");
        let reg1 = registry_at(tmp.path(), state_path.clone());
        let ws = open(&reg1, "a", &folder(tmp.path(), "a")).await;
        let setup = vec![assignment("a1")];
        reg1.set_project_setup(ws.id, setup.clone()).await.unwrap();
        reg1.close_workspace(ws.id, false, false, false)
            .await
            .unwrap();
        let before = reg1.recent_projects().await;
        assert_eq!(before.len(), 1);

        // No workspaces are left open, so restore must still load the lists.
        let reg2 = registry_at(tmp.path(), state_path);
        assert_eq!(reg2.restore_persisted_workspaces().await.unwrap(), 0);
        assert_eq!(reg2.recent_projects().await, before);
        let reopened = open(&reg2, "a", &ws.root_dir).await;
        assert_eq!(reopened.setup.as_ref(), Some(&setup));
    }

    /// A folder check that blocks until the returned sender is dropped or sent to.
    fn blocking_check(blocked: PathBuf) -> (DirCheck, std::sync::mpsc::Sender<()>) {
        let (release, gate) = std::sync::mpsc::channel::<()>();
        let gate = std::sync::Mutex::new(gate);
        let check: DirCheck = Arc::new(move |path: &Path| {
            if path == blocked {
                let _ = gate.lock().unwrap().recv();
            }
            true
        });
        (check, release)
    }

    #[tokio::test]
    async fn writers_are_not_blocked_by_a_slow_folder_check() {
        let tmp = TempDir::new().unwrap();
        let reg = Arc::new(registry_at(tmp.path(), tmp.path().join("state.json")));
        let slow = folder(tmp.path(), "slow");
        reg.record_recent_project("slow", &slow).await;
        let (check, release) = blocking_check(slow);

        let lister = {
            let reg = reg.clone();
            tokio::spawn(async move {
                reg.recent_projects_with(check, Duration::from_secs(30))
                    .await
            })
        };
        // Let the check start and block.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let other = folder(tmp.path(), "other");
        tokio::time::timeout(Duration::from_secs(5), async {
            reg.record_recent_project("other", &other).await;
            reg.project_setups
                .write()
                .await
                .insert(other.clone(), vec![assignment("x")]);
        })
        .await
        .expect("writers must not wait for the folder check");
        assert!(!lister.is_finished());

        release.send(()).unwrap();
        let listed = tokio::time::timeout(Duration::from_secs(5), lister)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "slow");
    }

    #[tokio::test]
    async fn folders_that_do_not_answer_in_time_are_dropped_but_kept() {
        let tmp = TempDir::new().unwrap();
        let reg = registry_at(tmp.path(), tmp.path().join("state.json"));
        let hung = folder(tmp.path(), "hung");
        let fine = folder(tmp.path(), "fine");
        reg.record_recent_project("hung", &hung).await;
        reg.record_recent_project("fine", &fine).await;
        let (check, release) = blocking_check(hung);

        let started = std::time::Instant::now();
        let listed = reg
            .recent_projects_with(check, Duration::from_millis(200))
            .await;
        assert!(started.elapsed() < Duration::from_secs(5));
        let names: Vec<&str> = listed.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["fine"]);
        assert_eq!(reg.recent_projects.read().await.len(), 2);
        drop(release);
    }
}
