use daedalus_proto::{ActivityCursor, ActivityEntry, ActivityKind, ActivityPage, WorkspaceId};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const RETAINED: usize = 256;

/// Shared by the workspace and its pane actors. Recording precedes broadcast and
/// does not require any connected clients.
#[derive(Clone, Default)]
pub(crate) struct Journal(Arc<Mutex<JournalState>>);

#[derive(Default)]
struct JournalState {
    entries: VecDeque<ActivityEntry>,
    last_read: u64,
}

impl Journal {
    pub fn record(&self, kind: ActivityKind) {
        let mut journal = self.0.lock().expect("journal lock poisoned");
        let at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        let last_read = journal.last_read;
        let entries = &mut journal.entries;
        if let ActivityKind::AgentOutput { pane_id, .. } = &kind
            && let Some(last) = entries.back_mut()
            && last.seq > last_read
            && matches!(&last.kind, ActivityKind::AgentOutput { pane_id: prior, .. } if prior == pane_id)
            && at_ms.abs_diff(last.at_ms) < 10_000
        {
            last.kind = kind;
            last.at_ms = at_ms;
            return;
        }
        let seq = entries.back().map_or(1, |last| last.seq + 1);
        if entries.len() == RETAINED {
            entries.pop_front();
        }
        entries.push_back(ActivityEntry { seq, at_ms, kind });
    }

    pub fn read(
        &self,
        workspace_id: WorkspaceId,
        epoch: WorkspaceId,
        after: Option<ActivityCursor>,
    ) -> ActivityPage {
        let mut journal = self.0.lock().expect("journal lock poisoned");
        let seq = journal.entries.back().map_or(0, |last| last.seq);
        journal.last_read = seq;
        let entries = &journal.entries;
        let oldest = entries
            .front()
            .map_or(0, |first| first.seq.saturating_sub(1));
        let gap = after.is_some_and(|c| c.epoch != epoch || c.seq < oldest || c.seq > seq);
        let entries = after.map_or_else(Vec::new, |c| {
            entries
                .iter()
                .filter(|e| gap || e.seq > c.seq)
                .cloned()
                .collect()
        });
        ActivityPage {
            workspace_id,
            cursor: ActivityCursor { epoch, seq },
            gap,
            entries,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn output_bursts_keep_the_latest_anchor_and_never_change_a_read_entry() {
        let journal = Journal::default();
        let id = WorkspaceId::new();
        let epoch = WorkspaceId::new();
        let pane_id = daedalus_proto::PaneId::new();
        let record = |row| {
            journal.record(ActivityKind::AgentOutput {
                pane_id,
                abs_row: row,
                pane_seq: row,
                history_generation: 0,
            })
        };
        let start = journal.read(id, epoch, None).cursor;
        record(2);
        record(5);
        let first = journal.read(id, epoch, Some(start));
        assert_eq!(first.entries.len(), 1);
        assert!(matches!(
            first.entries[0].kind,
            ActivityKind::AgentOutput { abs_row: 5, .. }
        ));
        record(6);
        let next = journal.read(id, epoch, Some(first.cursor));
        assert_eq!(next.entries.len(), 1);
        assert_eq!(next.cursor.seq, 2);
    }

    #[test]
    fn cursors_cover_eviction_restart_and_head_without_replaying_a_first_visit() {
        let log = Journal::default();
        let id = WorkspaceId::new();
        let epoch = WorkspaceId::new();
        let start = log.read(id, epoch, None).cursor;
        for _ in 0..=RETAINED {
            log.record(ActivityKind::Git);
        }
        let baseline = log.read(id, epoch, None);
        assert!(baseline.entries.is_empty());
        assert_eq!(baseline.cursor.seq, RETAINED as u64 + 1);
        let retained = log.read(id, epoch, Some(start));
        assert!(retained.gap);
        assert_eq!(retained.entries.len(), RETAINED);
        assert_eq!(retained.entries[0].seq, 2);
        let contiguous = log.read(id, epoch, Some(ActivityCursor { epoch, seq: 1 }));
        assert!(!contiguous.gap);
        assert_eq!(contiguous.entries, retained.entries);
        let restart = Journal::default().read(id, WorkspaceId::new(), Some(baseline.cursor));
        assert!(restart.gap);
        assert!(restart.entries.is_empty());
    }
}
