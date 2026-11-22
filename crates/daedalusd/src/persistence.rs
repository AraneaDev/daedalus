use daedalus_proto::WorkspaceId;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{self, ErrorKind, Write};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const STATE_VERSION: u32 = 1;

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedWorkspace {
    pub id: WorkspaceId,
    pub name: String,
    pub root_dir: PathBuf,
    pub slot_index: u16,
    #[serde(default)]
    pub agent_preset: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedState {
    pub version: u32,
    #[serde(default)]
    pub workspaces: Vec<PersistedWorkspace>,
}

impl Default for PersistedState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            workspaces: Vec::new(),
        }
    }
}

/// Atomic file-backed persistence store for daemon workspace state (`state.json`).
#[derive(Debug, Clone)]
pub struct StateStore {
    path: PathBuf,
}

impl StateStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> io::Result<PersistedState> {
        let contents = match fs::read_to_string(&self.path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(PersistedState::default()),
            Err(err) => return Err(err),
        };

        let state: PersistedState = serde_json::from_str(&contents).map_err(|e| {
            io::Error::new(ErrorKind::InvalidData, format!("invalid state.json: {e}"))
        })?;
        if state.version != STATE_VERSION {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                format!(
                    "unsupported state.json version {} (expected {STATE_VERSION})",
                    state.version
                ),
            ));
        }
        Ok(state)
    }

    pub fn save(&self, state: &PersistedState) -> io::Result<()> {
        if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            #[cfg_attr(not(unix), allow(unused_variables))]
            let created = !parent.exists();
            fs::create_dir_all(parent)?;
            #[cfg(unix)]
            if created {
                fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
            }
        }

        let mut bytes = serde_json::to_vec_pretty(state)
            .map_err(|e| io::Error::other(format!("failed to serialize state.json: {e}")))?;
        bytes.push(b'\n');

        let seq = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp_name = format!(
            "{}.{}.{}.tmp",
            self.path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("state.json"),
            std::process::id(),
            seq
        );
        let tmp_path = match self.path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.join(tmp_name),
            _ => PathBuf::from(tmp_name),
        };

        let mut open_opts = OpenOptions::new();
        open_opts.write(true).create_new(true);
        #[cfg(unix)]
        open_opts.mode(0o600);
        let mut file = open_opts.open(&tmp_path)?;

        let write_res = (|| -> io::Result<()> {
            #[cfg(unix)]
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            Ok(())
        })();

        if let Err(err) = write_res {
            drop(file);
            let _ = fs::remove_file(&tmp_path);
            return Err(err);
        }
        drop(file);

        if let Err(err) = fs::rename(&tmp_path, &self.path) {
            let _ = fs::remove_file(&tmp_path);
            return Err(err);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn round_trips_persisted_state_and_sets_restrictive_permissions() {
        let dir = tempdir().unwrap();
        let existing_parent = dir.path().join("existing");
        fs::create_dir(&existing_parent).unwrap();
        #[cfg(unix)]
        fs::set_permissions(&existing_parent, fs::Permissions::from_mode(0o755)).unwrap();

        let state_path_existing = existing_parent.join("state.json");
        let store1 = StateStore::new(state_path_existing.clone());
        let state = PersistedState {
            version: STATE_VERSION,
            workspaces: vec![PersistedWorkspace {
                id: WorkspaceId::new(),
                name: "alpha".to_string(),
                root_dir: PathBuf::from("/tmp/alpha"),
                slot_index: 0,
                agent_preset: Some("agy".to_string()),
            }],
        };
        store1.save(&state).unwrap();

        #[cfg(unix)]
        {
            assert_eq!(
                fs::metadata(&existing_parent).unwrap().permissions().mode() & 0o777,
                0o755
            );
            assert_eq!(
                fs::metadata(&state_path_existing)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        assert_eq!(store1.load().unwrap(), state);

        // No stray .tmp files left behind
        let entries: Vec<_> = fs::read_dir(&existing_parent)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, vec!["state.json"]);

        // Newly created parent directory receives 0o700
        let created_parent = dir.path().join("newly_created");
        let state_path_created = created_parent.join("state.json");
        let store2 = StateStore::new(state_path_created.clone());
        store2.save(&state).unwrap();
        assert_eq!(store2.load().unwrap(), state);
        #[cfg(unix)]
        {
            assert_eq!(
                fs::metadata(&created_parent).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(&state_path_created)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn load_returns_empty_state_when_file_missing() {
        let dir = tempdir().unwrap();
        let store = StateStore::new(dir.path().join("missing.json"));
        let loaded = store.load().unwrap();
        assert_eq!(loaded, PersistedState::default());
        assert_eq!(loaded.version, 1);
        assert!(loaded.workspaces.is_empty());
    }

    #[test]
    fn load_rejects_malformed_json_and_unsupported_version() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");
        let store = StateStore::new(path.clone());

        fs::write(&path, "{not valid json").unwrap();
        let err = store.load().unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);

        fs::write(&path, r#"{"version": 99, "workspaces": []}"#).unwrap();
        let err = store.load().unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
    }

    #[test]
    fn save_replaces_existing_file_atomically() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");
        let store = StateStore::new(path.clone());

        let s1 = PersistedState {
            version: 1,
            workspaces: vec![PersistedWorkspace {
                id: WorkspaceId::new(),
                name: "one".to_string(),
                root_dir: PathBuf::from("/tmp/one"),
                slot_index: 0,
                agent_preset: None,
            }],
        };
        store.save(&s1).unwrap();

        let s2 = PersistedState {
            version: 1,
            workspaces: vec![PersistedWorkspace {
                id: WorkspaceId::new(),
                name: "two".to_string(),
                root_dir: PathBuf::from("/tmp/two"),
                slot_index: 1,
                agent_preset: Some("claude".to_string()),
            }],
        };
        store.save(&s2).unwrap();

        assert_eq!(store.load().unwrap(), s2);
    }
}
