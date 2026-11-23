use crate::bridge::UiCommand;
use daedalus_proto::{ActivityCursor, ActivityPage, WorkspaceId};
use std::{
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
    pub fn deadline(&self) -> Option<Instant> {
        (self.visible.is_some() && self.pending.is_none() && self.staged.is_none() && self.dirty)
            .then_some(self.next_read)
    }
    pub fn request(&mut self, now: Instant) -> Option<UiCommand> {
        if self.deadline()? > now {
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
        let path = self.directory.join(format!("{id}.toml"));
        let result = toml::to_string(&cursor)
            .map_err(|e| e.to_string())
            .and_then(|s| crate::assignments::write_atomic(&path, &s));
        if let Err(error) = result {
            tracing::warn!(%error,"could not persist activity cursor");
        }
        self.acknowledged = Some(cursor);
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
