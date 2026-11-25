use super::{Recap, RecapEntry, replay::excerpt};
use crate::bridge::UiEvent;
use crate::state::{PaneViewState, WorkspaceView};
use daedalus_proto::{AgentState, PaneId, PaneRole, ServerEvent};
use std::collections::HashMap;

fn short_label(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_control())
        .take(80)
        .collect()
}

impl Recap {
    pub(super) fn observe_live(
        &mut self,
        event: &UiEvent,
        workspaces: &[WorkspaceView],
        panes: &HashMap<PaneId, PaneViewState>,
    ) {
        let workspace = |id| workspaces.iter().find(|ws| ws.id == id);
        let pane = |id: PaneId| panes.get(&id).map(|view| &view.summary);
        match event {
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
}
