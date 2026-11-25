use super::{MAX_EVENTS, RecapEntry};
use crate::state::PaneViewState;
use daedalus_proto::{ActivityEntry, ActivityKind, ActivityPage, GridRow, PaneId, PaneRole};
use std::collections::HashMap;

pub(super) fn excerpt(row: &GridRow) -> Option<String> {
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

pub(super) fn rows(
    page: &ActivityPage,
    first: bool,
    gap: bool,
    panes: &HashMap<PaneId, PaneViewState>,
) -> Vec<RecapEntry> {
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
    lines
}
