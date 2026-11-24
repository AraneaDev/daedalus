use std::collections::{HashMap, VecDeque};

use crate::bridge::UiEvent;
use crate::state::{PaneViewState, WorkspaceView};
use daedalus_proto::{
    ActivityEntry, ActivityKind, AgentState, GridRow, PaneId, PaneRole, ServerEvent, WorkspaceId,
};

const MAX_EVENTS: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecapEntry {
    pub label: String,
    pub pane: Option<PaneId>,
}

impl From<&str> for RecapEntry {
    fn from(label: &str) -> Self {
        Self {
            label: label.into(),
            pane: None,
        }
    }
}

#[derive(Debug, Default)]
struct Visit {
    journal_lines: Option<Vec<RecapEntry>>,
    foreground_activity: usize,
    has_left: bool,
    gap: bool,
    recent: VecDeque<RecapEntry>,
    agent_line: Option<RecapEntry>,
}

#[derive(Debug, Default)]
pub struct Recap {
    pub keyboard_row: Option<RecapEntry>,
    selected: Option<WorkspaceId>,
    unfocused: bool,
    disconnected: bool,
    unsupported: bool,
    visits: HashMap<WorkspaceId, Visit>,
}

fn short_label(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_control())
        .take(80)
        .collect()
}

fn excerpt(row: &GridRow) -> Option<String> {
    let text: String = row
        .cells
        .iter()
        .filter(|cell| cell.width > 0)
        .flat_map(|cell| {
            if cell.text.is_empty() {
                " "
            } else {
                &cell.text
            }
            .chars()
        })
        .filter(|ch| !ch.is_control())
        .skip_while(|ch| ch.is_whitespace())
        .take(240)
        .collect();
    let line = text.trim_end();
    let prompt = line.split_whitespace().next().unwrap_or_default();
    if !line.chars().any(char::is_alphanumeric)
        || prompt.ends_with(['$', '#', '❯', '>', 'λ', '%'])
        || line.starts_with(['❯', '›', '»'])
    {
        return None;
    }
    Some(format!("Agent terminal · {line}"))
}

/// A later redraw can replace text at the same absolute row in a terminal UI.
/// Only quote a live viewport that still matches the exact output frame.
pub fn activity_excerpt(
    entry: &ActivityEntry,
    panes: &HashMap<PaneId, PaneViewState>,
) -> Option<String> {
    let ActivityKind::AgentOutput {
        pane_id,
        abs_row,
        history_generation,
        pane_seq,
    } = entry.kind
    else {
        return None;
    };
    let pane = panes.get(&pane_id)?;
    if pane.summary.role != PaneRole::Agent
        || pane.history_generation != history_generation
        || pane.last_seq != pane_seq
    {
        return None;
    }
    pane.viewport_rows
        .iter()
        .find(|r| r.abs_row == abs_row)
        .and_then(excerpt)
}

impl Recap {
    fn visible(&self) -> Option<WorkspaceId> {
        self.selected.filter(|_| !self.unfocused)
    }

    fn transition(&mut self, previous: Option<WorkspaceId>) {
        if previous == self.visible() {
            return;
        }
        self.keyboard_row = None;
        if let Some(id) = previous
            && let Some(visit) = self.visits.get_mut(&id)
        {
            *visit = Visit {
                has_left: true,
                gap: self.disconnected,
                ..Visit::default()
            };
        }
        if let Some(id) = self.visible() {
            self.visits.entry(id).or_default();
        }
    }

    pub fn select(&mut self, selected: Option<WorkspaceId>) {
        let previous = self.visible();
        self.selected = selected;
        self.transition(previous);
    }

    pub fn focus(&mut self, focused: bool) {
        let previous = self.visible();
        self.unfocused = !focused;
        self.transition(previous);
    }

    fn record(&mut self, id: WorkspaceId, pane: Option<PaneId>, label: impl FnOnce() -> String) {
        if self.visible() != Some(id)
            && let Some(visit) = self.visits.get_mut(&id)
        {
            visit.recent.truncate(MAX_EVENTS - 1);
            visit.recent.push_front(RecapEntry {
                label: label(),
                pane,
            });
        }
    }

    pub fn observe(
        &mut self,
        event: &UiEvent,
        workspaces: &[WorkspaceView],
        panes: &HashMap<PaneId, PaneViewState>,
    ) {
        let workspace = |id| workspaces.iter().find(|ws| ws.id == id);
        let pane = |id: PaneId| panes.get(&id).map(|view| &view.summary);
        match event {
            UiEvent::JournalCapability(supported) => self.unsupported = !*supported,
            UiEvent::Connected => self.disconnected = false,
            UiEvent::Disconnected => {
                self.disconnected = true;
                let visible = self.visible();
                for (id, visit) in &mut self.visits {
                    if Some(*id) != visible {
                        visit.gap = true;
                    }
                }
            }
            UiEvent::WorkspacesLoaded(list) | UiEvent::WorkspaceCloseFailed(list) => {
                self.visits
                    .retain(|id, _| list.iter().any(|ws| ws.id == *id));
            }
            UiEvent::AgentStateChanged {
                workspace_id,
                pane_id,
                state,
            } => {
                if pane(*pane_id).and_then(|p| p.agent_state) != Some(*state) {
                    self.record(*workspace_id, Some(*pane_id), || match state {
                        AgentState::WaitingForApproval => "Check agent · approval signal".into(),
                        AgentState::Error => "Agent reported an error".into(),
                        AgentState::Idle => "Agent became idle".into(),
                        AgentState::Thinking => "Agent started thinking".into(),
                    });
                }
            }
            UiEvent::WorkspaceRuntimeUpdated {
                workspace_id,
                bound_ports,
                containers,
                git_status,
                git_branch,
            } => {
                if let Some(ws) = workspace(*workspace_id) {
                    if ws.git_branch != *git_branch || ws.git_status != *git_status {
                        self.record(*workspace_id, None, || "Git status changed".into());
                    }
                    if ws.bound_ports != *bound_ports {
                        self.record(*workspace_id, None, || {
                            format!("Ports changed · {} bound", bound_ports.len())
                        });
                    }
                    if ws.containers != *containers {
                        self.record(*workspace_id, None, || {
                            if let Some(changed) =
                                containers.iter().find(|c| !ws.containers.contains(c))
                            {
                                let name = changed.service.as_deref().unwrap_or(&changed.name);
                                let status = changed.health.as_deref().unwrap_or(&changed.state);
                                format!("{} reported {}", short_label(name), short_label(status))
                            } else if let Some(removed) =
                                ws.containers.iter().find(|c| !containers.contains(c))
                            {
                                format!("{} left inventory", short_label(&removed.name))
                            } else {
                                "Container inventory updated".into()
                            }
                        });
                    }
                }
            }
            UiEvent::ServerEvent(ServerEvent::GridDelta(delta)) => {
                if let Some(view) = panes.get(&delta.pane_id)
                    && (view.last_seq == 0 || delta.seq > view.last_seq)
                    && delta.history_generation >= view.history_generation
                    && view.summary.role == PaneRole::Agent
                    && self.visible() != Some(view.summary.workspace_id)
                    && let Some(visit) = self.visits.get_mut(&view.summary.workspace_id)
                {
                    if let Some(line) = delta.dirty_rows.iter().rev().find_map(excerpt) {
                        visit.agent_line = Some(RecapEntry {
                            label: line,
                            pane: Some(delta.pane_id),
                        });
                    } else if visit.agent_line.is_none() {
                        visit.agent_line = Some(RecapEntry {
                            label: "Agent pane produced output".into(),
                            pane: Some(delta.pane_id),
                        });
                    }
                }
            }
            UiEvent::ServerEvent(ServerEvent::ResyncRequired { pane_id }) => {
                if let Some(p) = pane(*pane_id)
                    && self.visible() != Some(p.workspace_id)
                    && let Some(visit) = self.visits.get_mut(&p.workspace_id)
                {
                    visit.gap = true;
                }
            }
            UiEvent::ServerEvent(ServerEvent::CommandFinished {
                pane_id, exit_code, ..
            }) => {
                if let Some(p) = pane(*pane_id) {
                    self.record(p.workspace_id, Some(*pane_id), || {
                        format!("{} command · exit {exit_code}", p.role)
                    });
                }
            }
            UiEvent::ServerEvent(ServerEvent::PaneExited { pane_id, exit_code }) => {
                if let Some(p) = pane(*pane_id) {
                    self.record(p.workspace_id, Some(*pane_id), || match exit_code {
                        Some(code) => format!("{} exited · code {code}", p.role),
                        None => format!("{} exited", p.role),
                    });
                }
            }
            UiEvent::ServerEvent(ServerEvent::ExternalStatusUpdated {
                workspace_id,
                status,
            }) if workspace(*workspace_id).is_some_and(|ws| {
                !ws.external_statuses.iter().any(|old| {
                    old.source == status.source
                        && (old.updated_at_ms > status.updated_at_ms
                            || (old.label == status.label
                                && old.level == status.level
                                && old.detail == status.detail))
                })
            }) =>
            {
                self.record(*workspace_id, None, || {
                    if status.label.trim().is_empty() {
                        return format!("{} cleared status", short_label(&status.source));
                    }
                    format!(
                        "{} reported {}",
                        short_label(&status.source),
                        short_label(&status.label)
                    )
                });
            }
            _ => {}
        }
    }

    pub fn journal(
        &mut self,
        page: &daedalus_proto::ActivityPage,
        first: bool,
        gap: bool,
        panes: &HashMap<PaneId, PaneViewState>,
    ) {
        if self.visible() != Some(page.workspace_id) {
            return;
        }
        let visit = self.visits.entry(page.workspace_id).or_default();
        let mut lines = Vec::new();
        if gap {
            lines.push("Update gap · partial history".into());
        }
        let latest_output = page
            .entries
            .iter()
            .rev()
            .find(|e| matches!(e.kind, ActivityKind::AgentOutput { .. }));
        for entry in latest_output.into_iter().chain(
            page.entries
                .iter()
                .rev()
                .filter(|e| !matches!(e.kind, ActivityKind::AgentOutput { .. })),
        ) {
            let label = activity_excerpt(entry, panes).unwrap_or_else(|| entry.kind.to_string());
            let pane = match entry.kind {
                ActivityKind::AgentOutput { pane_id, .. }
                | ActivityKind::Agent { pane_id, .. }
                | ActivityKind::Command { pane_id, .. }
                | ActivityKind::Exit { pane_id, .. } => Some(pane_id),
                _ => None,
            };
            let row = RecapEntry { label, pane };
            if !lines.contains(&row) {
                lines.push(row);
            }
            if lines.len() >= MAX_EVENTS + usize::from(gap) {
                break;
            }
        }
        if lines.is_empty() {
            lines.push(
                if first {
                    "First visit"
                } else {
                    "No changes observed while away"
                }
                .into(),
            );
        }
        visit.journal_lines = Some(lines);
    }

    pub fn foreground_activity(&mut self, id: WorkspaceId, count: usize) {
        if let Some(visit) = self.visits.get_mut(&id) {
            visit.foreground_activity = visit.foreground_activity.saturating_add(count);
        }
    }

    pub fn lines(&self, id: WorkspaceId) -> Vec<String> {
        self.entries(id)
            .into_iter()
            .map(|entry| entry.label)
            .collect()
    }

    pub fn entries(&self, id: WorkspaceId) -> Vec<RecapEntry> {
        let visit = self.visits.get(&id);
        let mut lines = Vec::new();
        if self.disconnected {
            lines.push("Disconnected · status may be stale".into());
        } else if visit.is_some_and(|v| v.gap) {
            lines.push("Update gap · partial history".into());
        }
        if let Some(journal) = visit.and_then(|v| v.journal_lines.as_ref()) {
            if !self.disconnected {
                lines.clear();
            }
            lines.extend(journal.iter().cloned());
            if let Some(v) = visit
                && v.foreground_activity > 0
            {
                lines.push(RecapEntry {
                    label: format!("New activity · {} updates", v.foreground_activity),
                    pane: None,
                });
            }
            if self.unsupported {
                lines.insert(0, "History unavailable on this daemon".into());
            }
            return lines;
        }
        if self.unsupported {
            lines.push("History unavailable on this daemon".into());
        }
        if let Some(line) = visit.and_then(|v| v.agent_line.as_ref()) {
            lines.push(line.clone());
        }
        match visit {
            Some(v) if !v.recent.is_empty() => lines.extend(v.recent.iter().cloned()),
            Some(v) if v.has_left && v.agent_line.is_none() && !v.gap && !self.disconnected => {
                lines.push("No changes observed while away".into())
            }
            _ if !visit.is_some_and(|v| v.has_left) => {
                lines.push("First visit this session".into())
            }
            _ => {}
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizable_prompts_and_echoed_commands_do_not_become_quotes() {
        let row = |text: &str| GridRow {
            row_index: 0,
            abs_row: 1,
            cells: vec![daedalus_proto::Cell {
                text: text.into(),
                ..Default::default()
            }],
        };
        for text in ["❯", "$ cargo test", "user@host:~/project$ printenv", "   "] {
            assert_eq!(excerpt(&row(text)), None);
        }
        assert_eq!(
            excerpt(&row("  build passed  ")).as_deref(),
            Some("Agent terminal · build passed")
        );
    }

    #[test]
    fn old_peers_label_missing_history_and_disconnect_marks_last_known_state() {
        let mut recap = Recap::default();
        let id = WorkspaceId::new();
        recap.select(Some(id));
        recap.observe(&UiEvent::JournalCapability(false), &[], &HashMap::new());
        assert!(
            recap
                .lines(id)
                .iter()
                .any(|line| line == "History unavailable on this daemon")
        );
        recap.observe(&UiEvent::Disconnected, &[], &HashMap::new());
        assert!(recap.lines(id)[0].contains("stale"));
    }

    #[test]
    fn each_absence_keeps_only_its_observed_changes() {
        let mut recap = Recap::default();
        let a = WorkspaceId::new();
        let b = WorkspaceId::new();
        recap.select(Some(a));
        assert_eq!(recap.lines(a), ["First visit this session"]);
        recap.record(a, None, || "foreground".into());
        recap.select(Some(b));
        recap.record(a, None, || "earlier".into());
        recap.focus(false);
        recap.record(b, None, || "background".into());
        recap.select(Some(a));
        recap.focus(true);
        assert_eq!(recap.lines(a), ["earlier"]);
        recap.select(Some(b));
        assert_eq!(recap.lines(b), ["background"]);
        recap.select(Some(a));
        assert_eq!(recap.lines(a), ["No changes observed while away"]);
    }

    #[test]
    fn a_disconnect_during_absence_survives_reconnection() {
        let mut recap = Recap::default();
        let id = WorkspaceId::new();
        recap.select(Some(id));
        recap.focus(false);
        recap.observe(&UiEvent::Disconnected, &[], &HashMap::new());
        assert!(recap.lines(id)[0].contains("stale"));
        recap.observe(&UiEvent::Connected, &[], &HashMap::new());
        recap.focus(true);
        assert_eq!(recap.lines(id), ["Update gap · partial history"]);
    }
    #[test]
    fn activity_replay_prefers_verified_latest_agent_text_and_keeps_navigation_targets() {
        use daedalus_proto::{ActivityCursor, ActivityEntry, ActivityKind, ActivityPage, PaneRole};
        let (mut state, _) = crate::renderer::tests::make_state_with_agent_row();
        let ws = state.active_workspace_id.unwrap();
        let agent = state.focused_pane_id().unwrap();
        let snapshot = &state.panes[&agent];
        let output = |seq| ActivityKind::AgentOutput {
            pane_id: agent,
            abs_row: snapshot.viewport_rows[0].abs_row,
            pane_seq: seq,
            history_generation: snapshot.history_generation,
        };
        let mut page = ActivityPage {
            workspace_id: ws,
            cursor: ActivityCursor { epoch: ws, seq: 5 },
            gap: false,
            entries: [
                output(0),
                ActivityKind::Git,
                ActivityKind::Git,
                ActivityKind::Exit {
                    pane_id: agent,
                    role: PaneRole::Agent,
                    exit_code: Some(2),
                },
                output(snapshot.last_seq),
            ]
            .into_iter()
            .enumerate()
            .map(|(idx, kind)| ActivityEntry {
                seq: idx as u64 + 1,
                at_ms: idx as u64,
                kind,
            })
            .collect(),
        };
        state.recap.select(Some(ws));
        state.recap.journal(&page, false, true, &state.panes);
        let entries = state.recap.entries(ws);
        assert_eq!(entries[0].label, "Update gap · partial history");
        assert_eq!(entries[1].label, "Agent terminal · abc");
        assert_eq!(entries[1].pane, Some(agent));
        assert!(entries[2].label.contains("exited · code 2"));
        assert_eq!(entries[2].pane, Some(agent));
        assert_eq!(entries[3].label, "Git status changed");
        assert_eq!(entries.len(), 4);
        state.recap.foreground_activity(ws, 2);
        assert_eq!(
            state.recap.lines(ws).last().unwrap(),
            "New activity · 2 updates"
        );
        page.entries.clear();
        state.recap.focus(false);
        let background = state.recap.lines(ws);
        state.recap.journal(&page, false, false, &state.panes);
        assert_eq!(state.recap.lines(ws), background);
        state.recap.focus(true);
        state.recap.journal(&page, true, false, &state.panes);
        assert_eq!(state.recap.lines(ws)[0], "First visit");
        state.recap.journal(&page, false, false, &state.panes);
        assert_eq!(state.recap.lines(ws)[0], "No changes observed while away");
    }

    #[test]
    fn runtime_changes_distinguish_container_health_removal_and_unchanged_status() {
        use daedalus_proto::{
            ContainerHealth, ExternalStatus, PortMapping, PortSource, StatusLevel,
        };
        let (mut state, _) = crate::renderer::tests::make_state_with_agent_row();
        let id = state.active_workspace_id.unwrap();
        let mut recap = Recap::default();
        recap.select(Some(id));
        recap.focus(false);
        let container = ContainerHealth {
            name: "db_1".into(),
            service: Some("db".into()),
            state: "running".into(),
            health: Some("healthy".into()),
            ports: vec![5432],
        };
        let runtime = |containers| UiEvent::WorkspaceRuntimeUpdated {
            workspace_id: id,
            containers,
            git_status: None,
            git_branch: Some("main".into()),
            bound_ports: vec![PortMapping {
                host_port: 5432,
                container_port: None,
                service: None,
                source: PortSource::Docker,
            }],
        };
        recap.observe(
            &runtime(vec![container.clone()]),
            &state.workspaces,
            &state.panes,
        );
        assert_eq!(
            recap.lines(id),
            [
                "db reported healthy",
                "Ports changed · 1 bound",
                "Git status changed"
            ]
        );
        state.workspaces[0].containers.push(container);
        recap.observe(&runtime(vec![]), &state.workspaces, &state.panes);
        assert_eq!(recap.lines(id)[0], "db_1 left inventory");

        let status = ExternalStatus {
            source: "CI".into(),
            level: StatusLevel::Ok,
            label: "passed".into(),
            detail: None,
            updated_at_ms: 2,
        };
        let event = |status| {
            UiEvent::ServerEvent(ServerEvent::ExternalStatusUpdated {
                workspace_id: id,
                status,
            })
        };
        recap.observe(&event(status.clone()), &state.workspaces, &state.panes);
        assert_eq!(recap.lines(id)[0], "CI reported passed");
        state.workspaces[0].external_statuses.push(status.clone());
        let before = recap.lines(id);
        for repeated in [
            status.clone(),
            ExternalStatus {
                label: "different".into(),
                updated_at_ms: 1,
                ..status.clone()
            },
        ] {
            recap.observe(&event(repeated), &state.workspaces, &state.panes);
            assert_eq!(recap.lines(id), before);
        }
        recap.observe(
            &event(ExternalStatus {
                label: String::new(),
                updated_at_ms: 3,
                ..status
            }),
            &state.workspaces,
            &state.panes,
        );
        assert_eq!(recap.lines(id)[0], "CI cleared status");
    }

    #[test]
    fn away_pane_events_retain_only_recent_changes_and_point_back_to_the_pane() {
        let (state, _) = crate::renderer::tests::make_state_with_agent_row();
        let workspace_id = state.active_workspace_id.unwrap();
        let pane_id = state.focused_pane_id().unwrap();
        let mut recap = Recap::default();
        recap.select(Some(workspace_id));
        recap.focus(false);
        for (agent, label) in [
            (AgentState::Idle, "Agent became idle"),
            (AgentState::Thinking, "Agent started thinking"),
            (AgentState::Error, "Agent reported an error"),
            (
                AgentState::WaitingForApproval,
                "Check agent · approval signal",
            ),
        ] {
            recap.observe(
                &UiEvent::AgentStateChanged {
                    workspace_id,
                    pane_id,
                    state: agent,
                },
                &state.workspaces,
                &state.panes,
            );
            let latest = &recap.entries(workspace_id)[0];
            assert_eq!(latest.label, label);
            assert_eq!(latest.pane, Some(pane_id));
        }
        for (event, suffix) in [
            (
                ServerEvent::CommandFinished {
                    pane_id,
                    command: None,
                    exit_code: 2,
                    has_failure_trace: false,
                },
                "command · exit 2",
            ),
            (
                ServerEvent::PaneExited {
                    pane_id,
                    exit_code: Some(1),
                },
                "exited · code 1",
            ),
            (
                ServerEvent::PaneExited {
                    pane_id,
                    exit_code: None,
                },
                "exited",
            ),
        ] {
            recap.observe(
                &UiEvent::ServerEvent(event),
                &state.workspaces,
                &state.panes,
            );
            let entries = recap.entries(workspace_id);
            assert!(entries[0].label.ends_with(suffix));
            assert!(entries.iter().all(|entry| entry.pane == Some(pane_id)));
            assert_eq!(entries.len(), MAX_EVENTS);
        }
        recap.observe(
            &UiEvent::ServerEvent(ServerEvent::ResyncRequired { pane_id }),
            &state.workspaces,
            &state.panes,
        );
        assert_eq!(recap.lines(workspace_id)[0], "Update gap · partial history");
        recap.observe(
            &UiEvent::WorkspacesLoaded(vec![]),
            &state.workspaces,
            &state.panes,
        );
        assert_eq!(recap.lines(workspace_id), ["First visit this session"]);
    }
}
