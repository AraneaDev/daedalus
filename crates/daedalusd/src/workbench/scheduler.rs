use super::command;
use super::runtime::SourceFields;
use crate::registry::Registry;
use daedalus_proto::workbench::{NamedSource, WidgetSource};
use daedalus_proto::{PaneId, PaneRole, ServerEvent, WorkspaceId, WorkspaceInfo};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, watch};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::Instant;

#[derive(Clone, Copy)]
pub(crate) enum Trigger {
    View,
    Manual,
    Timer,
    Runner,
}

type Key = (WorkspaceId, String, String);
type Entry = Arc<Mutex<Option<Run>>>;

#[derive(Default)]
pub(crate) struct WidgetCommands(Mutex<HashMap<Key, Entry>>);

struct Run {
    specification: WidgetSource,
    fields: SourceFields,
    completion: Instant,
    last_success: Option<Instant>,
}

impl WidgetCommands {
    pub async fn fields(
        &self,
        workspace: &WorkspaceInfo,
        widget: &str,
        source: &NamedSource,
        trigger: Trigger,
    ) -> SourceFields {
        let WidgetSource::Command {
            command: spec,
            refresh,
            output,
        } = &source.source
        else {
            return SourceFields::new();
        };
        if matches!(trigger, Trigger::Timer) && refresh.interval_seconds.is_none()
            || matches!(trigger, Trigger::Runner) && !refresh.on_runner_completion
        {
            return SourceFields::new();
        }
        let key = (workspace.id, widget.into(), source.id.clone());
        let entry = self.0.lock().await.entry(key).or_default().clone();
        let Ok(mut entry) = entry.try_lock() else {
            return SourceFields::from([("label".into(), "Refreshing…".into())]);
        };
        let previous = entry
            .as_ref()
            .filter(|run| run.specification == source.source);
        let due = match trigger {
            Trigger::View => false,
            Trigger::Manual | Trigger::Runner => true,
            Trigger::Timer => previous.is_none_or(|run| {
                run.completion.elapsed()
                    >= Duration::from_secs(u64::from(refresh.interval_seconds.unwrap_or_default()))
            }),
        };
        if !due {
            return previous.map_or_else(SourceFields::new, |run| run.fields.clone());
        }
        let mut fields = command::run(workspace, spec, output).await;
        let last_success = if fields.contains_key("error") {
            if let Some(previous) = previous {
                for (key, value) in &previous.fields {
                    fields.entry(key.clone()).or_insert_with(|| value.clone());
                }
            }
            previous.and_then(|run| run.last_success)
        } else {
            Some(Instant::now())
        };
        if let Some(last_success) = last_success.filter(|_| fields.contains_key("error")) {
            fields.insert(
                "detail.Last successful refresh".into(),
                format!("{} seconds ago", last_success.elapsed().as_secs()),
            );
        }
        *entry = Some(Run {
            specification: source.source.clone(),
            fields: fields.clone(),
            completion: Instant::now(),
            last_success,
        });
        fields
    }
}

pub(crate) fn spawn(
    registry: Arc<Registry>,
    mut shutdown: watch::Receiver<bool>,
) -> JoinHandle<()> {
    let mut events = registry.subscribe_events();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut jobs = JoinSet::new();
        let mut runner_completions: HashMap<PaneId, Instant> = HashMap::new();
        let mut timer_active = false;
        loop {
            tokio::select! {
                _ = shutdown.changed() => break,
                _ = tick.tick(), if !timer_active => {
                    timer_active = true;
                    let registry = registry.clone();
                    jobs.spawn(async move { registry.refresh_scheduled_widgets(None).await; true });
                }
                result = jobs.join_next(), if !jobs.is_empty() => {
                    if matches!(result, Some(Ok(true) | Err(_))) { timer_active = false; }
                }
                event = events.recv() => {
                    let pane_id = match event {
                        Ok(ServerEvent::CommandFinished { pane_id, .. }) => Some((pane_id, false)),
                        Ok(ServerEvent::PaneExited { pane_id, .. }) => Some((pane_id, true)),
                        _ => None,
                    };
                    if let Some((pane_id, exited)) = pane_id
                        && let Ok(pane) = registry.get_pane(pane_id).await
                        && let Ok(summary) = pane.summary().await
                        && summary.role == PaneRole::Runner
                    {
                        runner_completions.retain(|_, finished| finished.elapsed() < Duration::from_millis(250));
                        if exited && runner_completions.remove(&pane_id).is_some() { continue; }
                        if !exited { runner_completions.insert(pane_id, Instant::now()); }
                        let registry = registry.clone();
                        jobs.spawn(async move { registry.refresh_scheduled_widgets(Some(summary.workspace_id)).await; false });
                    }
                }
            }
        }
    })
}
