use daedalus_proto::{AgentState, PaneRole, PaneStatus, WorkspaceInfo};

use super::{
    agent_state_badge, external_status_color, format_docker_health, format_external_source_tag,
    sidebar_agent_state_pill_rect, sidebar_branch_badge_rect_with_reserve,
    sidebar_docker_pill_rect, sidebar_external_badge_rect, status_dot_color,
    visible_external_statuses, workspace_branch_badge,
};
use crate::layout::{Rect, SIDEBAR_ROW_LOGICAL_HEIGHT, SidebarList, sidebar_workspace_rect};
use crate::state::AppState;
use crate::theme::{Rgba, Theme};

pub(super) struct SidebarCards {
    pub list: SidebarList,
    pub cards: Vec<Card>,
}

impl SidebarCards {
    pub fn new(state: &AppState, sidebar: Rect, scale: f32, theme: &Theme) -> Self {
        let list = SidebarList::new(sidebar, state.workspaces.len(), state.sidebar_first, scale);
        let cards = list
            .visible
            .clone()
            .enumerate()
            .map(|(row, index)| {
                let rect = sidebar_workspace_rect(sidebar, row, scale);
                let ws = &state.workspaces[index];
                Card::new(ws, rect, state.workspace_agent_state(ws), scale, theme)
            })
            .collect();
        Self { list, cards }
    }
}

pub(super) struct Card {
    pub rect: Rect,
    pub dot: Rgba,
    pub full: bool,
    pub title_width: f32,
    pub subtitle_width: Option<f32>,
    pub ports_width: f32,
    pub badges: Vec<Badge>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum BadgeKind {
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
    fn new(
        ws: &WorkspaceInfo,
        rect: Rect,
        agent: Option<AgentState>,
        s: f32,
        theme: &Theme,
    ) -> Self {
        let panes = |role| ws.panes.iter().rev().find(|p| p.role == role);
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
            full: rect.height >= SIDEBAR_ROW_LOGICAL_HEIGHT * s,
            title_width: (rect.right() - left - 24.0 * s).max(1.0),
            subtitle_width: None,
            ports_width: (rect.width - 18.0 * s).max(1.0),
            badges: Vec::new(),
        };
        if let Some((label, color)) = panes(PaneRole::Runner).and_then(|p| {
            if p.has_failure_trace {
                Some((
                    format!("✗ {}", p.last_exit_code.unwrap_or(1)),
                    theme.status_err,
                ))
            } else if p.last_exit_code == Some(0) {
                Some(("✓ 0".into(), theme.status_ok))
            } else {
                None
            }
        }) {
            card.title_width = (rect.right() - left - 74.0 * s).max(1.0);
            card.badges.push(Badge {
                kind: BadgeKind::Runner,
                label,
                color,
                bold: true,
                border: color.with_alpha(0.65),
                rect: Rect::new(
                    rect.right() - 70.0 * s,
                    rect.y + 5.0 * s,
                    46.0 * s,
                    18.0 * s,
                ),
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
        ws: &WorkspaceInfo,
        agent: Option<AgentState>,
        s: f32,
        theme: &Theme,
    ) {
        let mut reserve = 0.0;
        if let Some(agent) = agent {
            let (label, color) = agent_state_badge(agent, theme);
            let rect = sidebar_agent_state_pill_rect(self.rect, label, s);
            reserve = rect.width + 6.0 * s;
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
