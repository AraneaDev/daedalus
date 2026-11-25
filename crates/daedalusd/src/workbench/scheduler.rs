use super::command;
use super::runtime::SourceFields;
use crate::registry::Registry;
use daedalus_proto::workbench::{NamedSource, WidgetSource};
use daedalus_proto::{PaneId, PaneRole, ServerEvent, WorkspaceId, WorkspaceInfo};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;
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
type Entry = Arc<Mutex<CommandState>>;

#[derive(Default)]
pub(crate) struct WidgetCommands(Mutex<HashMap<Key, Entry>>);

#[derive(Default)]
struct CommandState {
    active: bool,
    pending_runner: Option<(WorkspaceInfo, NamedSource)>,
    previous: Option<Run>,
}

struct Run {
    specification: WidgetSource,
    fields: SourceFields,
    completion: Instant,
    last_success: Option<Instant>,
}

enum Refresh {
    Cached(SourceFields),
    Run(ActiveRun),
}

struct ActiveRun {
    entry: Entry,
    owns_entry: bool,
}

impl Drop for ActiveRun {
    fn drop(&mut self) {
        if self.owns_entry {
            let mut state = self.entry.lock().unwrap();
            state.active = false;
            state.pending_runner = None;
        }
    }
}

impl ActiveRun {
    async fn execute(mut self, workspace: &WorkspaceInfo, source: &NamedSource) -> SourceFields {
        let mut queued = None;
        loop {
            let (workspace, source) = queued
                .as_ref()
                .map_or((workspace, source), |(ws, source)| (ws, source));
            let WidgetSource::Command {
                command: spec,
                output,
                ..
            } = &source.source
            else {
                return SourceFields::new();
            };
            let mut fields = command::run(workspace, spec, output).await;
            let mut state = self.entry.lock().unwrap();
            let previous = state
                .previous
                .as_ref()
                .filter(|run| run.specification == source.source);
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
            state.previous = Some(Run {
                specification: source.source.clone(),
                fields: fields.clone(),
                completion: Instant::now(),
                last_success,
            });
            // Consume the queued trigger or release the slot under the same
            // lock. A concurrent runner cannot enqueue after the final check.
            queued = state.pending_runner.take();
            if queued.is_none() {
                state.active = false;
                self.owns_entry = false;
                return fields;
            }
        }
    }
}

impl WidgetCommands {
    fn refresh(
        &self,
        workspace: &WorkspaceInfo,
        widget: &str,
        source: &NamedSource,
        trigger: Trigger,
    ) -> Refresh {
        let empty = || Refresh::Cached(SourceFields::new());
        let WidgetSource::Command { refresh, .. } = &source.source else {
            return empty();
        };
        if matches!(trigger, Trigger::Timer) && refresh.interval_seconds.is_none()
            || matches!(trigger, Trigger::Runner) && !refresh.on_runner_completion
        {
            return empty();
        }
        let key = (workspace.id, widget.into(), source.id.clone());
        let entry = self.0.lock().unwrap().entry(key).or_default().clone();
        let mut state = entry.lock().unwrap();
        if state.active {
            if matches!(trigger, Trigger::Runner) {
                state.pending_runner = Some((workspace.clone(), source.clone()));
            }
            return Refresh::Cached(SourceFields::from([("label".into(), "Refreshing…".into())]));
        }
        let previous = state
            .previous
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
        if due {
            state.active = true;
            drop(state);
            Refresh::Run(ActiveRun {
                entry,
                owns_entry: true,
            })
        } else {
            Refresh::Cached(previous.map_or_else(SourceFields::new, |run| run.fields.clone()))
        }
    }

    pub async fn fields(
        &self,
        workspace: &WorkspaceInfo,
        widget: &str,
        source: &NamedSource,
        trigger: Trigger,
    ) -> SourceFields {
        match self.refresh(workspace, widget, source, trigger) {
            Refresh::Cached(fields) => fields,
            Refresh::Run(run) => run.execute(workspace, source).await,
        }
    }

    pub fn schedule(
        &self,
        workspace: Arc<WorkspaceInfo>,
        widget: &str,
        source: &NamedSource,
        trigger: Trigger,
        jobs: &mut JoinSet<()>,
    ) {
        if let Refresh::Run(run) = self.refresh(&workspace, widget, source, trigger) {
            let source = source.clone();
            jobs.spawn(async move {
                run.execute(&workspace, &source).await;
            });
        }
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
        loop {
            tokio::select! {
                _ = shutdown.changed() => break,
                _ = tick.tick() => registry.refresh_scheduled_widgets(None, &mut jobs).await,
                _ = jobs.join_next(), if !jobs.is_empty() => {}
                event = events.recv() => {
                    let pane_id = match event {
                        Ok(ServerEvent::CommandFinished { pane_id, .. }) => Some((pane_id, false)),
                        Ok(ServerEvent::PaneExited { pane_id, .. }) => Some((pane_id, true)),
                        _ => None,
                    };
                    if let Some((pane_id, exited)) = pane_id
                        && let Ok(pane) = registry.get_pane(pane_id).await
                        && pane.role() == PaneRole::Runner
                    {
                        runner_completions.retain(|_, finished| finished.elapsed() < Duration::from_millis(250));
                        if exited && runner_completions.remove(&pane_id).is_some() { continue; }
                        if !exited { runner_completions.insert(pane_id, Instant::now()); }
                        registry.refresh_scheduled_widgets(Some(pane.workspace_id()), &mut jobs).await;
                    }
                }
            }
        }
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::supervisor::{IsolationMode, Supervisor};
    use daedalus_proto::workbench::{CommandOutput, RefreshPolicy, WorkspaceCommand};

    fn command_source(id: &str, script: &str) -> NamedSource {
        NamedSource {
            id: id.into(),
            source: WidgetSource::Command {
                command: WorkspaceCommand {
                    argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
                    env: Vec::new(),
                    timeout_seconds: Some(5),
                },
                refresh: RefreshPolicy {
                    interval_seconds: Some(1),
                    on_runner_completion: true,
                },
                output: CommandOutput::ExitStatus,
            },
        }
    }

    async fn wait_for(predicate: impl Fn() -> bool) {
        tokio::time::timeout(Duration::from_secs(4), async {
            while !predicate() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("command did not reach the expected state");
    }

    #[tokio::test]
    async fn independent_timers_and_coalesced_runner_requests_keep_one_run_per_source() {
        let dir = tempfile::tempdir().unwrap();
        let (events, _) = tokio::sync::broadcast::channel(16);
        let registry = Registry::new(
            Supervisor::with_mode(IsolationMode::PgidFallback),
            crate::shell_integration::ShellIntegration::prepare(dir.path()).unwrap(),
            events,
        );
        let workspace = Arc::new(
            registry
                .create_workspace("Scheduling".into(), dir.path().into(), HashMap::new())
                .await
                .unwrap(),
        );
        let commands = WidgetCommands::default();
        let slow = command_source(
            "slow",
            "echo run >> slow; while [ ! -f release ]; do sleep 0.01; done",
        );
        let fast = command_source("fast", "echo run >> fast");
        let runs = |file| std::fs::read_to_string(dir.path().join(file)).unwrap_or_default();
        let mut jobs = JoinSet::new();

        commands.schedule(workspace.clone(), "test", &slow, Trigger::Timer, &mut jobs);
        wait_for(|| runs("slow") == "run\n").await;
        for _ in 0..3 {
            commands.schedule(workspace.clone(), "test", &slow, Trigger::Runner, &mut jobs);
            commands.schedule(workspace.clone(), "test", &slow, Trigger::Timer, &mut jobs);
        }
        assert_eq!(
            commands
                .fields(&workspace, "test", &slow, Trigger::Manual)
                .await["label"],
            "Refreshing…"
        );

        commands.schedule(workspace.clone(), "test", &fast, Trigger::Timer, &mut jobs);
        jobs.join_next().await.unwrap().unwrap();
        {
            let entries = commands.0.lock().unwrap();
            let entry = &entries[&(workspace.id, "test".into(), "fast".into())];
            entry.lock().unwrap().previous.as_mut().unwrap().completion -= Duration::from_secs(1);
        }
        commands.schedule(workspace.clone(), "test", &fast, Trigger::Timer, &mut jobs);
        jobs.join_next().await.unwrap().unwrap();
        assert_eq!(runs("fast"), "run\nrun\n");
        assert_eq!(runs("slow"), "run\n");

        std::fs::write(dir.path().join("release"), "").unwrap();
        jobs.join_next().await.unwrap().unwrap();
        assert_eq!(runs("slow"), "run\nrun\n");
        assert!(jobs.is_empty());
    }
}
