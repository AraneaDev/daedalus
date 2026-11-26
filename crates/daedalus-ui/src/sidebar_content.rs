use crate::layout::{
    LayoutMetrics, Rect, SIDEBAR_ROW_LOGICAL_HEIGHT, SidebarList, sidebar_new_workspace_rect,
    sidebar_new_worktree_rect,
};
use crate::state::WorkspaceView;
use daedalus_proto::StatusLevel;
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
    fn height(&self, workspaces: &[WorkspaceView], scale: f32) -> f32 {
        (match self {
            Self::Workspace(i) => workspaces
                .get(*i)
                .map_or(SIDEBAR_ROW_LOGICAL_HEIGHT, workspace_card_height),
            Self::Heading(_) => 20.,
            Self::Detail(..) => 16.,
        }) * scale
    }
}

pub(crate) fn workspace_card_height(ws: &WorkspaceView) -> f32 {
    if ws.external_statuses.is_empty() {
        90.0
    } else {
        134.0
    }
}

/// Descriptive rows shared by the sidebar and the accessible workspace description.
pub fn workspace_details(ws: &WorkspaceView) -> Vec<SidebarRow> {
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

/// First-seen repository order, with daemon order preserved inside each group.
pub(crate) fn repository_groups(workspaces: &[WorkspaceView]) -> Vec<(Option<&Path>, Vec<usize>)> {
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
    groups
}

/// Parent prefixes are removed in order, yielding the basename first.
fn suffixes(path: &Path) -> impl Iterator<Item = &Path> {
    path.ancestors()
        .skip(1)
        .filter_map(move |ancestor| path.strip_prefix(ancestor).ok())
}

fn repository_label(path: &Path, suffix_counts: &HashMap<&Path, usize>) -> String {
    let suffix = suffixes(path)
        .find(|suffix| suffix_counts.get(suffix) == Some(&1))
        .unwrap_or(path);
    match suffix.to_str() {
        Some(text) if !text.starts_with('"') && !text.chars().any(char::is_control) => text.into(),
        // The quoted OS representation keeps non-Unicode and control bytes distinguishable.
        _ => format!("{:?}", suffix.as_os_str()),
    }
}

impl SidebarContent {
    pub fn for_workspaces(
        workspaces: &[WorkspaceView],
        sidebar: Rect,
        first: usize,
        scale: f32,
    ) -> Self {
        let groups = repository_groups(workspaces);
        let mut suffix_counts = HashMap::new();
        for path in groups.iter().filter_map(|(path, _)| *path) {
            for suffix in suffixes(path) {
                *suffix_counts.entry(suffix).or_insert(0) += 1;
            }
        }
        let has_repository = groups.iter().any(|(repo, _)| repo.is_some());
        let mut rows = Vec::new();
        for (repo, indices) in groups {
            if let Some(repo) = repo {
                let label = repository_label(repo, &suffix_counts);
                if workspaces.len() > 1 || workspaces[indices[0]].name != label {
                    rows.push(SidebarRow::Heading(label));
                }
            } else if has_repository {
                rows.push(SidebarRow::Heading("Other workspaces".into()));
            }
            for index in indices {
                let ws = &workspaces[index];
                rows.push(SidebarRow::Workspace(index));
                rows.extend(workspace_details(ws));
            }
        }
        Self::layout(rows, workspaces, sidebar, first, scale)
    }

    pub fn plain(count: usize, sidebar: Rect, first: usize, scale: f32) -> Self {
        Self::layout(
            (0..count).map(SidebarRow::Workspace).collect(),
            &[],
            sidebar,
            first,
            scale,
        )
    }

    fn layout(
        rows: Vec<SidebarRow>,
        workspaces: &[WorkspaceView],
        sidebar: Rect,
        first: usize,
        scale: f32,
    ) -> Self {
        let m = LayoutMetrics::for_scale(scale);
        let scale = m.scale_factor;
        let top = sidebar.y + m.sidebar_header_height + m.sidebar_pad;
        let limit = sidebar.bottom() - m.sidebar_pad - m.sidebar_new_btn_height - m.sidebar_row_gap;
        let available = (limit - top).max(0.);
        let mut last_page_height = 0.;
        let mut max_first = rows.len();
        for row in rows.iter().rev() {
            let step = row.height(workspaces, scale) + m.sidebar_row_gap;
            if last_page_height > 0. && last_page_height + step > available {
                break;
            }
            last_page_height += step;
            max_first -= 1;
        }
        let first = first.min(max_first);
        let mut y = top;
        let mut rects = Vec::new();
        let mut has_workspace = false;
        for row in &rows[first..] {
            let h = row.height(workspaces, scale);
            let is_workspace = matches!(row, SidebarRow::Workspace(_));
            let may_condense = rects.is_empty() || (is_workspace && !has_workspace);
            if y + h + m.sidebar_row_gap > limit
                && (!may_condense || limit - y < m.header_height.min(h))
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
            has_workspace |= is_workspace;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_grouped_sidebar_keeps_a_workspace_card_below_its_heading() {
        for scale in [1., 2.] {
            let sidebar = Rect::new(0., 0., 240. * scale, 180. * scale);
            let list = SidebarContent::layout(
                vec![
                    SidebarRow::Heading("Repository".into()),
                    SidebarRow::Workspace(0),
                ],
                &[],
                sidebar,
                0,
                scale,
            );
            assert_eq!(list.visible, 0..2);
            let [heading, card] = list.rects.as_slice() else {
                panic!("expected a heading and a condensed workspace card");
            };
            assert!(heading.bottom() < card.y);
            assert!(card.height >= crate::layout::HEADER_LOGICAL_HEIGHT * scale);
            assert!(card.height < SIDEBAR_ROW_LOGICAL_HEIGHT * scale);
            assert!(card.bottom() < list.new_workspace.y);
        }
    }

    fn labels(paths: &[&Path]) -> Vec<String> {
        let mut counts = HashMap::new();
        for path in paths {
            for suffix in suffixes(path) {
                *counts.entry(suffix).or_insert(0) += 1;
            }
        }
        paths
            .iter()
            .map(|path| repository_label(path, &counts))
            .collect()
    }

    #[test]
    fn headings_use_the_shortest_distinguishing_suffix_even_for_nested_names() {
        let first = Path::new("/a/long/common/prefix/one/api");
        let second = Path::new("/a/long/common/prefix/two/api");
        let nested = Path::new("/one/api");
        let single = Path::new("/a/long/common/prefix/cli");
        let displayed = labels(&[first, second, nested, single]);
        assert_eq!(Path::new(&displayed[0]), Path::new("prefix/one/api"));
        assert_eq!(Path::new(&displayed[1]), Path::new("two/api"));
        assert_eq!(Path::new(&displayed[2]), nested);
        assert_eq!(displayed[3], "cli");
    }

    #[cfg(unix)]
    #[test]
    fn headings_preserve_distinct_non_unicode_names() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
        let a = Path::new(OsStr::from_bytes(b"/src/alpha-\xfe"));
        let b = Path::new(OsStr::from_bytes(b"/src/alpha-\xff"));
        let displayed = labels(&[a, b]);
        assert_ne!(displayed[0], displayed[1]);
        assert!(
            displayed
                .iter()
                .all(|s| s.contains("alpha-") && !s.contains('�'))
        );
    }
}
