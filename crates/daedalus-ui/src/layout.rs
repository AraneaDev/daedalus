pub mod overlays;

use crate::sidebar_content::{SidebarContent, SidebarRow};
use daedalus_proto::{PaneId, PaneRole, WorkspaceId};

pub const SIDEBAR_LOGICAL_WIDTH: f32 = 240.0;
pub const CARGO_BAR_LOGICAL_HEIGHT: f32 = 34.;
pub const WIDGET_BUTTON_MIN_LOGICAL_WIDTH: f32 = 110.;
pub const WIDGET_BUTTON_GAP_LOGICAL: f32 = 5.;
pub const HEADER_LOGICAL_HEIGHT: f32 = 26.0;
const FOCUSED_HEADER_LOGICAL_HEIGHT: f32 = 34.0;
const TAB_LABEL_PADDING: f32 = 10.0;
const TAB_CLOSE_LOGICAL_WIDTH: f32 = 28.0;
pub const BANNER_LOGICAL_HEIGHT: f32 = 24.0;
pub const VIEWPORT_LOGICAL_PAD: f32 = 6.0;
pub const AGENT_WIDTH_RATIO: f32 = 0.55;
// Below this width the right column cannot comfortably fit the Runner title and action.
const MIN_SPLIT_CANVAS_LOGICAL_WIDTH: f32 = 560.0;

pub const SIDEBAR_HEADER_LOGICAL_HEIGHT: f32 = 32.0;
pub const SIDEBAR_ROW_LOGICAL_HEIGHT: f32 = 96.0;
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

impl PaneRect {
    /// The tab row shares horizontal space with an inline action. A wrapped action
    /// sits below this row and leaves its full width available for navigation.
    pub fn tab_strip(&self, scale: f32) -> Rect {
        let mut strip = self.header;
        strip.height = strip.height.min(FOCUSED_HEADER_LOGICAL_HEIGHT * scale);
        if let Some(pill) = self.action_pill
            && pill.y < strip.bottom()
        {
            strip.width = (pill.x - strip.x).max(0.);
        }
        strip
    }
}

pub(crate) fn tab_label_padding(tab: Rect, scale: f32) -> f32 {
    (TAB_LABEL_PADDING * scale).min(tab.width * 0.14)
}

/// Bounds shared by painting and pointer input for the focused terminal's role tabs.
pub fn session_tabs(pane: &PaneRect, scale: f32) -> [(PaneRole, Rect); 3] {
    let strip = pane.tab_strip(scale);
    let left = strip.x + 4.0 * scale;
    let right = strip.right();
    let width = ((right - left) / 3.0).clamp(0.0, 84.0 * scale);
    [PaneRole::Agent, PaneRole::Service, PaneRole::Runner].map(|role| {
        let index = match role {
            PaneRole::Agent => 0.,
            PaneRole::Service => 1.,
            PaneRole::Runner => 2.,
        };
        (
            role,
            Rect::new(left + width * index, strip.y, width, strip.height),
        )
    })
}

/// Inputs required to solve the workspace layout for a single frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayoutInput {
    pub window_w: f32,
    pub window_h: f32,
    pub scale_factor: f32,
    pub cell_w: f32,
    pub cell_h: f32,
    pub sidebar_override: Option<bool>,
    pub has_top_banner: bool,
    pub zoomed: bool,
    pub focused_role: PaneRole,
    pub runner_has_failure: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabKind {
    Session(PaneId),
    PreviousPage(PaneId),
    NextPage(PaneId),
}

impl TabKind {
    pub fn target(self) -> PaneId {
        match self {
            Self::Session(id) | Self::PreviousPage(id) | Self::NextPage(id) => id,
        }
    }

    pub fn session(self) -> Option<PaneId> {
        match self {
            Self::Session(id) => Some(id),
            Self::PreviousPage(_) | Self::NextPage(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NamedTab {
    pub kind: TabKind,
    pub label: String,
    pub rect: Rect,
}

/// Computed rectangles for the sidebar, optional top banner, and visible semantic panes.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceLayout {
    pub insight_rows: Vec<(PaneRole, Rect)>,
    pub compact_recap: Option<Rect>,
    pub close_tab: Option<(PaneId, Rect)>,
    pub named_tabs: Vec<NamedTab>,
    pub stacked: bool,
    pub run_tests_button: Option<Rect>,
    pub launch_button: Option<Rect>,
    pub layout_button: Option<Rect>,
    pub cargo_bar: Option<Rect>,
    pub cargo_presets: [bool; 2],
    pub widget_bar: Option<Rect>,
    pub widget_buttons: Vec<(Option<usize>, Rect)>,
    pub failed_run_buttons: Vec<(WorkspaceId, Rect)>,
    pub sidebar_first: usize,
    pub sidebar_content: Option<std::sync::Arc<crate::sidebar_content::SidebarContent>>,
    pub sidebar_toggle: Option<Rect>,
    pub sidebar: Option<Rect>,
    pub banner: Option<Rect>,
    pub panes: Vec<PaneRect>,
}

impl WorkspaceLayout {
    pub fn preview_button(&self, state: &crate::state::AppState, scale: f32) -> Option<Rect> {
        if state.search.is_some() || self.preview_panel(state, scale).is_some() {
            return None;
        }
        state.selected_png_path()?;
        let view = self.pane(state.focused_role)?.viewport;
        let width = (114. * scale).min(view.width);
        Some(Rect::new(
            view.right() - width,
            view.y,
            width,
            (23. * scale).min(view.height),
        ))
    }

    pub fn preview_panel(&self, state: &crate::state::AppState, scale: f32) -> Option<Rect> {
        let (id, image) = state.image_preview.as_ref()?;
        if state.focused_pane_id() != Some(*id) || state.search.is_some() {
            return None;
        }
        let view = self.pane(state.focused_role)?.viewport;
        let width = (320. * scale).min(view.width * 0.45);
        let height =
            (width - 8. * scale).max(0.) * image.height as f32 / image.width as f32 + 28. * scale;
        Some(Rect::new(
            view.right() - width,
            view.y,
            width,
            height.min(view.height),
        ))
    }

    pub fn reserve_insights(&mut self, roles: &[PaneRole], scale: f32, cell_h: f32) {
        let [pane] = self.panes.as_mut_slice() else {
            return;
        };
        if roles.is_empty() || pane.role != PaneRole::Agent {
            return;
        }
        let height = (28. * scale).min(pane.viewport.height * 0.1);
        if height < 18. * scale {
            return;
        }
        let stacked = pane.outer.width < 580. * scale;
        let total = if stacked {
            height * roles.len() as f32
        } else {
            height
        };
        pane.outer.height -= total;
        pane.viewport.height -= total;
        pane.rows = (pane.viewport.height / cell_h.max(1.)).floor().max(1.) as u16;
        let width = if stacked {
            pane.outer.width
        } else {
            pane.outer.width / roles.len() as f32
        };
        for (i, &role) in roles.iter().enumerate() {
            let (dx, dy) = if stacked {
                (0., i as f32 * height)
            } else {
                (i as f32 * width, 0.)
            };
            self.insight_rows.push((
                role,
                Rect::new(pane.outer.x + dx, pane.outer.bottom() + dy, width, height),
            ));
        }
    }

    pub fn cargo_buttons(
        &self,
        scale: f32,
    ) -> impl Iterator<Item = (crate::assignments::CargoPreset, Rect)> {
        let presets = self.cargo_presets;
        let count = presets.into_iter().filter(|shown| *shown).count() as f32;
        self.cargo_bar
            .filter(|_| count > 0.)
            .into_iter()
            .flat_map(move |bar| {
                let gap = 8. * scale;
                let button_budget = (bar.width - (count + 1.) * gap).max(0.);
                // Leave space for one readable widget or the overflow button before
                // widening the Cargo actions to their normal desktop size.
                let widget_room = if self.widget_bar.is_some() {
                    let slot =
                        (WIDGET_BUTTON_MIN_LOGICAL_WIDTH + 2. * WIDGET_BUTTON_GAP_LOGICAL) * scale;
                    (button_budget - count * 72. * scale).clamp(0., slot)
                } else {
                    0.
                };
                let width = ((button_budget - widget_room) / count).min(112. * scale);
                crate::assignments::CargoPreset::ALL
                    .into_iter()
                    .zip(presets)
                    .filter_map(|(preset, shown)| shown.then_some(preset))
                    .enumerate()
                    .map(move |(index, preset)| {
                        (
                            preset,
                            Rect::new(
                                bar.x + gap + index as f32 * (width + gap),
                                bar.y + 4. * scale,
                                width,
                                (bar.height - 8. * scale).max(0.),
                            ),
                        )
                    })
            })
    }

    pub fn enable_launch_button(&mut self, scale: f32) {
        let Some(header) = self.sidebar_toggle else {
            return;
        };
        let settings = settings_button(
            header,
            self.sidebar_content.as_ref().and_then(|list| list.arrows),
            scale,
        );
        let width = (52. * scale).min((settings.x - header.x) * 0.7);
        self.launch_button = Some(Rect::new(
            settings.x - width,
            header.y,
            width,
            header.height,
        ));
    }

    pub fn enable_layout_button(&mut self, scale: f32) {
        let Some(header) = self.sidebar_toggle else {
            return;
        };
        let settings = settings_button(
            header,
            self.sidebar_content.as_ref().and_then(|list| list.arrows),
            scale,
        );
        let right = self
            .run_tests_button
            .or(self.launch_button)
            .unwrap_or(settings)
            .x;
        let width = 52. * scale;
        if right - header.x >= width + 32. * scale {
            self.layout_button = Some(Rect::new(right - width, header.y, width, header.height));
        }
    }

    pub fn enable_run_tests(&mut self, scale: f32) {
        let Some(header) = self.sidebar_toggle else {
            return;
        };
        let settings = settings_button(
            header,
            self.sidebar_content.as_ref().and_then(|list| list.arrows),
            scale,
        );
        let width = 68. * scale;
        let right = self.launch_button.unwrap_or(settings).x;
        let reserve = if self.launch_button.is_some() {
            84.
        } else {
            42.
        };
        if right - header.x >= width + reserve * scale {
            self.run_tests_button = Some(Rect::new(right - width, header.y, width, header.height));
        }
    }

    pub fn set_named_tabs(
        &mut self,
        entries: &[(PaneId, String)],
        focused: Option<PaneId>,
        scale: f32,
    ) {
        let [pane] = self.panes.as_slice() else {
            return;
        };
        let strip = pane.tab_strip(scale);
        let left = strip.x + 3. * scale;
        let right = strip.right();
        let available = (right - left).max(0.);
        if entries.is_empty() || available < 1. {
            return;
        }
        let capacity = ((available / (100. * scale)).floor() as usize)
            .max(3)
            .min(entries.len());
        let index = entries
            .iter()
            .position(|(id, _)| Some(*id) == focused)
            .unwrap_or(0);
        let overflow = entries.len() > capacity || (entries.len() >= 3 && available < 150. * scale);
        let visible = if overflow { capacity - 2 } else { capacity };
        let start = (index / visible * visible).min(entries.len() - visible);
        let compact = overflow && available < 300. * scale;
        let arrow_width = if compact {
            (available / 4.).min(26. * scale)
        } else {
            0.
        };
        let width = if compact {
            (available - 2. * arrow_width) / visible as f32
        } else {
            (available / capacity as f32).min(148. * scale)
        };
        let preferred_extra = |label: &str, close_width: f32| {
            let text_width = unicode_width::UnicodeWidthStr::width(label) as f32 * 9.;
            let preferred = (text_width + 2. * TAB_LABEL_PADDING + 36. + close_width).min(240.);
            (preferred * scale - width).max(0.)
        };
        // Keep the selected tab readable first, then fit longer labels into any spare width.
        let mut spare = (available - width * capacity as f32).max(0.);
        let selected_extra = if !overflow && focused == Some(entries[index].0) {
            preferred_extra(&entries[index].1, TAB_CLOSE_LOGICAL_WIDTH).min(spare)
        } else {
            0.
        };
        spare -= selected_extra;
        let mut x = left;
        let mut push = |kind, label: String, w| {
            self.named_tabs.push(NamedTab {
                kind,
                label,
                rect: Rect::new(x, strip.y, w, strip.height),
            });
            x += w;
        };
        if overflow {
            push(
                TabKind::PreviousPage(entries[(start + entries.len() - 1) % entries.len()].0),
                if compact { "‹" } else { "‹ Prev" }.into(),
                if compact { arrow_width } else { width },
            );
        }
        for (id, label) in &entries[start..start + visible] {
            let extra = if overflow {
                0.
            } else if Some(*id) == focused {
                selected_extra
            } else {
                let extra = preferred_extra(label, 0.).min(spare);
                spare -= extra;
                extra
            };
            push(TabKind::Session(*id), label.clone(), width + extra);
        }
        if overflow {
            push(
                TabKind::NextPage(entries[(start + visible) % entries.len()].0),
                if compact { "›" } else { "Next ›" }.into(),
                if compact { arrow_width } else { width },
            );
        }
        if let Some(id) = focused
            && let Some(tab) = self
                .named_tabs
                .iter()
                .rev()
                .find(|tab| tab.kind == TabKind::Session(id))
            && tab.rect.width >= 52. * scale
        {
            let rect = tab.rect;
            let width = (TAB_CLOSE_LOGICAL_WIDTH * scale).min(rect.width - 30. * scale);
            self.close_tab = Some((
                id,
                Rect::new(rect.right() - width, rect.y, width, rect.height),
            ));
        }
    }

    pub fn pane(&self, role: PaneRole) -> Option<&PaneRect> {
        self.panes.iter().find(|p| p.role == role)
    }
}

/// Result of hit-testing a physical pixel coordinate against a `WorkspaceLayout`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitTarget {
    ToggleSidebar,
    ToggleLayout,
    RunCargo(crate::assignments::CargoPreset),
    WorkspaceSettings,
    SidebarPageUp,
    SidebarPageDown,
    SidebarWorkspace(usize),
    SidebarWorkspaceClose(usize),
    SidebarFailedRun(WorkspaceId),
    SidebarNewWorkspace,
    SidebarNewWorktree,
    RunTests,
    OpenSessions,
    Widget(PaneRole),
    CustomWidget(Option<usize>),
    SessionTab(PaneRole),
    SelectSession(PaneId),
    CloseSession(PaneId),
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
    zoomed: bool,
) -> PaneRect {
    let pill_logical_w = match role {
        PaneRole::Agent => Some(AGENT_PILL_LOGICAL_WIDTH),
        PaneRole::Runner if runner_has_failure => Some(RUNNER_PILL_LOGICAL_WIDTH),
        PaneRole::Service | PaneRole::Runner => None,
    };

    let header_row_height = if zoomed {
        FOCUSED_HEADER_LOGICAL_HEIGHT * metrics.scale_factor
    } else if outer.height >= 200.0 * metrics.scale_factor {
        36.0 * metrics.scale_factor
    } else {
        metrics.header_height
    };
    let narrow = zoomed && outer.width < 420. * metrics.scale_factor;
    let wrap_action = narrow
        && pill_logical_w.is_some()
        && outer.width < 200. * metrics.scale_factor
        && outer.height >= header_row_height * 3.;
    let header_rows = if wrap_action { 2. } else { 1. };
    let header_h = (header_row_height * header_rows).min(outer.height.max(0.0));
    let header = Rect::new(outer.x, outer.y, outer.width.max(0.0), header_h);
    let action_pill = pill_logical_w.and_then(|logical_w| {
        let margin_x = PILL_MARGIN_X_LOGICAL * metrics.scale_factor;
        let margin_y = PILL_MARGIN_Y_LOGICAL * metrics.scale_factor
            + (header_row_height - metrics.header_height) * 0.5;
        let max_w = (header.width - 2.0 * margin_x).max(0.0);
        let logical_w = if narrow { 72. } else { logical_w };
        let pill_w = (logical_w * metrics.scale_factor).min(max_w);
        let pill_h = (header.height.min(header_row_height) - 2.0 * margin_y).max(0.0);
        if pill_w > 0.0 && pill_h > 0.0 {
            Some(Rect::new(
                header.right() - margin_x - pill_w,
                header.bottom() - margin_y - pill_h,
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
    compute_split_layout(input, &PaneSplits::default())
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Divider {
    Column,
    RightRow,
    UpperRow,
    LowerRow,
}

#[derive(Clone, Copy, Debug)]
pub struct PaneSplits {
    column: f32,
    right_row: f32,
    upper: f32,
    lower: f32,
}

impl Default for PaneSplits {
    fn default() -> Self {
        Self {
            column: AGENT_WIDTH_RATIO,
            right_row: 0.5,
            upper: 0.5,
            lower: 0.75,
        }
    }
}

impl PaneSplits {
    pub fn portable(&self, split_view: bool) -> daedalus_proto::workbench::WorkbenchLayout {
        daedalus_proto::workbench::WorkbenchLayout {
            split_view,
            splits: [
                ("column", self.column),
                ("right_row", self.right_row),
                ("upper", self.upper),
                ("lower", self.lower),
            ]
            .into_iter()
            .map(|(key, value)| (key.into(), (value * 1000.).round() as u16))
            .collect(),
        }
    }

    pub fn from_portable(layout: &daedalus_proto::workbench::WorkbenchLayout) -> Self {
        let mut splits = Self::default();
        let get = |name| {
            layout
                .splits
                .get(name)
                .map(|value| f32::from(*value) / 1000.)
        };
        if let Some(value) = get("column") {
            splits.move_divider(Divider::Column, value);
        }
        if let Some(value) = get("right_row") {
            splits.move_divider(Divider::RightRow, value);
        }
        if let Some(value) = get("lower") {
            splits.lower = value.clamp(0.2, 0.9);
        }
        splits.move_divider(Divider::UpperRow, get("upper").unwrap_or(splits.upper));
        splits
    }
    pub fn move_divider(&mut self, divider: Divider, fraction: f32) {
        match divider {
            Divider::Column => self.column = fraction.clamp(0.15, 0.85),
            Divider::RightRow => self.right_row = fraction.clamp(0.15, 0.85),
            Divider::UpperRow => self.upper = fraction.clamp(0.1, self.lower - 0.1),
            Divider::LowerRow => self.lower = fraction.clamp(self.upper + 0.1, 0.9),
        }
    }
}

struct WorkbenchCanvas {
    bounds: Rect,
    stacked: bool,
    sidebar: Option<Rect>,
    sidebar_toggle: Option<Rect>,
    banner: Option<Rect>,
    compact_recap: Option<Rect>,
}

fn resolve_workbench_canvas(input: &LayoutInput, metrics: &LayoutMetrics) -> WorkbenchCanvas {
    let window_w = input.window_w.max(0.0);
    let window_h = input.window_h.max(0.0);

    let min_split_width = MIN_SPLIT_CANVAS_LOGICAL_WIDTH * metrics.scale_factor;
    let show_sidebar = input.sidebar_override.unwrap_or(
        window_w >= metrics.sidebar_width + min_split_width
            && (!input.zoomed
                || (window_w >= 900.0 * metrics.scale_factor
                    && window_h >= 520.0 * metrics.scale_factor)),
    );
    let (sidebar, sidebar_w) = if show_sidebar {
        let mut w = metrics.sidebar_width.min(window_w);
        if input.zoomed && input.sidebar_override.is_none() {
            let banner = if input.has_top_banner {
                metrics.banner_height
            } else {
                0.0
            };
            let terminal_h = (window_h
                - banner
                - FOCUSED_HEADER_LOGICAL_HEIGHT * metrics.scale_factor
                - 2.0 * metrics.viewport_pad)
                .max(1.0);
            let required_width = (window_w * window_h * (2.0 / 3.0) / terminal_h).ceil();
            w = w.min((window_w - required_width - 2.0 * metrics.viewport_pad).max(0.0));
        }
        (
            Some(Rect::new(
                if input.zoomed { window_w - w } else { 0. },
                0.0,
                w,
                window_h,
            )),
            w,
        )
    } else {
        (None, 0.0)
    };

    let canvas_x = if input.zoomed { 0. } else { sidebar_w };
    let canvas_w = (window_w - sidebar_w).max(0.0);

    let sidebar_toggle = (window_w > 0.0 && window_h > 0.0).then(|| {
        Rect::new(
            sidebar.map_or(0., |rect| rect.x),
            0.0,
            sidebar.map_or(window_w, |rect| rect.width),
            metrics.sidebar_header_height.min(window_h),
        )
    });
    let canvas_top = if sidebar.is_none() {
        sidebar_toggle.map_or(0.0, |rect| rect.bottom())
    } else {
        0.0
    };

    let (banner, banner_h) = if input.has_top_banner {
        let h = metrics.banner_height.min(window_h - canvas_top);
        (Some(Rect::new(canvas_x, canvas_top, canvas_w, h)), h)
    } else {
        (None, 0.0)
    };

    let canvas_y = canvas_top + banner_h;
    let mut canvas_h = (window_h - canvas_y).max(0.0);
    let compact_recap =
        if input.zoomed && sidebar.is_none() && input.sidebar_override != Some(false) {
            let terminal_width = (canvas_w - 2. * metrics.viewport_pad).max(1.);
            let required_height = window_w * window_h * (2. / 3.) / terminal_width
                + FOCUSED_HEADER_LOGICAL_HEIGHT * metrics.scale_factor
                + 2. * metrics.viewport_pad;
            let height = (canvas_h - required_height).min(window_h * 0.19).max(0.);
            canvas_h -= height;
            Some(Rect::new(canvas_x, canvas_y + canvas_h, canvas_w, height))
        } else {
            None
        };

    WorkbenchCanvas {
        bounds: Rect::new(canvas_x, canvas_y, canvas_w, canvas_h),
        stacked: canvas_w < min_split_width,
        sidebar,
        sidebar_toggle,
        banner,
        compact_recap,
    }
}

fn split_pane_bounds(
    canvas: &WorkbenchCanvas,
    metrics: &LayoutMetrics,
    splits: &PaneSplits,
    cell_h: f32,
) -> [Rect; 3] {
    let Rect {
        x: canvas_x,
        y: canvas_y,
        width: canvas_w,
        height: canvas_h,
    } = canvas.bounds;
    if canvas.stacked {
        // Reserve a readable terminal row for each role when the window is short.
        let one_row = metrics.header_height + 2. * metrics.viewport_pad + cell_h.max(0.);
        let minimum = one_row.min(canvas_h / 3.);
        let lower = (canvas_h * splits.lower)
            .max(2. * minimum)
            .min(canvas_h - minimum);
        let agent_h = (canvas_h * splits.upper).max(minimum).min(lower - minimum);
        let service_h = lower - agent_h;
        [
            Rect::new(canvas_x, canvas_y, canvas_w, agent_h),
            Rect::new(canvas_x, canvas_y + agent_h, canvas_w, service_h),
            Rect::new(
                canvas_x,
                canvas_y + agent_h + service_h,
                canvas_w,
                canvas_h - agent_h - service_h,
            ),
        ]
    } else {
        let agent_w = canvas_w * splits.column;
        let right_w = (canvas_w - agent_w).max(0.0);
        let top_h = canvas_h * splits.right_row;
        let bot_h = (canvas_h - top_h).max(0.0);

        let gutter = (4.0 * metrics.scale_factor).min(canvas_h * 0.02);
        let inset =
            |x, y, w, h| Rect::new(x + gutter, y + gutter, w - 2.0 * gutter, h - 2.0 * gutter);
        let agent_outer = inset(canvas_x, canvas_y, agent_w, canvas_h);
        let service_outer = inset(canvas_x + agent_w, canvas_y, right_w, top_h);
        let runner_outer = inset(canvas_x + agent_w, canvas_y + top_h, right_w, bot_h);

        [agent_outer, service_outer, runner_outer]
    }
}

pub fn compute_split_layout(input: &LayoutInput, splits: &PaneSplits) -> WorkspaceLayout {
    let metrics = LayoutMetrics::for_scale(input.scale_factor);
    let canvas = resolve_workbench_canvas(input, &metrics);
    let build = |role, outer| {
        build_pane_rect(
            role,
            outer,
            &metrics,
            input.cell_w,
            input.cell_h,
            input.runner_has_failure,
            input.zoomed,
        )
    };
    let panes = if input.zoomed {
        vec![build(input.focused_role, canvas.bounds)]
    } else {
        [PaneRole::Agent, PaneRole::Service, PaneRole::Runner]
            .into_iter()
            .zip(split_pane_bounds(&canvas, &metrics, splits, input.cell_h))
            .map(|(role, outer)| build(role, outer))
            .collect()
    };

    WorkspaceLayout {
        insight_rows: Vec::new(),
        widget_bar: None,
        widget_buttons: Vec::new(),
        compact_recap: canvas.compact_recap,
        close_tab: None,
        named_tabs: Vec::new(),
        sidebar_toggle: canvas.sidebar_toggle,
        run_tests_button: None,
        launch_button: None,
        layout_button: None,
        cargo_bar: None,
        cargo_presets: [true; 2],
        failed_run_buttons: Vec::new(),
        sidebar_first: 0,
        sidebar_content: None,
        stacked: canvas.stacked,
        sidebar: canvas.sidebar,
        banner: canvas.banner,
        panes,
    }
}

/// Geometry shared by sidebar painting, scrolling and hit testing.
pub struct SidebarList {
    pub visible: std::ops::Range<usize>,
    pub arrows: Option<(Rect, Rect)>,
}

impl SidebarList {
    pub fn new(sidebar: Rect, count: usize, first: usize, scale: f32) -> Self {
        let m = LayoutMetrics::for_scale(scale);
        let available = (sidebar.height
            - m.sidebar_header_height
            - 2.0 * m.sidebar_pad
            - m.sidebar_new_btn_height
            - m.sidebar_row_gap)
            .max(0.0);
        let mut capacity =
            (available / (m.sidebar_row_height + m.sidebar_row_gap)).floor() as usize;
        if capacity == 0 && available >= m.header_height {
            capacity = 1;
        }
        let capacity = capacity.min(count);
        let start = first.min(count.saturating_sub(capacity));
        let arrows = (capacity > 0 && count > capacity).then(|| {
            let width = m.sidebar_header_height.min(sidebar.width * 0.25);
            let right = sidebar.right() - m.sidebar_pad;
            (
                Rect::new(
                    right - 2.0 * width,
                    sidebar.y,
                    width,
                    m.sidebar_header_height,
                ),
                Rect::new(right - width, sidebar.y, width, m.sidebar_header_height),
            )
        });
        Self {
            visible: start..start + capacity,
            arrows,
        }
    }

    pub fn reveal(&self, index: usize) -> usize {
        if index < self.visible.start {
            index
        } else if index >= self.visible.end {
            (index + 1).saturating_sub(self.visible.len())
        } else {
            self.visible.start
        }
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
    let footer_top =
        sidebar.bottom() - m.sidebar_pad - m.sidebar_new_btn_height - m.sidebar_row_gap;
    Rect::new(x, y, w, m.sidebar_row_height.min((footer_top - y).max(0.0)))
}

/// Keeps the failed-run action inside the card and clear of its close control.
pub fn sidebar_failed_run_rect(card: Rect, scale_factor: f32) -> Rect {
    let s = LayoutMetrics::for_scale(scale_factor).scale_factor;
    let right = (card.right() - 24.0 * s).max(card.x);
    let left = (right - 78.0 * s).max(card.x);
    let top = (card.y + 3.0 * s).min(card.bottom());
    Rect::new(left, top, right - left, (card.bottom() - top).min(22.0 * s))
}

/// Computes the compact `×` close button rectangle in the top-right corner of a sidebar workspace card.
pub fn sidebar_workspace_close_rect(card: Rect, scale_factor: f32) -> Rect {
    let s = if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    let size = 16.0 * s;
    let w = size.min(card.width.max(0.0));
    let h = size.min(card.height.max(0.0));
    let x = (card.right() - w - 4.0 * s).max(card.x);
    let y = card.y + 5.0 * s;
    Rect::new(x, y, w, h)
}

fn sidebar_footer_buttons_y(sidebar: Rect, workspace_count: usize, m: &LayoutMetrics) -> f32 {
    let after_cards = sidebar.y
        + m.sidebar_header_height
        + m.sidebar_pad
        + (workspace_count as f32) * (m.sidebar_row_height + m.sidebar_row_gap)
        + m.sidebar_row_gap;
    after_cards
        .min(sidebar.bottom() - m.sidebar_pad - m.sidebar_new_btn_height)
        .max(sidebar.y + m.sidebar_header_height.min(sidebar.height))
}

/// Computes the rectangle of the "+ Workspace" button inside the sidebar.
pub fn sidebar_new_workspace_rect(
    sidebar: Rect,
    workspace_count: usize,
    scale_factor: f32,
) -> Rect {
    let m = LayoutMetrics::for_scale(scale_factor);
    let total_w = (sidebar.width - 2.0 * m.sidebar_pad).max(0.0);
    let btn_w = ((total_w - m.sidebar_row_gap) * 0.5).max(0.0);
    let x = sidebar.x + m.sidebar_pad;
    let y = sidebar_footer_buttons_y(sidebar, workspace_count, &m);
    Rect::new(
        x,
        y,
        btn_w,
        m.sidebar_new_btn_height
            .min((sidebar.bottom() - y).max(0.0)),
    )
}

/// Computes the rectangle of the "+ Worktree" button next to "+ Workspace" inside the sidebar.
pub fn sidebar_new_worktree_rect(sidebar: Rect, workspace_count: usize, scale_factor: f32) -> Rect {
    let m = LayoutMetrics::for_scale(scale_factor);
    let total_w = (sidebar.width - 2.0 * m.sidebar_pad).max(0.0);
    let left_w = ((total_w - m.sidebar_row_gap) * 0.5).max(0.0);
    let right_w = (total_w - left_w - m.sidebar_row_gap).max(0.0);
    let x = sidebar.x + m.sidebar_pad + left_w + m.sidebar_row_gap;
    let y = sidebar_footer_buttons_y(sidebar, workspace_count, &m);
    Rect::new(
        x,
        y,
        right_w,
        m.sidebar_new_btn_height
            .min((sidebar.bottom() - y).max(0.0)),
    )
}

/// Settings occupies the end of the workspace heading, before any paging arrows.
pub fn settings_button(header: Rect, arrows: Option<(Rect, Rect)>, scale: f32) -> Rect {
    let right = arrows.map_or(header.right(), |(up, _)| up.x);
    let width = (64. * scale).min((right - header.x) * 0.48);
    Rect::new(right - width, header.y, width, header.height)
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
    let point = (x, y);
    if let Some(target) = footer_target(layout, scale_factor, point) {
        return target;
    }
    let fallback = layout
        .sidebar
        .filter(|_| layout.sidebar_content.is_none())
        .map(|rect| {
            SidebarContent::plain(workspace_count, rect, layout.sidebar_first, scale_factor)
        });
    let list = layout.sidebar_content.as_deref().or(fallback.as_ref());
    if let Some(target) =
        workbench_controls_target(layout, list, workspace_count, scale_factor, point)
    {
        return target;
    }
    if layout.sidebar.is_some_and(|sidebar| sidebar.contains(x, y)) {
        return sidebar_target(
            layout,
            list.expect("visible sidebar has content"),
            scale_factor,
            point,
        );
    }

    layout
        .panes
        .iter()
        .find_map(|pane| {
            pane_target(
                layout,
                pane,
                workspace_count > 0,
                scale_factor,
                (cell_w, cell_h),
                point,
            )
        })
        .unwrap_or(HitTarget::None)
}

fn footer_target(
    layout: &WorkspaceLayout,
    scale_factor: f32,
    (x, y): (f32, f32),
) -> Option<HitTarget> {
    if let Some((index, _)) = layout
        .widget_buttons
        .iter()
        .find(|(_, rect)| rect.contains(x, y))
    {
        return Some(HitTarget::CustomWidget(*index));
    }
    if let Some((preset, _)) = layout
        .cargo_buttons(scale_factor)
        .find(|(_, rect)| rect.contains(x, y))
    {
        return Some(HitTarget::RunCargo(preset));
    }
    if let Some((role, _)) = layout.insight_rows.iter().find(|(_, r)| r.contains(x, y)) {
        return Some(HitTarget::Widget(*role));
    }
    None
}

fn workbench_controls_target(
    layout: &WorkspaceLayout,
    list: Option<&SidebarContent>,
    workspace_count: usize,
    scale_factor: f32,
    (x, y): (f32, f32),
) -> Option<HitTarget> {
    if let Some((up, down)) = list.and_then(|list| list.arrows) {
        if up.contains(x, y) {
            return Some(HitTarget::SidebarPageUp);
        }
        if down.contains(x, y) {
            return Some(HitTarget::SidebarPageDown);
        }
    }
    if layout
        .layout_button
        .is_some_and(|button| button.contains(x, y))
    {
        return Some(HitTarget::ToggleLayout);
    }
    if layout
        .launch_button
        .is_some_and(|button| button.contains(x, y))
    {
        return Some(HitTarget::OpenSessions);
    }
    if layout
        .run_tests_button
        .is_some_and(|button| button.contains(x, y))
    {
        return Some(HitTarget::RunTests);
    }
    if workspace_count > 0
        && layout.sidebar_toggle.is_some_and(|rect| {
            settings_button(rect, list.and_then(|l| l.arrows), scale_factor).contains(x, y)
        })
    {
        return Some(HitTarget::WorkspaceSettings);
    }
    if layout
        .sidebar_toggle
        .is_some_and(|rect| rect.contains(x, y))
    {
        return Some(HitTarget::ToggleSidebar);
    }
    None
}

fn sidebar_target(
    layout: &WorkspaceLayout,
    list: &SidebarContent,
    scale_factor: f32,
    (x, y): (f32, f32),
) -> HitTarget {
    if let Some(&(workspace, _)) = layout
        .failed_run_buttons
        .iter()
        .find(|(_, rect)| rect.contains(x, y))
    {
        return HitTarget::SidebarFailedRun(workspace);
    }
    for (row, rect) in list.rows[list.visible.clone()].iter().zip(&list.rects) {
        if let SidebarRow::Workspace(i) = row
            && rect.contains(x, y)
        {
            if sidebar_workspace_close_rect(*rect, scale_factor).contains(x, y) {
                return HitTarget::SidebarWorkspaceClose(*i);
            }
            return HitTarget::SidebarWorkspace(*i);
        }
    }
    if list.new_workspace.contains(x, y) {
        return HitTarget::SidebarNewWorkspace;
    }
    if list.new_worktree.contains(x, y) {
        return HitTarget::SidebarNewWorktree;
    }
    HitTarget::None
}

fn pane_target(
    layout: &WorkspaceLayout,
    pane: &PaneRect,
    has_workspace: bool,
    scale_factor: f32,
    (cell_w, cell_h): (f32, f32),
    (x, y): (f32, f32),
) -> Option<HitTarget> {
    if !has_workspace && pane.header.contains(x, y) {
        return Some(HitTarget::None);
    }
    if let Some(pill) = pane.action_pill
        && pill.contains(x, y)
    {
        return Some(HitTarget::PaneHeaderAction(pane.role));
    }
    if let Some((id, rect)) = layout.close_tab
        && rect.contains(x, y)
    {
        return Some(HitTarget::CloseSession(id));
    }
    if let Some(tab) = layout.named_tabs.iter().find(|tab| tab.rect.contains(x, y)) {
        return Some(HitTarget::SelectSession(tab.kind.target()));
    }
    if layout.named_tabs.is_empty()
        && layout.panes.len() == 1
        && let Some((role, _)) = session_tabs(pane, scale_factor)
            .into_iter()
            .find(|(_, rect)| rect.contains(x, y))
    {
        return Some(HitTarget::SessionTab(role));
    }
    if pane.header.contains(x, y) {
        return Some(HitTarget::PaneHeader(pane.role));
    }
    pane.viewport
        .contains(x, y)
        .then(|| HitTarget::PaneViewport {
            role: pane.role,
            col: viewport_cell(x - pane.viewport.x, cell_w, pane.cols),
            row: viewport_cell(y - pane.viewport.y, cell_h, pane.rows),
        })
}

fn viewport_cell(offset: f32, size: f32, count: u16) -> u16 {
    if size > 0.0 {
        ((offset / size).floor().max(0.0) as u16).min(count.saturating_sub(1))
    } else {
        0
    }
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
            sidebar_override: None,
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

        let agent = layout.pane(PaneRole::Agent).unwrap();
        let service = layout.pane(PaneRole::Service).unwrap();
        let runner = layout.pane(PaneRole::Runner).unwrap();
        assert_eq!(agent.outer, Rect::new(244.0, 4.0, 542.0, 792.0));
        assert_eq!(service.outer, Rect::new(794.0, 4.0, 442.0, 392.0));
        assert_eq!(runner.outer, Rect::new(794.0, 404.0, 442.0, 392.0));

        // Agent always gets preset action pill; Service never; Runner only on failure
        assert_eq!(agent.action_pill.unwrap().width, 116.0);
        assert!(service.action_pill.is_none());
        assert!(runner.action_pill.is_none());
    }

    #[test]
    fn resizing_updates_terminal_rectangles_in_both_layouts() {
        let mut input = default_input();
        let mut splits = PaneSplits::default();
        splits.move_divider(Divider::Column, 0.7);
        splits.move_divider(Divider::RightRow, 0.625);
        let wide = compute_split_layout(&input, &splits);
        assert_eq!(wide.pane(PaneRole::Agent).unwrap().outer.width, 692.);
        assert_eq!(wide.pane(PaneRole::Runner).unwrap().outer.y, 504.);

        input.window_w = 320.;
        splits.move_divider(Divider::UpperRow, 0.35);
        splits.move_divider(Divider::LowerRow, 0.8);
        let narrow = compute_split_layout(&input, &splits);
        let agent = narrow.pane(PaneRole::Agent).unwrap().outer;
        let runner = narrow.pane(PaneRole::Runner).unwrap().outer;
        let height = runner.bottom() - agent.y;
        assert!((agent.height - height * 0.35).abs() < 0.01);
        assert!((runner.height - height * 0.2).abs() < 0.01);
    }

    #[test]
    fn focused_tabs_have_room_for_labels_and_the_close_button() {
        for scale in [1., 1.5, 2.] {
            let entries = [
                (PaneId::new(), "Claude".into()),
                (PaneId::new(), "Codex".into()),
                (PaneId::new(), "Exited 0 · Cargo Test".into()),
            ];
            let mut layout = compute_layout(&LayoutInput {
                window_w: 1000. * scale,
                window_h: 600. * scale,
                scale_factor: scale,
                zoomed: true,
                ..default_input()
            });
            layout.set_named_tabs(&entries, Some(entries[1].0), scale);
            let active = layout.named_tabs[1].rect;
            let (_, close) = layout.close_tab.unwrap();
            assert_eq!(active.height, 34. * scale);
            assert_eq!(close.width, 28. * scale);
            assert!(layout.named_tabs[2].rect.width >= 180. * scale);
            assert_eq!(close.right(), active.right());
            assert!(layout.named_tabs[2].rect.right() <= layout.panes[0].tab_strip(scale).right());
        }
    }

    #[test]
    fn wide_header_uses_spare_space_for_the_selected_terminal_title() {
        let entries = [
            (PaneId::new(), "Agent".into()),
            (PaneId::new(), "Service".into()),
            (PaneId::new(), "Exited 3 · Runner".into()),
        ];
        let mut layout = compute_layout(&LayoutInput {
            window_w: 1440.,
            zoomed: true,
            ..default_input()
        });
        layout.set_named_tabs(&entries, Some(entries[2].0), 1.);
        let tab = layout.named_tabs[2].rect;
        assert!(tab.width >= 160.);
        assert!(tab.right() <= layout.panes[0].tab_strip(1.).right());
        assert_eq!(layout.close_tab.unwrap().1.right(), tab.right());
    }

    #[test]
    fn cargo_bar_buttons_map_to_their_own_presets() {
        let mut layout = compute_layout(&default_input());
        layout.cargo_bar = Some(Rect::new(0., 800., 480., CARGO_BAR_LOGICAL_HEIGHT));
        let buttons = layout.cargo_buttons(1.).collect::<Vec<_>>();
        assert_eq!(buttons.len(), 2);
        assert!(buttons[0].1.right() < buttons[1].1.x);
        for (preset, rect) in buttons {
            assert_eq!(
                hit_test(&layout, 1, 1., 8., 16., rect.x + 4., rect.y + 4.),
                HitTarget::RunCargo(preset)
            );
        }
    }

    #[test]
    fn layout_switch_has_a_separate_header_target_in_both_views() {
        for (zoomed, scale) in [(true, 1.), (false, 1.5)] {
            let mut layout = compute_layout(&LayoutInput {
                window_w: 1200. * scale,
                window_h: 700. * scale,
                scale_factor: scale,
                zoomed,
                ..default_input()
            });
            layout.enable_run_tests(scale);
            layout.enable_layout_button(scale);
            let view = layout.layout_button.unwrap();
            let run = layout.run_tests_button.unwrap();
            assert!(view.right() <= run.x);
            assert_eq!(
                hit_test(&layout, 1, scale, 8., 16., view.x + 1., view.y + 1.),
                HitTarget::ToggleLayout
            );
            assert_eq!(layout.panes.len(), if zoomed { 1 } else { 3 });
        }
    }

    #[test]
    fn saved_test_button_has_its_own_workspace_header_target() {
        for scale in [1., 1.5, 2.] {
            let mut layout = compute_layout(&LayoutInput {
                scale_factor: scale,
                ..default_input()
            });
            layout.enable_run_tests(scale);
            let button = layout.run_tests_button.unwrap();
            assert!(
                button.right() <= settings_button(layout.sidebar_toggle.unwrap(), None, scale).x
            );
            assert_eq!(
                hit_test(
                    &layout,
                    1,
                    scale,
                    8.,
                    16.,
                    button.x + 8. * scale,
                    button.y + 5. * scale
                ),
                HitTarget::RunTests
            );
        }
    }

    #[test]
    fn overflowing_session_tabs_reveal_and_select_the_requested_pane() {
        let entries: Vec<_> = (0..12)
            .map(|i| (PaneId::new(), format!("Session {i}")))
            .collect();
        let mut layout = compute_layout(&LayoutInput {
            window_w: 320.,
            window_h: 480.,
            zoomed: true,
            ..default_input()
        });
        layout.set_named_tabs(&entries, Some(entries[6].0), 1.);
        assert!(
            layout
                .named_tabs
                .iter()
                .any(|tab| tab.kind == TabKind::Session(entries[6].0) && tab.label == "Session 6")
        );
        for tab in &layout.named_tabs {
            let rect = tab.rect;
            assert_eq!(
                hit_test(
                    &layout,
                    1,
                    1.,
                    8.,
                    16.,
                    rect.x + rect.width * 0.5,
                    rect.y + rect.height * 0.5
                ),
                HitTarget::SelectSession(tab.kind.target())
            );
        }
        if let Some((id, button)) = layout.close_tab {
            assert_eq!(id, entries[6].0);
            assert_eq!(
                hit_test(&layout, 1, 1., 8., 16., button.x + 1., button.y + 1.),
                HitTarget::CloseSession(id)
            );
        }
        let previous = layout.named_tabs.first().unwrap();
        assert_eq!(previous.kind, TabKind::PreviousPage(entries[5].0));
        let next = layout.named_tabs.last().unwrap();
        assert_eq!(next.label, "›"); // Compact paging reserves width for the selected terminal.
        assert_eq!(next.kind, TabKind::NextPage(entries[7].0));
    }

    #[test]
    fn empty_workbench_header_does_not_offer_terminal_actions() {
        let input = LayoutInput {
            zoomed: true,
            ..default_input()
        };
        let layout = compute_layout(&input);
        let pane = &layout.panes[0];
        let pill = pane.action_pill.unwrap();
        assert_eq!(
            hit_test(
                &layout,
                0,
                input.scale_factor,
                input.cell_w,
                input.cell_h,
                pill.x + 1.,
                pill.y + 1.
            ),
            HitTarget::None
        );
    }

    #[test]
    fn compact_headers_keep_terminal_paging_separate_from_preset_and_failure_actions() {
        for scale in [1., 2.] {
            for role in [PaneRole::Agent, PaneRole::Runner] {
                let entries: Vec<_> = (0..5)
                    .map(|n| (PaneId::new(), format!("Session {n}")))
                    .collect();
                let mut layout = compute_layout(&LayoutInput {
                    window_w: 320. * scale,
                    window_h: 568. * scale,
                    scale_factor: scale,
                    sidebar_override: Some(true),
                    zoomed: true,
                    focused_role: role,
                    runner_has_failure: true,
                    ..default_input()
                });
                layout.set_named_tabs(&entries, Some(entries[2].0), scale);
                let pill = layout.panes[0].action_pill.unwrap();
                let hit = |r: Rect| {
                    hit_test(
                        &layout,
                        1,
                        scale,
                        8.,
                        16.,
                        r.x + r.width / 2.,
                        r.y + r.height / 2.,
                    )
                };
                assert_eq!(hit(pill), HitTarget::PaneHeaderAction(role));
                assert_eq!(layout.named_tabs.len(), 3);
                for (i, tab) in layout.named_tabs.iter().enumerate() {
                    assert_eq!(tab.kind.target(), entries[i + 1].0);
                    assert!(tab.rect.bottom() <= pill.y);
                    assert_eq!(hit(tab.rect), HitTarget::SelectSession(tab.kind.target()));
                }
                assert!(layout.named_tabs[1].rect.width >= 36. * scale);
                assert!(layout.panes[0].viewport.y >= pill.bottom());
            }
        }
    }

    #[test]
    fn focused_terminal_uses_two_thirds_of_default_viewport() {
        for (w, h) in [
            (320., 480.),
            (640., 320.),
            (900., 520.),
            (1280., 800.),
            (2560., 1080.),
        ] {
            for s in [1., 2.] {
                let layout = compute_layout(&LayoutInput {
                    window_w: w * s,
                    window_h: h * s,
                    scale_factor: s,
                    zoomed: true,
                    has_top_banner: true,
                    ..default_input()
                });
                assert!(
                    layout.panes[0].viewport.area() >= w * h * s * s * 2. / 3.,
                    "{w}x{h} at {s}"
                );
            }
        }
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
        assert_eq!(pane.outer, Rect::new(0.0, 0.0, 1000.0, 800.0));
        assert!(pane.action_pill.is_some());
    }

    #[test]
    fn sidebar_toggle_and_top_banner_adjust_canvas() {
        let mut input = default_input();
        input.sidebar_override = Some(false);
        input.has_top_banner = true;
        input.scale_factor = 2.0;
        input.window_w = 2480.0;

        let layout = compute_layout(&input);
        assert!(layout.sidebar.is_none());
        let banner = layout.banner.expect("banner should exist");
        // The collapsed disclosure stays above the banner at desktop widths.
        let toggle = layout.sidebar_toggle.unwrap();
        assert_eq!(toggle, Rect::new(0.0, 0.0, 2480.0, 64.0));
        assert_eq!(banner, Rect::new(0.0, toggle.bottom(), 2480.0, 48.0));

        let agent = layout.pane(PaneRole::Agent).unwrap();
        assert_eq!(agent.outer.x, 8.0);
        assert_eq!(agent.outer.y, banner.bottom() + 8.0);
        assert_eq!(agent.outer.bottom(), input.window_h - 8.0);
    }

    #[test]
    fn opening_workspaces_in_a_narrow_focused_terminal_exposes_full_controls() {
        for scale in [1., 2.] {
            let input = LayoutInput {
                window_w: 320. * scale,
                window_h: 568. * scale,
                zoomed: true,
                scale_factor: scale,
                sidebar_override: Some(true),
                ..default_input()
            };
            let layout = compute_layout(&input);
            let sidebar = layout.sidebar.unwrap();
            assert_eq!(sidebar.width, SIDEBAR_LOGICAL_WIDTH * scale);
            assert_eq!(layout.panes[0].outer.right(), sidebar.x);
            let list = crate::sidebar_content::SidebarContent::plain(10, sidebar, 0, scale);
            let settings = settings_button(layout.sidebar_toggle.unwrap(), list.arrows, scale);
            assert_eq!(settings.width, 64. * scale);
            for (rect, target) in [
                (settings, HitTarget::WorkspaceSettings),
                (list.new_workspace, HitTarget::SidebarNewWorkspace),
                (list.new_worktree, HitTarget::SidebarNewWorktree),
            ] {
                assert_eq!(
                    hit_test(
                        &layout,
                        10,
                        scale,
                        input.cell_w,
                        input.cell_h,
                        rect.x + rect.width / 2.,
                        rect.y + rect.height / 2.
                    ),
                    target
                );
            }
        }
    }

    #[test]
    fn sidebar_disclosure_remains_clickable_after_resize_and_reopen() {
        for scale in [1.0, 2.0] {
            let mut input = default_input();
            input.scale_factor = scale;
            for (width, visible) in [(320.0, false), (1920.0, false), (1920.0, true)] {
                input.window_w = width * scale;
                input.sidebar_override = Some(visible);
                let layout = compute_layout(&input);
                let header = layout.sidebar_toggle.expect("visible workspace disclosure");
                assert_eq!(layout.sidebar.is_some(), visible);
                assert_eq!(
                    hit_test(
                        &layout,
                        1,
                        scale,
                        input.cell_w,
                        input.cell_h,
                        header.x + 12.0 * scale,
                        header.y + header.height / 2.0
                    ),
                    HitTarget::ToggleSidebar
                );
                let agent = layout.pane(PaneRole::Agent).unwrap();
                let gutter = if width == 320.0 { 0.0 } else { 4.0 * scale };
                if visible {
                    assert_eq!(agent.outer.y, gutter);
                    assert_eq!(agent.outer.x, header.right() + gutter);
                } else {
                    assert_eq!(agent.outer.y, header.bottom() + gutter);
                    assert_eq!(agent.outer.x, gutter);
                }
            }
        }
    }

    #[test]
    fn short_stacked_workbench_keeps_a_full_terminal_row_in_each_pane() {
        for scale in [1., 1.5, 2.] {
            let input = LayoutInput {
                window_w: 420. * scale,
                window_h: 230. * scale,
                cell_h: 16. * scale,
                scale_factor: scale,
                has_top_banner: true,
                ..default_input()
            };
            let layout = compute_layout(&input);
            assert!(layout.stacked);
            assert_eq!(layout.panes.len(), 3);
            for pane in &layout.panes {
                assert!(pane.viewport.height >= input.cell_h, "{:?}", pane.role);
            }
            assert_eq!(layout.panes.last().unwrap().outer.bottom(), input.window_h);
        }
    }

    #[test]
    fn narrow_windows_stack_all_roles_and_keep_them_reachable_at_both_scales() {
        for scale in [1.0, 2.0] {
            let mut input = default_input();
            input.scale_factor = scale;
            input.window_w = 320.0 * scale;
            input.window_h = 568.0 * scale;
            input.cell_w *= scale;
            input.cell_h *= scale;
            input.has_top_banner = true;
            let layout = compute_layout(&input);
            assert!(layout.sidebar.is_none());
            assert!(layout.stacked);
            assert_eq!(
                hit_test(
                    &layout,
                    0,
                    scale,
                    input.cell_w,
                    input.cell_h,
                    8.0 * scale,
                    8.0 * scale
                ),
                HitTarget::ToggleSidebar
            );
            let mut top = layout.banner.unwrap().bottom();
            for pane in &layout.panes {
                assert_eq!(pane.outer.y, top);
                assert_eq!(pane.outer.width, input.window_w);
                assert_eq!(pane.cols, 38);
                assert!(pane.rows > 0);
                assert!(
                    matches!(hit_test(&layout, 0, scale, input.cell_w, input.cell_h, pane.viewport.x, pane.viewport.y), HitTarget::PaneViewport {role, ..} if role == pane.role)
                );
                top = pane.outer.bottom();
            }
            assert_eq!(top, input.window_h);
            input.sidebar_override = Some(true);
            assert!(compute_layout(&input).sidebar.is_some());
            input.zoomed = true;
            input.focused_role = PaneRole::Runner;
            assert_eq!(compute_layout(&input).panes.len(), 1);
        }
    }

    #[test]
    fn failed_run_button_is_independent_of_card_and_close_at_each_scale() {
        for scale in [1.0, 1.5, 2.0] {
            let input = LayoutInput {
                window_w: 640.0 * scale,
                window_h: 420.0 * scale,
                scale_factor: scale,
                sidebar_override: Some(true),
                ..default_input()
            };
            let mut layout = compute_layout(&input);
            let row = sidebar_workspace_rect(layout.sidebar.unwrap(), 0, scale);
            let button = sidebar_failed_run_rect(row, scale);
            let close = sidebar_workspace_close_rect(row, scale);
            let hit = |layout: &WorkspaceLayout, rect: Rect| {
                hit_test(
                    layout,
                    1,
                    scale,
                    input.cell_w,
                    input.cell_h,
                    rect.x + rect.width / 2.0,
                    rect.y + rect.height / 2.0,
                )
            };
            assert_eq!(hit(&layout, button), HitTarget::SidebarWorkspace(0));
            let workspace = WorkspaceId::new();
            layout.failed_run_buttons.push((workspace, button));
            assert_eq!(hit(&layout, button), HitTarget::SidebarFailedRun(workspace));
            assert_eq!(hit(&layout, close), HitTarget::SidebarWorkspaceClose(0));
            assert_eq!(hit(&layout, row), HitTarget::SidebarWorkspace(0));
        }
    }

    #[test]
    fn sidebar_page_reveals_active_workspace_and_keeps_footer_reachable() {
        for scale in [1.0, 2.0] {
            let mut input = default_input();
            input.sidebar_override = Some(true);
            input.scale_factor = scale;
            input.window_w = 640.0 * scale;
            input.window_h = 420.0 * scale;
            let mut layout = compute_layout(&input);
            let sidebar = layout.sidebar.unwrap();
            let count = 50;
            let first = SidebarList::new(sidebar, count, 0, scale);
            assert_eq!(first.visible, 0..3);
            layout.sidebar_first = first.reveal(49);
            let list = SidebarList::new(sidebar, count, layout.sidebar_first, scale);
            assert_eq!(list.visible, 47..50);
            assert_eq!(list.reveal(48), 47);
            assert_eq!(list.reveal(1), 1);

            let hit = |rect: Rect| {
                hit_test(
                    &layout,
                    count,
                    scale,
                    8.0 * scale,
                    16.0 * scale,
                    rect.x + rect.width / 2.0,
                    rect.y + rect.height / 2.0,
                )
            };
            let bottom_card = sidebar_workspace_rect(sidebar, 2, scale);
            assert_eq!(hit(bottom_card), HitTarget::SidebarWorkspace(49));
            assert_eq!(
                hit(sidebar_workspace_close_rect(bottom_card, scale)),
                HitTarget::SidebarWorkspaceClose(49)
            );
            let workspace_btn = sidebar_new_workspace_rect(sidebar, count, scale);
            let worktree_btn = sidebar_new_worktree_rect(sidebar, count, scale);
            assert!(bottom_card.bottom() <= workspace_btn.y);
            assert!(worktree_btn.bottom() <= sidebar.bottom());
            assert_eq!(hit(workspace_btn), HitTarget::SidebarNewWorkspace);
            assert_eq!(hit(worktree_btn), HitTarget::SidebarNewWorktree);
            let (up, down) = list.arrows.unwrap();
            assert_eq!(hit(up), HitTarget::SidebarPageUp);
            assert_eq!(hit(down), HitTarget::SidebarPageDown);
            assert_eq!(SidebarList::new(sidebar, 2, 47, scale).visible, 0..2);
        }
    }

    #[test]
    fn short_sidebar_keeps_one_condensed_workspace_card_above_footer() {
        let sidebar = Rect::new(0.0, 0.0, 240.0, 140.0);
        let list = SidebarList::new(sidebar, 10, 9, 1.0);
        assert_eq!(list.visible, 9..10);
        let card = sidebar_workspace_rect(sidebar, 0, 1.0);
        assert!(card.height >= HEADER_LOGICAL_HEIGHT && card.height < SIDEBAR_ROW_LOGICAL_HEIGHT);
        assert!(card.bottom() <= sidebar_new_workspace_rect(sidebar, 10, 1.0).y);
    }

    #[test]
    fn cell_grid_size_clamps_to_at_least_one_by_one() {
        assert_eq!(
            cell_grid_size(Rect::new(0.0, 0.0, 0.0, 0.0), 8.0, 16.0),
            (1, 1)
        );
        assert_eq!(
            cell_grid_size(Rect::new(0.0, 0.0, 3.0, 5.0), 8.0, 16.0),
            (1, 1)
        );
        assert_eq!(
            cell_grid_size(Rect::new(0.0, 0.0, 80.0, 160.0), 0.0, -4.0),
            (1, 1)
        );
        assert_eq!(
            cell_grid_size(Rect::new(0.0, 0.0, 85.0, 170.0), 8.0, 16.0),
            (10, 10)
        );

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
            hit_test(
                &layout,
                2,
                input.scale_factor,
                input.cell_w,
                input.cell_h,
                cell_x,
                cell_y
            ),
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

        // Hit sidebar workspace 1, its close button, and "+ New Workspace" button
        let sidebar = layout.sidebar.unwrap();
        let ws1 = sidebar_workspace_rect(sidebar, 1, input.scale_factor);
        let close1 = sidebar_workspace_close_rect(ws1, input.scale_factor);
        assert!(close1.x >= ws1.x && close1.right() <= ws1.right());
        assert!(close1.y >= ws1.y && close1.bottom() <= ws1.bottom());
        assert_eq!(
            hit_test(
                &layout,
                2,
                input.scale_factor,
                input.cell_w,
                input.cell_h,
                close1.x + 2.0,
                close1.y + 2.0,
            ),
            HitTarget::SidebarWorkspaceClose(1)
        );
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
            hit_test(
                &layout,
                2,
                input.scale_factor,
                input.cell_w,
                input.cell_h,
                -5.0,
                -5.0
            ),
            HitTarget::None
        );
    }
}
