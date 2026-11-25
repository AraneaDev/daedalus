use super::command;
use super::runtime::SourceFields;
use crate::registry::Registry;
use daedalus_proto::workbench::{NamedSource, WidgetSource, WorkbenchState};
use daedalus_proto::{PaneId, PaneRole, ServerEvent, WorkspaceInfo};
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

type Key = (String, String);
type Entry = Arc<Mutex<CommandState>>;

pub(crate) struct WidgetCommands(Mutex<Option<WorkspaceCommands>>);

#[derive(Default)]
struct WorkspaceCommands {
    revision: u64,
    entries: HashMap<Key, Entry>,
}

impl Default for WidgetCommands {
    fn default() -> Self {
        Self(Mutex::new(Some(WorkspaceCommands::default())))
    }
}

#[derive(Default)]
struct CommandState {
    specification: Option<Arc<WidgetSource>>,
    active: bool,
    pending_runner: Option<(WorkspaceInfo, Arc<WidgetSource>)>,
    previous: Option<Run>,
}

impl CommandState {
    fn configure(&mut self, specification: Option<&WidgetSource>) {
        if self.specification.as_deref() != specification {
            self.specification = specification.cloned().map(Arc::new);
            self.pending_runner = None;
            self.previous = None;
        }
    }

    fn is_current(&self, specification: &Arc<WidgetSource>) -> bool {
        self.specification
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, specification))
    }

    fn remember(&mut self, fields: &mut SourceFields) {
        let last_success = if fields.contains_key("error") {
            if let Some(previous) = &self.previous {
                for (key, value) in &previous.fields {
                    fields.entry(key.clone()).or_insert_with(|| value.clone());
                }
                previous.last_success
            } else {
                None
            }
        } else {
            Some(Instant::now())
        };
        if let Some(last_success) = last_success.filter(|_| fields.contains_key("error")) {
            fields.insert(
                "detail.Last successful refresh".into(),
                format!("{} seconds ago", last_success.elapsed().as_secs()),
            );
        }
        self.previous = Some(Run {
            fields: fields.clone(),
            completion: Instant::now(),
            last_success,
        });
    }
}

struct Run {
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
    specification: Arc<WidgetSource>,
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
    async fn execute(mut self, workspace: &WorkspaceInfo) -> SourceFields {
        let mut queued = None;
        let mut fields = SourceFields::new();
        loop {
            let (workspace, specification) = queued
                .as_ref()
                .map_or((workspace, &self.specification), |(ws, spec)| (ws, spec));
            // The entry lock orders this admission against source retirement.
            let admitted = self.entry.lock().unwrap().is_current(specification);
            if admitted
                && let WidgetSource::Command {
                    command: spec,
                    output,
                    ..
                } = specification.as_ref()
            {
                fields = command::run(workspace, spec, output).await;
            }
            let mut state = self.entry.lock().unwrap();
            if admitted && state.is_current(specification) {
                state.remember(&mut fields);
            }
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
    pub(crate) fn reconcile(&self, snapshot: &WorkbenchState) {
        let mut commands = self.0.lock().unwrap();
        let Some(commands) = commands.as_mut() else {
            return;
        };
        if snapshot.revision <= commands.revision {
            return;
        }
        commands.revision = snapshot.revision;
        commands.entries.retain(|(widget_id, source_id), entry| {
            let document = &snapshot.document;
            let specification = document
                .widgets
                .iter()
                .find(|widget| &widget.id == widget_id)
                .and_then(|widget| document.widget_definition(widget))
                .and_then(|definition| {
                    definition.sources.iter().find(|source| {
                        &source.id == source_id
                            && matches!(source.source, WidgetSource::Command { .. })
                    })
                })
                .map(|source| &source.source);
            let mut state = entry.lock().unwrap();
            state.configure(specification);
            // A running job keeps the slot until it finishes. Re-adding the same source
            // meanwhile must not start a competing process.
            state.active || specification.is_some()
        });
    }

    pub(crate) fn close(&self) {
        if let Some(commands) = self.0.lock().unwrap().take() {
            for entry in commands.entries.values() {
                entry.lock().unwrap().configure(None);
            }
        }
    }

    fn refresh(
        &self,
        revision: u64,
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
        let key = (widget.into(), source.id.clone());
        let mut commands = self.0.lock().unwrap();
        let Some(commands) = commands
            .as_mut()
            .filter(|commands| commands.revision == revision)
        else {
            return empty();
        };
        let entry = commands
            .entries
            .entry(key)
            .or_insert_with(|| {
                Arc::new(Mutex::new(CommandState {
                    specification: Some(Arc::new(source.source.clone())),
                    ..CommandState::default()
                }))
            })
            .clone();
        let mut state = entry.lock().unwrap();
        let Some(specification) = state.specification.clone() else {
            return empty();
        };
        if state.active {
            if matches!(trigger, Trigger::Runner) {
                state.pending_runner = Some((workspace.clone(), specification));
            }
            return Refresh::Cached(SourceFields::from([("label".into(), "Refreshing…".into())]));
        }
        let previous = state.previous.as_ref();
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
                specification,
                owns_entry: true,
            })
        } else {
            Refresh::Cached(previous.map_or_else(SourceFields::new, |run| run.fields.clone()))
        }
    }

    pub async fn fields(
        &self,
        revision: u64,
        workspace: &WorkspaceInfo,
        widget: &str,
        source: &NamedSource,
        trigger: Trigger,
    ) -> SourceFields {
        match self.refresh(revision, workspace, widget, source, trigger) {
            Refresh::Cached(fields) => fields,
            Refresh::Run(run) => run.execute(workspace).await,
        }
    }

    pub fn schedule(
        &self,
        revision: u64,
        workspace: Arc<WorkspaceInfo>,
        widget: &str,
        source: &NamedSource,
        trigger: Trigger,
        jobs: &mut JoinSet<()>,
    ) {
        if let Refresh::Run(run) = self.refresh(revision, &workspace, widget, source, trigger) {
            jobs.spawn(async move {
                run.execute(&workspace).await;
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
    use daedalus_proto::workbench::{
        CommandOutput, RefreshPolicy, WorkspaceCommand, WorkspaceDocument,
    };

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

    async fn workspace_at(path: &std::path::Path) -> Arc<WorkspaceInfo> {
        let (events, _) = tokio::sync::broadcast::channel(16);
        let registry = Registry::new(
            Supervisor::with_mode(IsolationMode::PgidFallback),
            crate::shell_integration::ShellIntegration::prepare(path).unwrap(),
            events,
        );
        Arc::new(
            registry
                .create_workspace("Scheduling".into(), path.into(), HashMap::new())
                .await
                .unwrap(),
        )
    }

    #[tokio::test]
    async fn retired_sources_cannot_start_reserved_runs_or_be_revived_by_stale_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = workspace_at(dir.path()).await;
        let commands = WidgetCommands::default();
        let source = command_source("retired", "touch unexpected");
        let Refresh::Run(run) = commands.refresh(0, &workspace, "test", &source, Trigger::Runner)
        else {
            panic!("the first runner trigger should reserve the slot");
        };
        let removed = WorkbenchState {
            revision: 1,
            document: WorkspaceDocument::new("Scheduling".into()),
            repo_baseline: None,
        };
        commands.reconcile(&removed);
        assert!(run.execute(&workspace).await.is_empty());
        assert!(
            commands
                .fields(0, &workspace, "test", &source, Trigger::Runner)
                .await
                .is_empty()
        );
        commands.close();
        commands.reconcile(&removed);
        assert!(
            commands
                .fields(1, &workspace, "test", &source, Trigger::Runner)
                .await
                .is_empty()
        );
        assert!(!dir.path().join("unexpected").exists());
    }

    #[tokio::test]
    async fn independent_timers_and_coalesced_runner_requests_keep_one_run_per_source() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = workspace_at(dir.path()).await;
        let commands = WidgetCommands::default();
        let slow = command_source(
            "slow",
            "echo run >> slow; while [ ! -f release ]; do sleep 0.01; done",
        );
        let fast = command_source("fast", "echo run >> fast");
        let runs = |file| std::fs::read_to_string(dir.path().join(file)).unwrap_or_default();
        let mut jobs = JoinSet::new();
        let schedule = |source, trigger, jobs: &mut JoinSet<()>| {
            commands.schedule(0, workspace.clone(), "test", source, trigger, jobs);
        };

        schedule(&slow, Trigger::Timer, &mut jobs);
        wait_for(|| runs("slow") == "run\n").await;
        for _ in 0..3 {
            schedule(&slow, Trigger::Runner, &mut jobs);
            schedule(&slow, Trigger::Timer, &mut jobs);
        }
        assert_eq!(
            commands
                .fields(0, &workspace, "test", &slow, Trigger::Manual)
                .await["label"],
            "Refreshing…"
        );

        schedule(&fast, Trigger::Timer, &mut jobs);
        jobs.join_next().await.unwrap().unwrap();
        {
            let commands = commands.0.lock().unwrap();
            let entry = &commands.as_ref().unwrap().entries[&("test".into(), "fast".into())];
            entry.lock().unwrap().previous.as_mut().unwrap().completion -= Duration::from_secs(1);
        }
        schedule(&fast, Trigger::Timer, &mut jobs);
        jobs.join_next().await.unwrap().unwrap();
        assert_eq!(runs("fast"), "run\nrun\n");
        assert_eq!(runs("slow"), "run\n");

        std::fs::write(dir.path().join("release"), "").unwrap();
        jobs.join_next().await.unwrap().unwrap();
        assert_eq!(runs("slow"), "run\nrun\n");
        assert!(jobs.is_empty());
    }
}
