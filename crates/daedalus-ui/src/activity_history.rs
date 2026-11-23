use crate::bridge::UiCommand;
use daedalus_proto::{ActivityCursor, ActivityPage, WorkspaceId};
use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const REFRESH_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, PartialEq, Eq)]
pub enum PageUpdate {
    Recap { first: bool, gap: bool },
    Foreground(usize),
}

/// A read belongs to one focused visit on one connection. Its cursor only becomes
/// durable when the renderer confirms that visit's recap reached the surface.
pub struct ActivityHistory {
    directory: PathBuf,
    supported: bool,
    visible: Option<WorkspaceId>,
    serial: u64,
    pending: Option<u64>,
    acknowledged: Option<ActivityCursor>,
    staged: Option<ActivityCursor>,
    unsaved: HashMap<WorkspaceId, ActivityCursor>,
    retry_at: Option<Instant>,
    initial: bool,
    dirty: bool,
    next_read: Instant,
    storage_gap: bool,
}

impl ActivityHistory {
    pub fn new(profile: &Path, endpoint: &Path) -> Self {
        // Stable FNV-1a fingerprint avoids putting socket paths or SSH identities in filenames.
        let hash = endpoint
            .as_os_str()
            .as_encoded_bytes()
            .iter()
            .fold(0xcbf29ce484222325u64, |hash, b| {
                (hash ^ u64::from(*b)).wrapping_mul(0x100000001b3)
            });
        Self {
            directory: profile
                .with_added_extension("history")
                .join(format!("{hash:016x}")),
            supported: false,
            visible: None,
            serial: 0,
            pending: None,
            acknowledged: None,
            staged: None,
            unsaved: HashMap::new(),
            retry_at: None,
            initial: true,
            dirty: false,
            next_read: Instant::now(),
            storage_gap: false,
        }
    }
    pub fn connection(&mut self, supported: bool) {
        self.supported = supported;
        self.visible = None;
        self.pending = None;
        self.staged = None;
    }
    pub fn visit(&mut self, visible: Option<WorkspaceId>) {
        let visible = visible.filter(|_| self.supported);
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        self.pending = None;
        self.staged = None;
        self.storage_gap = false;
        self.acknowledged = visible.and_then(|id| {
            if let Some(cursor) = self.unsaved.get(&id) {
                return Some(*cursor);
            }
            let path = self.directory.join(format!("{id}.toml"));
            match fs::read_to_string(&path) {
                Ok(contents) => match toml::from_str(&contents) {
                    Ok(cursor) => Some(cursor),
                    Err(error) => {
                        tracing::warn!(%error,"invalid activity cursor");
                        self.storage_gap = true;
                        None
                    }
                },
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => {
                    tracing::warn!(%error,"could not load activity cursor");
                    self.storage_gap = true;
                    None
                }
            }
        });
        self.initial = true;
        self.dirty = true;
        self.next_read = Instant::now();
    }
    pub fn changed(&mut self, id: WorkspaceId) {
        if self.visible == Some(id) {
            self.dirty = true;
        }
    }
    fn read_deadline(&self) -> Option<Instant> {
        (self.visible.is_some() && self.pending.is_none() && self.staged.is_none() && self.dirty)
            .then_some(self.next_read)
    }
    pub fn deadline(&self) -> Option<Instant> {
        self.read_deadline().into_iter().chain(self.retry_at).min()
    }
    pub fn request(&mut self, now: Instant) -> Option<UiCommand> {
        if self.retry_at.is_some_and(|deadline| now >= deadline) {
            self.flush_cursors(now);
        }
        if self.read_deadline()? > now {
            return None;
        }
        self.serial += 1;
        self.pending = Some(self.serial);
        self.dirty = false;
        self.next_read = now + REFRESH_INTERVAL;
        Some(UiCommand::ReadActivity {
            workspace_id: self.visible?,
            cursor: self.acknowledged,
            token: self.serial,
        })
    }
    pub fn failed(&mut self, token: u64) {
        if self.pending == Some(token) {
            self.pending = None;
            self.dirty = true;
            self.next_read = Instant::now() + Duration::from_secs(5);
        }
    }
    /// Only the initial page supplies absence content. Later reads track what the
    /// user saw during this visit, while the incoming recap stays available.
    pub fn received(&mut self, token: u64, page: &ActivityPage) -> Option<PageUpdate> {
        if self.pending != Some(token) || self.visible != Some(page.workspace_id) {
            return None;
        }
        self.pending = None;
        self.staged = Some(page.cursor);
        let first = self.acknowledged.is_none() && !self.storage_gap;
        let show = self.initial || page.gap;
        self.initial = false;
        Some(if show {
            PageUpdate::Recap {
                first,
                gap: self.storage_gap || page.gap,
            }
        } else {
            PageUpdate::Foreground(page.entries.len())
        })
    }
    pub fn presented(&mut self) {
        let (Some(id), Some(cursor)) = (self.visible, self.staged.take()) else {
            return;
        };
        if self.acknowledged == Some(cursor) {
            return;
        }
        self.acknowledged = Some(cursor);
        self.unsaved.insert(id, cursor);
        if self.retry_at.is_none() {
            self.flush_cursors(Instant::now());
        }
    }

    fn flush_cursors(&mut self, now: Instant) {
        self.unsaved.retain(|id, cursor| {
            let path = self.directory.join(format!("{id}.toml"));
            let result = toml::to_string(cursor)
                .map_err(|error| error.to_string())
                .and_then(|text| crate::assignments::write_atomic(&path, &text));
            if let Err(error) = result {
                tracing::warn!(%error, "could not persist activity cursor");
                return true;
            }
            false
        });
        self.retry_at = (!self.unsaved.is_empty()).then_some(now + Duration::from_secs(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn read(history: &mut ActivityHistory) -> (u64, Option<ActivityCursor>) {
        let UiCommand::ReadActivity { token, cursor, .. } = history
            .request(history.deadline().expect("read scheduled"))
            .unwrap()
        else {
            panic!()
        };
        (token, cursor)
    }
    #[test]
    fn only_a_focused_presented_visit_persists_its_head() {
        let dir = tempdir().unwrap();
        let profile = dir.path().join("profile.toml");
        let endpoint = Path::new("ssh://me@dev");
        let id = WorkspaceId::new();
        let page = ActivityPage {
            workspace_id: id,
            cursor: ActivityCursor {
                epoch: WorkspaceId::new(),
                seq: 7,
            },
            entries: vec![],
            gap: false,
        };
        let mut history = ActivityHistory::new(&profile, endpoint);
        history.connection(true);
        history.visit(Some(id));
        let (stale, cursor) = read(&mut history);
        assert_eq!(cursor, None);
        history.visit(None);
        assert_eq!(history.received(stale, &page), None);
        history.presented();
        history.visit(Some(id));
        let (token, cursor) = read(&mut history);
        assert_eq!(cursor, None);
        assert_eq!(history.received(stale, &page), None);
        assert_eq!(
            history.received(token, &page),
            Some(PageUpdate::Recap {
                first: true,
                gap: false
            })
        );
        history.visit(None);
        history.presented();
        history.visit(Some(id));
        let (token, cursor) = read(&mut history);
        assert_eq!(cursor, None);
        assert_eq!(
            history.received(token, &page),
            Some(PageUpdate::Recap {
                first: true,
                gap: false
            })
        );
        history.presented();
        assert!(history.deadline().is_none());

        for (path, target, expected) in [
            (&profile, endpoint, Some(page.cursor)),
            (&profile, Path::new("local"), None),
            (&dir.path().join("another.toml"), endpoint, None),
        ] {
            let mut fresh = ActivityHistory::new(path, target);
            fresh.connection(true);
            fresh.visit(Some(id));
            assert_eq!(read(&mut fresh).1, expected);
        }
    }

    #[test]
    fn failed_save_retries_after_leaving_without_new_activity() {
        let dir = tempdir().unwrap();
        let profile = dir.path().join("profile");
        let endpoint = Path::new("socket");
        let id = WorkspaceId::new();
        let mut history = ActivityHistory::new(&profile, endpoint);
        let page = ActivityPage {
            workspace_id: id,
            cursor: ActivityCursor {
                epoch: WorkspaceId::new(),
                seq: 12,
            },
            entries: vec![],
            gap: false,
        };
        history.connection(true);
        history.visit(Some(id));
        let (token, _) = read(&mut history);
        history.received(token, &page);
        fs::create_dir_all(history.directory.parent().unwrap()).unwrap();
        fs::write(&history.directory, "blocked").unwrap();
        history.presented();
        history.visit(None);
        let retry = history.deadline().unwrap();
        fs::remove_file(&history.directory).unwrap();
        assert!(history.request(retry - Duration::from_millis(1)).is_none());
        assert!(!history.directory.exists());
        assert!(history.request(retry).is_none());
        assert!(history.deadline().is_none());
        let mut fresh = ActivityHistory::new(&profile, endpoint);
        fresh.connection(true);
        fresh.visit(Some(id));
        assert_eq!(read(&mut fresh).1, Some(page.cursor));
    }

    #[test]
    fn reconnect_rejects_an_old_page_and_exposes_the_new_epoch_gap() {
        let dir = tempdir().unwrap();
        let id = WorkspaceId::new();
        let mut h = ActivityHistory::new(&dir.path().join("profile"), Path::new("socket"));
        let mut page = ActivityPage {
            workspace_id: id,
            cursor: ActivityCursor {
                epoch: WorkspaceId::new(),
                seq: 8,
            },
            entries: vec![],
            gap: false,
        };
        h.connection(true);
        h.visit(Some(id));
        let (initial, _) = read(&mut h);
        h.received(initial, &page);
        h.presented();
        h.changed(id);
        let (stale, _) = read(&mut h);
        h.connection(false);
        h.visit(Some(id));
        assert!(h.deadline().is_none());
        h.connection(true);
        h.visit(Some(id));
        let (fresh, saved) = read(&mut h);
        assert_eq!(saved, Some(page.cursor));
        assert_eq!(h.received(stale, &page), None);
        page.cursor = ActivityCursor {
            epoch: WorkspaceId::new(),
            seq: 0,
        };
        page.gap = true;
        assert_eq!(
            h.received(fresh, &page),
            Some(PageUpdate::Recap {
                first: false,
                gap: true
            })
        );
    }

    #[test]
    fn foreground_updates_advance_baseline_without_replacing_the_held_recap() {
        let dir = tempdir().unwrap();
        let id = WorkspaceId::new();
        let mut history = ActivityHistory::new(&dir.path().join("config"), Path::new("sock"));
        let mut page = ActivityPage {
            workspace_id: id,
            cursor: ActivityCursor {
                epoch: WorkspaceId::new(),
                seq: 1,
            },
            entries: vec![],
            gap: false,
        };
        history.connection(true);
        history.visit(Some(id));
        let (token, _) = read(&mut history);
        history.received(token, &page);
        history.presented();
        history.changed(id);
        let (next, cursor) = read(&mut history);
        assert_eq!(cursor, Some(page.cursor));
        page.cursor.seq += 1;
        assert_eq!(
            history.received(next, &page),
            Some(PageUpdate::Foreground(0))
        );
        history.presented();
        history.visit(None);
        history.visit(Some(id));
        assert_eq!(read(&mut history).1, Some(page.cursor));
    }
}
