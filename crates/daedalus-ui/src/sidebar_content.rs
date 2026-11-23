use crate::layout::{
    LayoutMetrics, Rect, SIDEBAR_ROW_LOGICAL_HEIGHT, SidebarList, sidebar_new_workspace_rect,
    sidebar_new_worktree_rect,
};
use daedalus_proto::{StatusLevel, WorkspaceInfo};
use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub enum SidebarRow {
    Workspace(usize),
    Heading(String),
    Detail(String, Option<StatusLevel>),
}

impl SidebarRow {
    fn height(&self, scale: f32) -> f32 {
        (match self {
            Self::Workspace(_) => SIDEBAR_ROW_LOGICAL_HEIGHT,
            Self::Heading(_) => 20.,
            Self::Detail(..) => 16.,
        }) * scale
    }
}

/// Descriptive rows shared by the sidebar and the accessible workspace description.
pub fn workspace_details(ws: &WorkspaceInfo) -> Vec<SidebarRow> {
    let mut rows = Vec::new();
    for tools in ws.project.tools.chunks(2) {
        rows.push(SidebarRow::Detail(tools.join(" · "), None));
    }
    if let Some(manifest) = &ws.manifest {
        for widget in &manifest.sidebar_widgets {
            rows.push(SidebarRow::Heading(widget.title.clone()));
            for key in &widget.statuses {
                let source = format!("{}.{key}", widget.provider);
                let value = ws.external_statuses.iter().find(|s| s.source == source);
                let label = value.map_or("No report", |s| &s.label);
                rows.push(SidebarRow::Detail(
                    format!("{key} · {label}"),
                    value.map(|s| s.level),
                ));
            }
        }
    }
    rows
}

/// The same rows and physical rectangles drive keyboard paging, pointer selection and painting.
#[derive(Clone, Debug, PartialEq)]
pub struct SidebarContent {
    pub rows: Vec<SidebarRow>,
    pub visible: Range<usize>,
    pub rects: Vec<Rect>,
    pub arrows: Option<(Rect, Rect)>,
    pub max_first: usize,
    pub new_workspace: Rect,
    pub new_worktree: Rect,
}

impl SidebarContent {
    pub fn for_workspaces(
        workspaces: &[WorkspaceInfo],
        sidebar: Rect,
        first: usize,
        scale: f32,
    ) -> Self {
        // Preserve first-seen project order and workspace order within each project.
        let mut groups: Vec<(Option<&Path>, Vec<usize>)> = Vec::new();
        let mut by_repo: HashMap<Option<&Path>, usize> = HashMap::new();
        for (i, ws) in workspaces.iter().enumerate() {
            let repo = ws.project.repository.as_deref();
            if let Some(group) = by_repo.get(&repo) {
                groups[*group].1.push(i);
            } else {
                by_repo.insert(repo, groups.len());
                groups.push((repo, vec![i]));
            }
        }
        let mut names = HashMap::new();
        for path in by_repo.keys().flatten() {
            *names.entry(path.file_name()).or_insert(0) += 1;
        }
        let mut rows = Vec::new();
        for (repo, indices) in groups {
            if let Some(repo) = repo {
                let name = match repo.file_name() {
                    Some(name) if names[&Some(name)] == 1 => name.to_string_lossy(),
                    _ => repo.to_string_lossy(),
                };
                rows.push(SidebarRow::Heading(name.into_owned()));
            } else if !names.is_empty() {
                rows.push(SidebarRow::Heading("Other workspaces".into()));
            }
            for index in indices {
                let ws = &workspaces[index];
                rows.push(SidebarRow::Workspace(index));
                rows.extend(workspace_details(ws));
            }
        }
        Self::layout(rows, sidebar, first, scale)
    }

    pub fn plain(count: usize, sidebar: Rect, first: usize, scale: f32) -> Self {
        Self::layout(
            (0..count).map(SidebarRow::Workspace).collect(),
            sidebar,
            first,
            scale,
        )
    }

    fn layout(rows: Vec<SidebarRow>, sidebar: Rect, first: usize, scale: f32) -> Self {
        let m = LayoutMetrics::for_scale(scale);
        let scale = m.scale_factor;
        let top = sidebar.y + m.sidebar_header_height + m.sidebar_pad;
        let limit = sidebar.bottom() - m.sidebar_pad - m.sidebar_new_btn_height - m.sidebar_row_gap;
        let available = (limit - top).max(0.);
        let mut last_page_height = 0.;
        let mut max_first = rows.len();
        for row in rows.iter().rev() {
            let step = row.height(scale) + m.sidebar_row_gap;
            if last_page_height > 0. && last_page_height + step > available {
                break;
            }
            last_page_height += step;
            max_first -= 1;
        }
        let first = first.min(max_first);
        let mut y = top;
        let mut rects = Vec::new();
        for row in &rows[first..] {
            let h = row.height(scale);
            if y + h + m.sidebar_row_gap > limit
                && (!rects.is_empty() || available < m.header_height.min(h))
            {
                break;
            }
            let rect = Rect::new(
                sidebar.x + m.sidebar_pad,
                y,
                (sidebar.width - 2. * m.sidebar_pad).max(0.),
                h.min(limit - y),
            );
            rects.push(rect);
            y = rect.bottom() + m.sidebar_row_gap;
        }
        let visible = first..first + rects.len();
        let arrows = (first > 0 || visible.end < rows.len())
            .then(|| SidebarList::new(sidebar, usize::MAX, 0, scale).arrows)
            .flatten();
        let mut new_workspace = sidebar_new_workspace_rect(sidebar, rows.len(), scale);
        let mut new_worktree = sidebar_new_worktree_rect(sidebar, rows.len(), scale);
        if visible.end == rows.len() && first == 0 {
            new_workspace.y = new_workspace.y.min(y + m.sidebar_row_gap);
            new_worktree.y = new_workspace.y;
        }
        Self {
            rows,
            visible,
            rects,
            arrows,
            max_first,
            new_workspace,
            new_worktree,
        }
    }
}
