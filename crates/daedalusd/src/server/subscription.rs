use daedalus_proto::{PaneId, ServerEvent, ServerMessage, WorkspaceId};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::{RwLock, broadcast, mpsc};
use tracing::warn;

#[derive(Default)]
pub(super) struct Subscription {
    filter: RwLock<SubscriptionFilter>,
}

impl Subscription {
    pub(super) async fn subscribe(
        &self,
        pane_ids: Option<Vec<PaneId>>,
        project_setup_events: bool,
    ) {
        let mut filter = self.filter.write().await;
        filter.active = true;
        filter.pane_ids = pane_ids.map(|ids| ids.into_iter().collect());
        filter.project_setup_events = project_setup_events;
    }

    pub(super) async fn unsubscribe(&self) {
        *self.filter.write().await = SubscriptionFilter::default();
    }

    pub(super) async fn watch_membership(&self) {
        self.filter.write().await.workspace_membership_events = true;
    }

    pub(super) async fn watch_workbench(&self, workspace_id: WorkspaceId) {
        self.filter.write().await.workbench = Some(workspace_id);
    }
}

#[derive(Clone, Default)]
struct SubscriptionFilter {
    active: bool,
    pane_ids: Option<HashSet<PaneId>>,
    /// Older UIs drop the connection on an event variant they do not know, so
    /// `ProjectSetupChanged` is sent only to connections that asked for it.
    project_setup_events: bool,
    workspace_membership_events: bool,
    workbench: Option<daedalus_proto::WorkspaceId>,
}

impl SubscriptionFilter {
    fn matches(&self, event: &ServerEvent) -> bool {
        if let ServerEvent::WorkbenchChanged { workspace_id, .. } = event {
            return self.workbench == Some(*workspace_id);
        }
        if matches!(event, ServerEvent::WorkspaceMembershipChanged) {
            return self.workspace_membership_events;
        }
        if !self.active {
            return false;
        }
        if matches!(event, ServerEvent::ProjectSetupChanged { .. }) {
            return self.project_setup_events;
        }
        let Some(allowed) = &self.pane_ids else {
            return true;
        };
        match event {
            ServerEvent::GridDelta(d) => allowed.contains(&d.pane_id),
            ServerEvent::PaneStatusChanged { pane_id, .. }
            | ServerEvent::AgentStateChanged { pane_id, .. }
            | ServerEvent::CommandFinished { pane_id, .. }
            | ServerEvent::PaneExited { pane_id, .. }
            | ServerEvent::ResyncRequired { pane_id } => allowed.contains(pane_id),
            ServerEvent::WorkspaceRuntimeUpdated { .. }
            | ServerEvent::ExternalStatusUpdated { .. }
            | ServerEvent::ProjectSetupChanged { .. }
            | ServerEvent::WorkspaceMembershipChanged
            | ServerEvent::WorkbenchChanged { .. } => false,
        }
    }
}

enum DrainResult {
    Drained,
    StillFull,
    Closed,
}

fn drain_pending_resync(
    pending_resync: &mut HashSet<PaneId>,
    out_tx: &mpsc::Sender<ServerMessage>,
) -> DrainResult {
    let pane_ids: Vec<PaneId> = pending_resync.drain().collect();
    for (idx, pane_id) in pane_ids.iter().enumerate() {
        let msg = ServerMessage::Event(ServerEvent::ResyncRequired { pane_id: *pane_id });
        match out_tx.try_send(msg) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                pending_resync.extend(pane_ids[idx..].iter().copied());
                return DrainResult::StillFull;
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                return DrainResult::Closed;
            }
        }
    }
    DrainResult::Drained
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum SubscriberExit {
    Closed,
    Lagged,
}

pub(super) async fn forward_subscriber_events(
    mut event_rx: broadcast::Receiver<ServerEvent>,
    sub_out_tx: mpsc::Sender<ServerMessage>,
    sub_filter: Arc<Subscription>,
) -> SubscriberExit {
    let mut pending_resync: HashSet<PaneId> = HashSet::new();

    loop {
        let recv_res = if pending_resync.is_empty() {
            event_rx.recv().await
        } else {
            tokio::select! {
                permit = sub_out_tx.reserve() => {
                    match permit {
                        Ok(permit) => {
                            if let Some(&pane_id) = pending_resync.iter().next() {
                                pending_resync.remove(&pane_id);
                                permit.send(ServerMessage::Event(ServerEvent::ResyncRequired {
                                    pane_id,
                                }));
                            }
                            if let DrainResult::Closed =
                                drain_pending_resync(&mut pending_resync, &sub_out_tx)
                            {
                                break;
                            }
                            continue;
                        }
                        Err(_) => break,
                    }
                }
                res = event_rx.recv() => res,
            }
        };

        match recv_res {
            Ok(evt) => {
                if !sub_filter.filter.read().await.matches(&evt) {
                    continue;
                }
                match drain_pending_resync(&mut pending_resync, &sub_out_tx) {
                    DrainResult::Drained => {}
                    DrainResult::StillFull => {
                        if let ServerEvent::GridDelta(delta) = &evt {
                            pending_resync.insert(delta.pane_id);
                            continue;
                        }
                    }
                    DrainResult::Closed => break,
                }
                match &evt {
                    ServerEvent::GridDelta(delta) => {
                        let pane_id = delta.pane_id;
                        match sub_out_tx.try_send(ServerMessage::Event(evt)) {
                            Ok(()) => {}
                            Err(mpsc::error::TrySendError::Full(_)) => {
                                pending_resync.insert(pane_id);
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => break,
                        }
                    }
                    _ => {
                        if sub_out_tx.send(ServerMessage::Event(evt)).await.is_err() {
                            break;
                        }
                        if let DrainResult::Closed =
                            drain_pending_resync(&mut pending_resync, &sub_out_tx)
                        {
                            break;
                        }
                    }
                }
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                let has_subscription = {
                    let filter = sub_filter.filter.read().await;
                    filter.active
                        || filter.workspace_membership_events
                        || filter.workbench.is_some()
                };
                if has_subscription {
                    // Missing updates may affect any pane or workspace.
                    warn!(
                        skipped,
                        "event subscriber lagged; disconnecting stale client"
                    );
                    return SubscriberExit::Lagged;
                }
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
    SubscriberExit::Closed
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use daedalus_proto::{CursorState, GridDelta};
    use std::path::PathBuf;
    use std::time::Duration;

    pub(in crate::server) fn dummy_delta(pane_id: PaneId, seq: u64) -> ServerEvent {
        ServerEvent::GridDelta(GridDelta {
            activity: None,
            insight: None,
            terminal_modes: Default::default(),
            pane_id,
            seq,
            cols: 80,
            rows: 24,
            cursor: CursorState::default(),
            history_generation: 0,
            scrollback_len: None,
            dirty_rows: Vec::new(),
        })
    }

    #[test]
    fn project_setup_events_require_opt_in_for_global_and_scoped_filters() {
        let event = ServerEvent::ProjectSetupChanged {
            root_dir: PathBuf::from("/p"),
            assignments: Vec::new(),
        };
        for pane_ids in [None, Some(HashSet::new())] {
            let mut filter = SubscriptionFilter {
                active: true,
                pane_ids,
                project_setup_events: false,
                ..Default::default()
            };
            assert!(!filter.matches(&event));
            filter.project_setup_events = true;
            assert!(filter.matches(&event));
        }
    }

    #[tokio::test]
    async fn resync_required_is_delivered_after_backpressure_drains() {
        let (event_tx, _) = broadcast::channel(16);
        let event_rx = event_tx.subscribe();
        let (out_tx, mut out_rx) = mpsc::channel(2);
        let filter = Arc::new(Subscription::default());
        filter.subscribe(None, false).await;

        let handle = tokio::spawn(forward_subscriber_events(event_rx, out_tx, filter));

        let pane_id = PaneId::new();
        for seq in 1..=4 {
            event_tx.send(dummy_delta(pane_id, seq)).unwrap();
        }

        // Yield so forwarder fills the 2-slot mpsc and queues pending_resync
        tokio::time::sleep(Duration::from_millis(20)).await;

        let m1 = out_rx.recv().await.unwrap();
        let m2 = out_rx.recv().await.unwrap();
        assert!(matches!(
            m1,
            ServerMessage::Event(ServerEvent::GridDelta(_))
        ));
        assert!(matches!(
            m2,
            ServerMessage::Event(ServerEvent::GridDelta(_))
        ));

        // Even with no further events on event_tx, ResyncRequired must arrive
        let m3 = tokio::time::timeout(Duration::from_secs(1), out_rx.recv())
            .await
            .expect("timed out waiting for ResyncRequired")
            .unwrap();
        assert_eq!(
            m3,
            ServerMessage::Event(ServerEvent::ResyncRequired { pane_id })
        );

        drop(event_tx);
        tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("forwarder did not exit on broadcast close")
            .unwrap();
    }

    #[tokio::test]
    async fn broadcast_lag_ends_subscription_instead_of_forwarding_incomplete_state() {
        let (events, receiver) = broadcast::channel(2);
        let (out_tx, mut out_rx) = mpsc::channel(4);
        let filter = Arc::new(Subscription {
            filter: RwLock::new(SubscriptionFilter {
                active: true,
                ..Default::default()
            }),
        });
        let skipped_pane = PaneId::new();
        let retained_pane = PaneId::new();
        events.send(dummy_delta(skipped_pane, 1)).unwrap();
        events
            .send(ServerEvent::ProjectSetupChanged {
                root_dir: PathBuf::from("/workspace"),
                assignments: Vec::new(),
            })
            .unwrap();
        events.send(dummy_delta(retained_pane, 1)).unwrap();
        events.send(dummy_delta(retained_pane, 2)).unwrap();

        let exit = tokio::time::timeout(
            Duration::from_secs(1),
            forward_subscriber_events(receiver, out_tx, filter),
        )
        .await
        .expect("a lagged stream cannot continue with a partial view");
        assert_eq!(exit, SubscriberExit::Lagged);
        assert!(out_rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn event_forwarder_exits_when_outbound_receiver_closes() {
        let (event_tx, _) = broadcast::channel(16);
        let event_rx = event_tx.subscribe();
        let (out_tx, out_rx) = mpsc::channel(1);
        let filter = Arc::new(Subscription::default());
        filter.subscribe(None, false).await;

        let handle = tokio::spawn(forward_subscriber_events(event_rx, out_tx, filter));
        drop(out_rx);

        event_tx.send(dummy_delta(PaneId::new(), 1)).unwrap();

        tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("forwarder did not exit when outbound channel closed")
            .unwrap();
    }
}
