use daedalus_proto::PaneRole;

pub const SIDEBAR_LOGICAL_WIDTH: f32 = 240.0;
pub const HEADER_LOGICAL_HEIGHT: f32 = 26.0;
pub const BANNER_LOGICAL_HEIGHT: f32 = 24.0;
pub const VIEWPORT_LOGICAL_PAD: f32 = 6.0;
pub const AGENT_WIDTH_RATIO: f32 = 0.55;

pub const SIDEBAR_HEADER_LOGICAL_HEIGHT: f32 = 32.0;
pub const SIDEBAR_ROW_LOGICAL_HEIGHT: f32 = 84.0;
pub const SIDEBAR_ROW_LOGICAL_GAP: f32 = 4.0;
pub const SIDEBAR_PAD_LOGICAL: f32 = 8.0;
pub const SIDEBAR_NEW_BTN_LOGICAL_HEIGHT: f32 = 34.0;

const AGENT_PILL_LOGICAL_WIDTH: f32 = 116.0;
const RUNNER_PILL_LOGICAL_WIDTH: f32 = 176.0;
const PILL_MARGIN_X_LOGICAL: f32 = 6.0;
const PILL_MARGIN_Y_LOGICAL: f32 = 3.0;

/// Axis-aligned rectangle in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn right(&self) -> f32 {
        self.x + self.width
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.height
    }

    pub fn area(&self) -> f32 {
        self.width.max(0.0) * self.height.max(0.0)
    }

    pub fn contains(&self, px: f32, py: f32) -> bool {
        self.width > 0.0
            && self.height > 0.0
            && px >= self.x
            && px < self.x + self.width
            && py >= self.y
            && py < self.y + self.height
    }
}

/// Physical-pixel layout metrics derived from a window's HiDPI `scale_factor`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayoutMetrics {
    pub scale_factor: f32,
    pub sidebar_width: f32,
    pub header_height: f32,
    pub banner_height: f32,
    pub viewport_pad: f32,
    pub sidebar_header_height: f32,
    pub sidebar_row_height: f32,
    pub sidebar_row_gap: f32,
    pub sidebar_pad: f32,
    pub sidebar_new_btn_height: f32,
}

impl LayoutMetrics {
    pub fn for_scale(scale_factor: f32) -> Self {
        let s = if scale_factor.is_finite() && scale_factor > 0.0 {
            scale_factor
        } else {
            1.0
        };
        Self {
            scale_factor: s,
            sidebar_width: SIDEBAR_LOGICAL_WIDTH * s,
            header_height: HEADER_LOGICAL_HEIGHT * s,
            banner_height: BANNER_LOGICAL_HEIGHT * s,
            viewport_pad: VIEWPORT_LOGICAL_PAD * s,
            sidebar_header_height: SIDEBAR_HEADER_LOGICAL_HEIGHT * s,
            sidebar_row_height: SIDEBAR_ROW_LOGICAL_HEIGHT * s,
            sidebar_row_gap: SIDEBAR_ROW_LOGICAL_GAP * s,
            sidebar_pad: SIDEBAR_PAD_LOGICAL * s,
            sidebar_new_btn_height: SIDEBAR_NEW_BTN_LOGICAL_HEIGHT * s,
        }
    }
}

/// Geometry and terminal cell dimensions for a single semantic pane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaneRect {
    pub role: PaneRole,
    pub outer: Rect,
    pub header: Rect,
    pub viewport: Rect,
    pub action_pill: Option<Rect>,
    pub cols: u16,
    pub rows: u16,
}

/// Inputs required to solve the workspace layout for a single frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayoutInput {
    pub window_w: f32,
    pub window_h: f32,
    pub scale_factor: f32,
    pub cell_w: f32,
    pub cell_h: f32,
    pub sidebar_visible: bool,
    pub has_top_banner: bool,
    pub zoomed: bool,
    pub focused_role: PaneRole,
    pub runner_has_failure: bool,
}

/// Computed rectangles for the sidebar, optional top banner, and visible semantic panes.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceLayout {
    pub sidebar: Option<Rect>,
    pub banner: Option<Rect>,
    pub panes: Vec<PaneRect>,
}

impl WorkspaceLayout {
    pub fn pane(&self, role: PaneRole) -> Option<&PaneRect> {
        self.panes.iter().find(|p| p.role == role)
    }
}

/// Result of hit-testing a physical pixel coordinate against a `WorkspaceLayout`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitTarget {
    SidebarWorkspace(usize),
    SidebarNewWorkspace,
    SidebarNewWorktree,
    PaneHeaderAction(PaneRole),
    PaneViewport { role: PaneRole, col: u16, row: u16 },
    PaneHeader(PaneRole),
    None,
}

/// Computes terminal `(cols, rows)` fitting inside `viewport`, clamped to at least `(1, 1)`.
pub fn cell_grid_size(viewport: Rect, cell_w: f32, cell_h: f32) -> (u16, u16) {
    let cols = if cell_w.is_finite() && cell_w > 0.0 && viewport.width > 0.0 {
        (viewport.width / cell_w)
            .floor()
            .clamp(1.0, u16::MAX as f32) as u16
    } else {
        1
    };
    let rows = if cell_h.is_finite() && cell_h > 0.0 && viewport.height > 0.0 {
        (viewport.height / cell_h)
            .floor()
            .clamp(1.0, u16::MAX as f32) as u16
    } else {
        1
    };
    (cols, rows)
}

fn build_pane_rect(
    role: PaneRole,
    outer: Rect,
    metrics: &LayoutMetrics,
    cell_w: f32,
    cell_h: f32,
    runner_has_failure: bool,
) -> PaneRect {
    let header_h = metrics.header_height.min(outer.height.max(0.0));
    let header = Rect::new(outer.x, outer.y, outer.width.max(0.0), header_h);

    let pill_logical_w = match role {
        PaneRole::Agent => Some(AGENT_PILL_LOGICAL_WIDTH),
        PaneRole::Runner if runner_has_failure => Some(RUNNER_PILL_LOGICAL_WIDTH),
        PaneRole::Service | PaneRole::Runner => None,
    };

    let action_pill = pill_logical_w.and_then(|logical_w| {
        let margin_x = PILL_MARGIN_X_LOGICAL * metrics.scale_factor;
        let margin_y = PILL_MARGIN_Y_LOGICAL * metrics.scale_factor;
        let max_w = (header.width - 2.0 * margin_x).max(0.0);
        let pill_w = (logical_w * metrics.scale_factor).min(max_w);
        let pill_h = (header.height - 2.0 * margin_y).max(0.0);
        if pill_w > 0.0 && pill_h > 0.0 {
            Some(Rect::new(
                header.right() - margin_x - pill_w,
                header.y + margin_y,
                pill_w,
                pill_h,
            ))
        } else {
            None
        }
    });

    let body_y = outer.y + header_h;
    let body_h = (outer.height - header_h).max(0.0);
    let pad_x = metrics.viewport_pad.min(outer.width.max(0.0) * 0.5);
    let pad_y = metrics.viewport_pad.min(body_h * 0.5);
    let viewport = Rect::new(
        outer.x + pad_x,
        body_y + pad_y,
        (outer.width - 2.0 * pad_x).max(0.0),
        (body_h - 2.0 * pad_y).max(0.0),
    );

    let (cols, rows) = cell_grid_size(viewport, cell_w, cell_h);

    PaneRect {
        role,
        outer,
        header,
        viewport,
        action_pill,
        cols,
        rows,
    }
}

/// Pure geometry solver for Daedalus's fixed 3-pane semantic workspace layout.
pub fn compute_layout(input: &LayoutInput) -> WorkspaceLayout {
    let metrics = LayoutMetrics::for_scale(input.scale_factor);
    let window_w = input.window_w.max(0.0);
    let window_h = input.window_h.max(0.0);

    let (sidebar, sidebar_w) = if input.sidebar_visible {
        let w = metrics.sidebar_width.min(window_w);
        (Some(Rect::new(0.0, 0.0, w, window_h)), w)
    } else {
        (None, 0.0)
    };

    let canvas_x = sidebar_w;
    let canvas_w = (window_w - sidebar_w).max(0.0);

    let (banner, banner_h) = if input.has_top_banner {
        let h = metrics.banner_height.min(window_h);
        (Some(Rect::new(canvas_x, 0.0, canvas_w, h)), h)
    } else {
        (None, 0.0)
    };

    let canvas_y = banner_h;
    let canvas_h = (window_h - banner_h).max(0.0);

    let panes = if input.zoomed {
        let outer = Rect::new(canvas_x, canvas_y, canvas_w, canvas_h);
        vec![build_pane_rect(
            input.focused_role,
            outer,
            &metrics,
            input.cell_w,
            input.cell_h,
            input.runner_has_failure,
        )]
    } else {
        let agent_w = canvas_w * AGENT_WIDTH_RATIO;
        let right_w = (canvas_w - agent_w).max(0.0);
        let top_h = canvas_h * 0.5;
        let bot_h = (canvas_h - top_h).max(0.0);

        let agent_outer = Rect::new(canvas_x, canvas_y, agent_w, canvas_h);
        let service_outer = Rect::new(canvas_x + agent_w, canvas_y, right_w, top_h);
        let runner_outer = Rect::new(canvas_x + agent_w, canvas_y + top_h, right_w, bot_h);

        vec![
            build_pane_rect(
                PaneRole::Agent,
                agent_outer,
                &metrics,
                input.cell_w,
                input.cell_h,
                input.runner_has_failure,
            ),
            build_pane_rect(
                PaneRole::Service,
                service_outer,
                &metrics,
                input.cell_w,
                input.cell_h,
                input.runner_has_failure,
            ),
            build_pane_rect(
                PaneRole::Runner,
                runner_outer,
                &metrics,
                input.cell_w,
                input.cell_h,
                input.runner_has_failure,
            ),
        ]
    };

    WorkspaceLayout {
        sidebar,
        banner,
        panes,
    }
}

/// Computes the rectangle of workspace row `index` inside the sidebar.
pub fn sidebar_workspace_rect(sidebar: Rect, index: usize, scale_factor: f32) -> Rect {
    let m = LayoutMetrics::for_scale(scale_factor);
    let x = sidebar.x + m.sidebar_pad;
    let w = (sidebar.width - 2.0 * m.sidebar_pad).max(0.0);
    let y = sidebar.y
        + m.sidebar_header_height
        + m.sidebar_pad
        + (index as f32) * (m.sidebar_row_height + m.sidebar_row_gap);
    Rect::new(x, y, w, m.sidebar_row_height)
}

fn sidebar_footer_buttons_y(sidebar: Rect, workspace_count: usize, m: &LayoutMetrics) -> f32 {
    sidebar.y
        + m.sidebar_header_height
        + m.sidebar_pad
        + (workspace_count as f32) * (m.sidebar_row_height + m.sidebar_row_gap)
        + m.sidebar_row_gap
}

/// Computes the rectangle of the "+ Workspace" button inside the sidebar.
pub fn sidebar_new_workspace_rect(sidebar: Rect, workspace_count: usize, scale_factor: f32) -> Rect {
    let m = LayoutMetrics::for_scale(scale_factor);
    let total_w = (sidebar.width - 2.0 * m.sidebar_pad).max(0.0);
    let btn_w = ((total_w - m.sidebar_row_gap) * 0.5).max(0.0);
    let x = sidebar.x + m.sidebar_pad;
    let y = sidebar_footer_buttons_y(sidebar, workspace_count, &m);
    Rect::new(x, y, btn_w, m.sidebar_new_btn_height)
}

/// Computes the rectangle of the "+ Worktree" button next to "+ Workspace" inside the sidebar.
pub fn sidebar_new_worktree_rect(sidebar: Rect, workspace_count: usize, scale_factor: f32) -> Rect {
    let m = LayoutMetrics::for_scale(scale_factor);
    let total_w = (sidebar.width - 2.0 * m.sidebar_pad).max(0.0);
    let left_w = ((total_w - m.sidebar_row_gap) * 0.5).max(0.0);
    let right_w = (total_w - left_w - m.sidebar_row_gap).max(0.0);
    let x = sidebar.x + m.sidebar_pad + left_w + m.sidebar_row_gap;
    let y = sidebar_footer_buttons_y(sidebar, workspace_count, &m);
    Rect::new(x, y, right_w, m.sidebar_new_btn_height)
}

/// Hit-tests a physical pixel coordinate `(x, y)` against the current `WorkspaceLayout`.
pub fn hit_test(
    layout: &WorkspaceLayout,
    workspace_count: usize,
    scale_factor: f32,
    cell_w: f32,
    cell_h: f32,
    x: f32,
    y: f32,
) -> HitTarget {
    if let Some(sidebar) = layout.sidebar
        && sidebar.contains(x, y)
    {
        for i in 0..workspace_count {
            let row_rect = sidebar_workspace_rect(sidebar, i, scale_factor);
            if row_rect.contains(x, y) {
                return HitTarget::SidebarWorkspace(i);
            }
        }
        let new_btn_rect = sidebar_new_workspace_rect(sidebar, workspace_count, scale_factor);
        if new_btn_rect.contains(x, y) {
            return HitTarget::SidebarNewWorkspace;
        }
        let new_wt_rect = sidebar_new_worktree_rect(sidebar, workspace_count, scale_factor);
        if new_wt_rect.contains(x, y) {
            return HitTarget::SidebarNewWorktree;
        }
        return HitTarget::None;
    }

    for pane in &layout.panes {
        if let Some(pill) = pane.action_pill
            && pill.contains(x, y)
        {
            return HitTarget::PaneHeaderAction(pane.role);
        }
        if pane.header.contains(x, y) {
            return HitTarget::PaneHeader(pane.role);
        }
        if pane.viewport.contains(x, y) {
            let col = if cell_w > 0.0 {
                (((x - pane.viewport.x) / cell_w).floor().max(0.0) as u16)
                    .min(pane.cols.saturating_sub(1))
            } else {
                0
            };
            let row = if cell_h > 0.0 {
                (((y - pane.viewport.y) / cell_h).floor().max(0.0) as u16)
                    .min(pane.rows.saturating_sub(1))
            } else {
                0
            };
            return HitTarget::PaneViewport {
                role: pane.role,
                col,
                row,
            };
        }
    }

    HitTarget::None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_input() -> LayoutInput {
        LayoutInput {
            window_w: 1240.0,
            window_h: 800.0,
            scale_factor: 1.0,
            cell_w: 8.0,
            cell_h: 16.0,
            sidebar_visible: true,
            has_top_banner: false,
            zoomed: false,
            focused_role: PaneRole::Agent,
            runner_has_failure: false,
        }
    }

    #[test]
    fn unzoomed_layout_splits_55_22_5_22_5_percent() {
        let layout = compute_layout(&default_input());
        let sidebar = layout.sidebar.expect("sidebar should be visible");
        assert_eq!(sidebar, Rect::new(0.0, 0.0, 240.0, 800.0));
        assert!(layout.banner.is_none());
        assert_eq!(layout.panes.len(), 3);

        // Remaining canvas is 1000 x 800
        let canvas_area = 1000.0 * 800.0;
        let agent = layout.pane(PaneRole::Agent).unwrap();
        let service = layout.pane(PaneRole::Service).unwrap();
        let runner = layout.pane(PaneRole::Runner).unwrap();

        assert!((agent.outer.area() / canvas_area - 0.55).abs() < 1e-5);
        assert!((service.outer.area() / canvas_area - 0.225).abs() < 1e-5);
        assert!((runner.outer.area() / canvas_area - 0.225).abs() < 1e-5);

        assert_eq!(agent.outer, Rect::new(240.0, 0.0, 550.0, 800.0));
        assert_eq!(service.outer, Rect::new(790.0, 0.0, 450.0, 400.0));
        assert_eq!(runner.outer, Rect::new(790.0, 400.0, 450.0, 400.0));

        // Agent always gets preset action pill; Service never; Runner only on failure
        assert_eq!(agent.action_pill.unwrap().width, 116.0);
        assert!(service.action_pill.is_none());
        assert!(runner.action_pill.is_none());
    }

    #[test]
    fn zoomed_layout_gives_focused_role_100_percent_of_canvas() {
        let mut input = default_input();
        input.zoomed = true;
        input.focused_role = PaneRole::Runner;
        input.runner_has_failure = true;

        let layout = compute_layout(&input);
        assert_eq!(layout.panes.len(), 1);
        let pane = &layout.panes[0];
        assert_eq!(pane.role, PaneRole::Runner);
        assert_eq!(pane.outer, Rect::new(240.0, 0.0, 1000.0, 800.0));
        assert!(pane.action_pill.is_some());
    }

    #[test]
    fn sidebar_toggle_and_top_banner_adjust_canvas() {
        let mut input = default_input();
        input.sidebar_visible = false;
        input.has_top_banner = true;
        input.scale_factor = 2.0;

        let layout = compute_layout(&input);
        assert!(layout.sidebar.is_none());
        let banner = layout.banner.expect("banner should exist");
        assert_eq!(banner, Rect::new(0.0, 0.0, 1240.0, 48.0));

        let agent = layout.pane(PaneRole::Agent).unwrap();
        assert_eq!(agent.outer.x, 0.0);
        assert_eq!(agent.outer.y, 48.0);
        assert_eq!(agent.outer.height, 800.0 - 48.0);
    }

    #[test]
    fn cell_grid_size_clamps_to_at_least_one_by_one() {
        assert_eq!(cell_grid_size(Rect::new(0.0, 0.0, 0.0, 0.0), 8.0, 16.0), (1, 1));
        assert_eq!(cell_grid_size(Rect::new(0.0, 0.0, 3.0, 5.0), 8.0, 16.0), (1, 1));
        assert_eq!(cell_grid_size(Rect::new(0.0, 0.0, 80.0, 160.0), 0.0, -4.0), (1, 1));
        assert_eq!(cell_grid_size(Rect::new(0.0, 0.0, 85.0, 170.0), 8.0, 16.0), (10, 10));

        let mut tiny = default_input();
        tiny.window_w = 10.0;
        tiny.window_h = 10.0;
        let layout = compute_layout(&tiny);
        for pane in &layout.panes {
            assert!(pane.cols >= 1);
            assert!(pane.rows >= 1);
        }
    }

    #[test]
    fn hit_test_identifies_cells_headers_action_pills_and_sidebar() {
        let mut input = default_input();
        input.runner_has_failure = true;
        let layout = compute_layout(&input);

        let agent = layout.pane(PaneRole::Agent).unwrap();
        let runner = layout.pane(PaneRole::Runner).unwrap();

        // Hit Agent viewport cell (col=3, row=2)
        let cell_x = agent.viewport.x + 3.0 * input.cell_w + 1.0;
        let cell_y = agent.viewport.y + 2.0 * input.cell_h + 1.0;
        assert_eq!(
            hit_test(&layout, 2, input.scale_factor, input.cell_w, input.cell_h, cell_x, cell_y),
            HitTarget::PaneViewport {
                role: PaneRole::Agent,
                col: 3,
                row: 2,
            }
        );

        // Hit Agent header (left side, away from action pill)
        assert_eq!(
            hit_test(
                &layout,
                2,
                input.scale_factor,
                input.cell_w,
                input.cell_h,
                agent.header.x + 10.0,
                agent.header.y + 10.0,
            ),
            HitTarget::PaneHeader(PaneRole::Agent)
        );

        // Hit Agent action pill
        let agent_pill = agent.action_pill.unwrap();
        assert_eq!(
            hit_test(
                &layout,
                2,
                input.scale_factor,
                input.cell_w,
                input.cell_h,
                agent_pill.x + 4.0,
                agent_pill.y + 4.0,
            ),
            HitTarget::PaneHeaderAction(PaneRole::Agent)
        );

        // Hit Runner failure action pill
        let runner_pill = runner.action_pill.unwrap();
        assert_eq!(
            hit_test(
                &layout,
                2,
                input.scale_factor,
                input.cell_w,
                input.cell_h,
                runner_pill.x + 4.0,
                runner_pill.y + 4.0,
            ),
            HitTarget::PaneHeaderAction(PaneRole::Runner)
        );

        // Hit sidebar workspace 1 and "+ New Workspace" button
        let sidebar = layout.sidebar.unwrap();
        let ws1 = sidebar_workspace_rect(sidebar, 1, input.scale_factor);
        assert_eq!(
            hit_test(
                &layout,
                2,
                input.scale_factor,
                input.cell_w,
                input.cell_h,
                ws1.x + 10.0,
                ws1.y + 10.0,
            ),
            HitTarget::SidebarWorkspace(1)
        );

        let new_btn = sidebar_new_workspace_rect(sidebar, 2, input.scale_factor);
        assert_eq!(
            hit_test(
                &layout,
                2,
                input.scale_factor,
                input.cell_w,
                input.cell_h,
                new_btn.x + 10.0,
                new_btn.y + 10.0,
            ),
            HitTarget::SidebarNewWorkspace
        );

        let new_wt_btn = sidebar_new_worktree_rect(sidebar, 2, input.scale_factor);
        assert_eq!(new_wt_btn.y, new_btn.y);
        assert!(new_wt_btn.x >= new_btn.right());
        assert!(new_wt_btn.right() <= sidebar.right());
        assert_eq!(
            hit_test(
                &layout,
                2,
                input.scale_factor,
                input.cell_w,
                input.cell_h,
                new_wt_btn.x + 10.0,
                new_wt_btn.y + 10.0,
            ),
            HitTarget::SidebarNewWorktree
        );

        // Outside window returns None
        assert_eq!(
            hit_test(&layout, 2, input.scale_factor, input.cell_w, input.cell_h, -5.0, -5.0),
            HitTarget::None
        );
    }
}
