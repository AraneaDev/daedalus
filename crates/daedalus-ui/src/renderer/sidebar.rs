use daedalus_proto::{AgentState, PaneRole, PaneStatus};

use super::{
    agent_state_badge, external_status_color, format_docker_health, format_external_source_tag,
    sidebar_agent_state_pill_rect, sidebar_branch_badge_rect_with_reserve,
    sidebar_docker_pill_rect, sidebar_external_badge_rect, status_dot_color,
    visible_external_statuses, workspace_branch_badge,
};
use crate::layout::{Rect, sidebar_failed_run_rect};
use crate::sidebar_content::{SidebarContent, SidebarRow};
use crate::state::{AppState, WorkspaceView};
use crate::theme::{Rgba, Theme};
use std::sync::Arc;

pub(super) struct SidebarCards {
    pub list: Arc<SidebarContent>,
    pub indices: Vec<usize>,
    pub cards: Vec<Card>,
}

impl SidebarCards {
    pub fn new(
        state: &AppState,
        sidebar: Rect,
        scale: f32,
        theme: &Theme,
        prepared: Option<&Arc<SidebarContent>>,
    ) -> Self {
        let list = prepared.cloned().unwrap_or_else(|| {
            Arc::new(SidebarContent::for_workspaces(
                &state.workspaces,
                sidebar,
                state.sidebar_first,
                scale,
            ))
        });
        let mut cards = Vec::new();
        let mut indices = Vec::new();
        for (row, rect) in list.rows[list.visible.clone()].iter().zip(&list.rects) {
            if let SidebarRow::Workspace(index) = row {
                let ws = &state.workspaces[*index];
                indices.push(*index);
                cards.push(Card::new(ws, *rect, state, scale, theme));
            }
        }
        Self {
            list,
            indices,
            cards,
        }
    }
}

pub(super) struct Card {
    pub rect: Rect,
    pub dot: Rgba,
    pub needs_reply: bool,
    pub full: bool,
    pub title_width: f32,
    pub subtitle_width: Option<f32>,
    pub ports_width: f32,
    pub badges: Vec<Badge>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum BadgeKind {
    FailedRun,
    Runner,
    Agent,
    Branch,
    Docker,
    External,
}

pub(super) struct Badge {
    pub kind: BadgeKind,
    pub rect: Rect,
    pub label: String,
    pub color: Rgba,
    pub border: Rgba,
    pub bold: bool,
}

impl Card {
    fn new(ws: &WorkspaceView, rect: Rect, state: &AppState, s: f32, theme: &Theme) -> Self {
        let agent = state.workspace_agent_state(ws);
        let panes = |role| state.workspace_panes(ws).rev().find(|p| p.role == role);
        let dot = match agent {
            Some(AgentState::WaitingForApproval) => theme.status_busy,
            Some(AgentState::Error) => theme.status_err,
            _ => status_dot_color(
                panes(PaneRole::Agent).map_or(PaneStatus::Exited { code: None }, |p| p.status),
                theme,
            ),
        };
        let left = rect.x + 22.0 * s;
        let mut card = Self {
            rect,
            dot,
            needs_reply: agent == Some(AgentState::WaitingForApproval),
            full: rect.height >= crate::sidebar_content::workspace_card_height(ws) * s,
            title_width: (rect.right() - left - 24.0 * s).max(1.0),
            subtitle_width: None,
            ports_width: (rect.width - 18.0 * s).max(1.0),
            badges: Vec::new(),
        };
        let runner_badge = if state.failed_runner(ws).is_some() {
            Some((
                BadgeKind::FailedRun,
                "Failed →",
                theme.bg_pane,
                sidebar_failed_run_rect(rect, s),
            ))
        } else {
            panes(PaneRole::Runner)
                .filter(|p| match p.status {
                    PaneStatus::Exited { code: Some(code) } => code == 0,
                    _ => p.last_exit_code == Some(0),
                })
                .map(|_| {
                    (
                        BadgeKind::Runner,
                        "Passed",
                        theme.status_ok,
                        Rect::new(
                            rect.right() - 70.0 * s,
                            rect.y + 5.0 * s,
                            46.0 * s,
                            18.0 * s,
                        ),
                    )
                })
        };
        if let Some((kind, label, color, bounds)) = runner_badge {
            card.title_width = (bounds.x - left - 4.0 * s).max(1.0);
            card.badges.push(Badge {
                kind,
                label: label.into(),
                color,
                bold: true,
                border: color.with_alpha(0.65),
                rect: bounds,
            });
        }
        if !card.full {
            return card;
        }
        card.add_identity_badges(ws, agent, s, theme);
        if let Some((label, healthy)) = format_docker_health(&ws.containers) {
            let color = if healthy {
                theme.status_ok
            } else {
                theme.status_err
            };
            card.ports_width = (rect.width - 82.0 * s).max(1.0);
            card.badges.push(Badge {
                kind: BadgeKind::Docker,
                label,
                color,
                bold: true,
                border: color.with_alpha(0.65),
                rect: sidebar_docker_pill_rect(rect, s),
            });
        }
        let statuses = visible_external_statuses(ws);
        for (i, ext) in statuses.iter().enumerate() {
            let color = external_status_color(ext.level, theme);
            card.badges.push(Badge {
                kind: BadgeKind::External,
                label: format!("{} {}", format_external_source_tag(&ext.source), ext.label),
                color,
                bold: true,
                border: color.with_alpha(0.65),
                rect: sidebar_external_badge_rect(rect, i, statuses.len(), s),
            });
        }
        card
    }

    fn add_identity_badges(
        &mut self,
        ws: &WorkspaceView,
        agent: Option<AgentState>,
        s: f32,
        theme: &Theme,
    ) {
        let mut reserve = 0.0;
        if let Some(agent) = agent {
            let (label, color) = if self.needs_reply {
                ("Needs reply", theme.status_busy)
            } else {
                agent_state_badge(agent, theme)
            };
            let mut rect = sidebar_agent_state_pill_rect(self.rect, label, s);
            if ws.containers.is_empty() {
                rect.y = self.rect.y + 62.0 * s;
                self.ports_width = (rect.x - self.rect.x - 14.0 * s).max(1.0);
            } else {
                reserve = rect.width + 12.0 * s;
            }
            let alpha = if agent == AgentState::WaitingForApproval {
                0.90
            } else {
                0.60
            };
            self.badges.push(Badge {
                kind: BadgeKind::Agent,
                label: label.into(),
                color,
                bold: true,
                border: color.with_alpha(alpha),
                rect,
            });
        }
        if let Some((label, worktree)) = workspace_branch_badge(ws) {
            let rect = sidebar_branch_badge_rect_with_reserve(self.rect, &label, reserve, s);
            let columns = ((rect.width - 10.0 * s) / (7.0 * s)).max(1.0) as usize;
            let label = super::text::truncate_left_with_ellipsis(&label, columns);
            let (color, border) = if worktree {
                (theme.border_focused, theme.border_focused.with_alpha(0.65))
            } else {
                (theme.text_muted, theme.border_unfocused)
            };
            self.badges.push(Badge {
                kind: BadgeKind::Branch,
                label,
                color,
                bold: worktree,
                border,
                rect,
            });
        } else {
            self.subtitle_width = Some((self.rect.width - 30.0 * s - reserve).max(1.0));
        }
    }
}
