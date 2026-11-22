use std::collections::{HashMap, HashSet};

use daedalus_proto::{CellFlags, GridRow, PaneId, PaneRole, PaneStatus, WorkspaceInfo};
use glyphon::{
    Attrs, Buffer, Cache, Color as GlyphonColor, Family, FontSystem, Metrics, Resolution, Shaping,
    Style, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer as GlyphonTextRenderer,
    Viewport, Weight, Wrap,
};

use crate::config::clamp_font_size;
use crate::layout::{
    LayoutMetrics, PaneRect, Rect, WorkspaceLayout, sidebar_new_workspace_rect,
    sidebar_new_worktree_rect, sidebar_workspace_close_rect, sidebar_workspace_rect,
};
use crate::state::{AgentPreset, AppState, SearchState};
use crate::theme::{self, Theme};

use super::{
    CLOSE_MODAL_HEADER_LOGICAL_HEIGHT, action_palette_layout, active_external_detail_preview,
    agent_state_badge, close_modal_cancel_rect, close_modal_compose_rect, close_modal_confirm_rect,
    close_modal_dirty_banner_rect, close_modal_force_rect, close_modal_worktree_rect,
    close_workspace_modal_rect, external_status_color, format_docker_health,
    format_external_source_tag, format_ports_summary, pane_agent_state_pill_rect, picker_item_rect,
    picker_modal_rect, search_bar_rect, sidebar_agent_state_pill_rect,
    sidebar_branch_badge_rect_with_reserve, sidebar_docker_pill_rect, sidebar_external_badge_rect,
    visible_external_statuses, workspace_branch_badge, worktree_input_rect, worktree_modal_rect,
};

pub const FONT_SIZE_LOGICAL: f32 = 14.0;
pub const LINE_HEIGHT_LOGICAL: f32 = 19.0;
const CHROME_FONT_SIZE_LOGICAL: f32 = 12.5;
const CHROME_LINE_HEIGHT_LOGICAL: f32 = 16.0;
const CHROME_BTN_FONT_SIZE_LOGICAL: f32 = 11.5;
const CHROME_SUB_FONT_SIZE_LOGICAL: f32 = 11.0;
const CHROME_SUB_LINE_HEIGHT_LOGICAL: f32 = 14.0;
const CHROME_BADGE_FONT_SIZE_LOGICAL: f32 = 10.0;
const CHROME_BADGE_LINE_HEIGHT_LOGICAL: f32 = 12.0;
const MODAL_TITLE_FONT_SIZE_LOGICAL: f32 = 13.5;
const MODAL_TITLE_LINE_HEIGHT_LOGICAL: f32 = 18.0;

const PREFERRED_MONO_FAMILIES: [&str; 3] = ["JetBrains Mono", "Cascadia Mono", "DejaVu Sans Mono"];

fn sanitize_scale(scale_factor: f32) -> f32 {
    if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    }
}

fn rect_to_bounds(rect: Rect) -> TextBounds {
    TextBounds {
        left: rect.x.floor() as i32,
        top: rect.y.floor() as i32,
        right: rect.right().ceil() as i32,
        bottom: rect.bottom().ceil() as i32,
    }
}

fn select_mono_family(font_system: &FontSystem) -> Option<String> {
    let db = font_system.db();
    for preferred in PREFERRED_MONO_FAMILIES {
        let found = db.faces().any(|face| {
            face.families
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case(preferred))
        });
        if found {
            return Some(preferred.to_string());
        }
    }
    None
}

fn resolve_mono_family(font_system: &FontSystem, requested: Option<&str>) -> Option<String> {
    if let Some(name) = requested.map(str::trim).filter(|s| !s.is_empty()) {
        let matched = font_system.db().faces().find_map(|face| {
            face.families
                .iter()
                .find(|(fam, _)| fam.eq_ignore_ascii_case(name))
                .map(|(fam, _)| fam.clone())
        });
        if matched.is_some() {
            return matched;
        }
        tracing::warn!(
            font_family = %name,
            "requested font_family not found; falling back to default monospace font"
        );
    }
    select_mono_family(font_system)
}

/// Computes the proportional logical line height for a clamped logical `font_size`.
pub fn line_height_for_font_size(font_size_logical: f32) -> f32 {
    let clamped = clamp_font_size(font_size_logical);
    (clamped * (LINE_HEIGHT_LOGICAL / FONT_SIZE_LOGICAL))
        .round()
        .max(clamped + 2.0)
}

/// Left-truncates `input` to at most `max_chars` Unicode scalar values, prefixing `'…'`
/// when truncated so the trailing characters and caret stay visible inside fixed-width inputs.
pub fn truncate_left_with_ellipsis(input: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let char_count = input.chars().count();
    if char_count <= max_chars {
        return input.to_string();
    }
    if max_chars == 1 {
        return "…".to_string();
    }
    let keep = max_chars - 1;
    let skip = char_count - keep;
    let byte_start = input
        .char_indices()
        .nth(skip)
        .map_or(input.len(), |(idx, _)| idx);
    let mut out = String::with_capacity('…'.len_utf8() + (input.len() - byte_start));
    out.push('…');
    out.push_str(&input[byte_start..]);
    out
}

/// Right-truncates `input` to at most `max_chars` Unicode scalar values, appending `'…'`
/// when truncated.
pub fn truncate_right_with_ellipsis(input: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let char_count = input.chars().count();
    if char_count <= max_chars {
        return input.to_string();
    }
    if max_chars == 1 {
        return "…".to_string();
    }
    let keep = max_chars - 1;
    let byte_end = input
        .char_indices()
        .nth(keep)
        .map_or(input.len(), |(idx, _)| idx);
    let mut out = String::with_capacity(byte_end + '…'.len_utf8());
    out.push_str(&input[..byte_end]);
    out.push('…');
    out
}

/// Measures the physical `(cell_w, cell_h)` of the chosen monospace font at
/// `font_size_logical` and `scale_factor`.
pub fn measure_cell_size_for_font(
    font_system: &mut FontSystem,
    mono_family_name: Option<&str>,
    font_size_logical: f32,
    scale_factor: f32,
) -> (f32, f32) {
    let s = sanitize_scale(scale_factor);
    let clamped_logical = clamp_font_size(font_size_logical);
    let font_size = clamped_logical * s;
    let line_height = line_height_for_font_size(clamped_logical) * s;

    let family = mono_family_name.map_or(Family::Monospace, Family::Name);
    let mut buffer = Buffer::new(font_system, Metrics::new(font_size, line_height));
    buffer.set_wrap(Wrap::None);
    buffer.set_size(Some(font_size * 20.0), Some(line_height * 2.0));
    buffer.set_text(
        "MMMMMMMMMM",
        &Attrs::new().family(family),
        Shaping::Advanced,
        None,
    );
    buffer.shape_until_scroll(font_system, false);

    let measured_w = buffer
        .layout_runs()
        .next()
        .and_then(|run| {
            if !run.glyphs.is_empty() && run.line_w > 0.0 {
                Some(run.line_w / run.glyphs.len() as f32)
            } else {
                None
            }
        })
        .filter(|w| w.is_finite() && *w > 0.0)
        .unwrap_or(font_size * 0.6);

    (measured_w.max(1.0), line_height.round().max(1.0))
}

/// Measures the physical `(cell_w, cell_h)` of the chosen monospace font at the default
/// `FONT_SIZE_LOGICAL` and `scale_factor`.
pub fn measure_cell_size(
    font_system: &mut FontSystem,
    mono_family_name: Option<&str>,
    scale_factor: f32,
) -> (f32, f32) {
    measure_cell_size_for_font(
        font_system,
        mono_family_name,
        FONT_SIZE_LOGICAL,
        scale_factor,
    )
}

struct CachedPaneRows {
    rows: Vec<Buffer>,
    scale_factor: f32,
    viewport_width: f32,
    theme: Theme,
}

struct ChromeTextItem {
    buffer: Buffer,
    left: f32,
    top: f32,
    bounds: TextBounds,
    default_color: GlyphonColor,
}

struct ChromeLineSpec<'a> {
    text: &'a str,
    left: f32,
    top: f32,
    max_w: f32,
    bounds: Rect,
    font_size_logical: f32,
    line_height_logical: f32,
    color: theme::Rgba,
    bold: bool,
}

struct VisiblePaneDraw {
    pane_id: PaneId,
    viewport: Rect,
    row_count: usize,
}

/// `glyphon` / `cosmic-text` text renderer (Pass 2 of the two-pass renderer).
pub struct TextRenderer {
    font_system: FontSystem,
    swash_cache: SwashCache,
    _cache: Cache,
    atlas: TextAtlas,
    viewport: Viewport,
    base_renderer: GlyphonTextRenderer,
    modal_renderer: GlyphonTextRenderer,
    mono_family_name: Option<String>,
    scale_factor: f32,
    font_size_logical: f32,
    font_size_px: f32,
    line_height_px: f32,
    cell_w: f32,
    cell_h: f32,
    pane_buffers: HashMap<PaneId, CachedPaneRows>,
    chrome_items: Vec<ChromeTextItem>,
    modal_items: Vec<ChromeTextItem>,
    has_modal_text: bool,
}

impl TextRenderer {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
        scale_factor: f32,
        font_size_logical: f32,
        font_family: Option<&str>,
    ) -> Self {
        let mut font_system = FontSystem::new();
        let swash_cache = SwashCache::new();
        let cache = Cache::new(device);
        let mut viewport = Viewport::new(device, &cache);
        viewport.update(
            queue,
            Resolution {
                width: width.max(1),
                height: height.max(1),
            },
        );
        let mut atlas = TextAtlas::new(device, queue, &cache, format);
        let base_renderer =
            GlyphonTextRenderer::new(&mut atlas, device, wgpu::MultisampleState::default(), None);
        let modal_renderer =
            GlyphonTextRenderer::new(&mut atlas, device, wgpu::MultisampleState::default(), None);

        let mono_family_name = resolve_mono_family(&font_system, font_family);
        let s = sanitize_scale(scale_factor);
        let clamped_logical = clamp_font_size(font_size_logical);
        let line_height_logical = line_height_for_font_size(clamped_logical);
        let (cell_w, cell_h) = measure_cell_size_for_font(
            &mut font_system,
            mono_family_name.as_deref(),
            clamped_logical,
            s,
        );

        Self {
            font_system,
            swash_cache,
            _cache: cache,
            atlas,
            viewport,
            base_renderer,
            modal_renderer,
            mono_family_name,
            scale_factor: s,
            font_size_logical: clamped_logical,
            font_size_px: clamped_logical * s,
            line_height_px: line_height_logical * s,
            cell_w,
            cell_h,
            pane_buffers: HashMap::new(),
            chrome_items: Vec::new(),
            modal_items: Vec::new(),
            has_modal_text: false,
        }
    }

    pub fn cell_size(&self) -> (f32, f32) {
        (self.cell_w, self.cell_h)
    }

    pub fn font_size(&self) -> f32 {
        self.font_size_logical
    }

    /// Updates the logical terminal font size (clamped to `[8.0, 32.0]`), recalculates cell
    /// metrics, invalidates cached row buffers, and returns whether `(cell_w, cell_h)` changed.
    pub fn set_font_size(&mut self, font_size_logical: f32) -> bool {
        let clamped = clamp_font_size(font_size_logical);
        if (clamped - self.font_size_logical).abs() <= 1e-4 {
            return false;
        }
        self.font_size_logical = clamped;
        self.font_size_px = clamped * self.scale_factor;
        self.line_height_px = line_height_for_font_size(clamped) * self.scale_factor;
        let (cw, ch) = measure_cell_size_for_font(
            &mut self.font_system,
            self.mono_family_name.as_deref(),
            clamped,
            self.scale_factor,
        );
        let metrics_changed = (cw - self.cell_w).abs() > 1e-4 || (ch - self.cell_h).abs() > 1e-4;
        self.cell_w = cw;
        self.cell_h = ch;
        self.pane_buffers.clear();
        metrics_changed
    }

    pub fn resize(&mut self, queue: &wgpu::Queue, width: u32, height: u32, scale_factor: f32) {
        self.viewport.update(
            queue,
            Resolution {
                width: width.max(1),
                height: height.max(1),
            },
        );
        let s = sanitize_scale(scale_factor);
        if (s - self.scale_factor).abs() > 1e-4 {
            self.scale_factor = s;
            self.font_size_px = self.font_size_logical * s;
            self.line_height_px = line_height_for_font_size(self.font_size_logical) * s;
            let (cw, ch) = measure_cell_size_for_font(
                &mut self.font_system,
                self.mono_family_name.as_deref(),
                self.font_size_logical,
                s,
            );
            self.cell_w = cw;
            self.cell_h = ch;
            self.pane_buffers.clear();
        }
    }

    /// Updates dirty terminal row buffers, rebuilds UI chrome text buffers, and uploads
    /// text areas to the `glyphon` atlas.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        state: &mut AppState,
        layout: &WorkspaceLayout,
        theme: &Theme,
        screen_w: f32,
        screen_h: f32,
    ) {
        // Prune caches for closed panes.
        self.pane_buffers
            .retain(|pane_id, _| state.panes.contains_key(pane_id));

        // 1. Incrementally update dirty terminal row buffers for visible panes.
        let mut visible_panes = Vec::with_capacity(layout.panes.len());
        for pane_rect in &layout.panes {
            let Some(pane_id) = state.pane_id_for_role(pane_rect.role) else {
                continue;
            };
            let Some(pane_state) = state.panes.get_mut(&pane_id) else {
                continue;
            };

            let dirty_indices = pane_state.take_dirty_rows();
            let visible_rows = pane_state.visible_rows();
            let row_count = visible_rows.len().min(pane_rect.rows as usize);

            let metrics = Metrics::new(self.font_size_px, self.line_height_px);
            let viewport_width = pane_rect.viewport.width.max(1.0);

            let entry = self
                .pane_buffers
                .entry(pane_id)
                .or_insert_with(|| CachedPaneRows {
                    rows: Vec::new(),
                    scale_factor: 0.0,
                    viewport_width: 0.0,
                    theme: *theme,
                });

            let needs_full_rebuild = entry.rows.len() != row_count
                || (entry.scale_factor - self.scale_factor).abs() > 1e-4
                || (entry.viewport_width - viewport_width).abs() > 1.0
                || entry.theme != *theme;

            if entry.rows.len() < row_count {
                while entry.rows.len() < row_count {
                    entry.rows.push(Buffer::new(&mut self.font_system, metrics));
                }
            } else if entry.rows.len() > row_count {
                entry.rows.truncate(row_count);
            }
            entry.scale_factor = self.scale_factor;
            entry.viewport_width = viewport_width;
            entry.theme = *theme;

            let family = self
                .mono_family_name
                .as_deref()
                .map_or(Family::Monospace, Family::Name);

            if needs_full_rebuild {
                for (row_idx, grid_row) in visible_rows.iter().take(row_count).enumerate() {
                    shape_terminal_row(
                        &mut self.font_system,
                        &mut entry.rows[row_idx],
                        grid_row,
                        metrics,
                        viewport_width,
                        family,
                        theme,
                    );
                }
            } else if !dirty_indices.is_empty() {
                let mut seen = HashSet::with_capacity(dirty_indices.len());
                for row_idx in dirty_indices {
                    if row_idx < row_count && seen.insert(row_idx) {
                        shape_terminal_row(
                            &mut self.font_system,
                            &mut entry.rows[row_idx],
                            &visible_rows[row_idx],
                            metrics,
                            viewport_width,
                            family,
                            theme,
                        );
                    }
                }
            }

            visible_panes.push(VisiblePaneDraw {
                pane_id,
                viewport: pane_rect.viewport,
                row_count,
            });
        }

        // 2. Rebuild UI chrome text buffers (sidebar, banner, pane headers, pills).
        self.chrome_items.clear();
        self.build_chrome_items(state, layout, theme);

        // 3. Prepare base text pass (terminal rows + UI chrome).
        let default_fg = theme.text_primary.to_glyphon_color();
        let mut base_areas: Vec<TextArea<'_>> = Vec::new();

        for vp in &visible_panes {
            let Some(cached) = self.pane_buffers.get(&vp.pane_id) else {
                continue;
            };
            let bounds = rect_to_bounds(vp.viewport);
            for (row_idx, buf) in cached.rows.iter().take(vp.row_count).enumerate() {
                base_areas.push(TextArea {
                    buffer: buf,
                    left: vp.viewport.x,
                    top: vp.viewport.y + (row_idx as f32) * self.cell_h,
                    scale: 1.0,
                    bounds,
                    default_color: default_fg,
                    custom_glyphs: &[],
                });
            }
        }

        for item in &self.chrome_items {
            base_areas.push(TextArea {
                buffer: &item.buffer,
                left: item.left,
                top: item.top,
                scale: 1.0,
                bounds: item.bounds,
                default_color: item.default_color,
                custom_glyphs: &[],
            });
        }

        if let Err(err) = self.base_renderer.prepare(
            device,
            queue,
            &mut self.font_system,
            &mut self.atlas,
            &self.viewport,
            base_areas,
            &mut self.swash_cache,
        ) {
            tracing::warn!(%err, "glyphon base text prepare failed");
        }

        // 4. Rebuild & prepare overlay text pass if a modal or search bar is open.
        self.modal_items.clear();
        let modal_open = state.action_palette.open
            || state.picker.open
            || state.worktree_modal.open
            || state.close_modal.open
            || state.search.is_some();
        self.has_modal_text = modal_open;
        if modal_open {
            if state.picker.open || state.action_palette.open {
                self.build_picker_items(state, theme, screen_w, screen_h);
            } else if state.worktree_modal.open {
                self.build_worktree_modal_items(state, theme, screen_w, screen_h);
            } else if state.close_modal.open {
                self.build_close_modal_items(state, theme, screen_w, screen_h);
            } else if let Some(ref search) = state.search {
                self.build_search_bar_items(search, state, layout, theme);
            }
            let modal_areas: Vec<TextArea<'_>> = self
                .modal_items
                .iter()
                .map(|item| TextArea {
                    buffer: &item.buffer,
                    left: item.left,
                    top: item.top,
                    scale: 1.0,
                    bounds: item.bounds,
                    default_color: item.default_color,
                    custom_glyphs: &[],
                })
                .collect();

            if let Err(err) = self.modal_renderer.prepare(
                device,
                queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                modal_areas,
                &mut self.swash_cache,
            ) {
                tracing::warn!(%err, "glyphon modal text prepare failed");
            }
        }
    }

    pub fn render_base<'a>(&'a self, render_pass: &mut wgpu::RenderPass<'a>) {
        if let Err(err) = self
            .base_renderer
            .render(&self.atlas, &self.viewport, render_pass)
        {
            tracing::warn!(%err, "glyphon base text render failed");
        }
    }

    pub fn render_modal<'a>(&'a self, render_pass: &mut wgpu::RenderPass<'a>) {
        if !self.has_modal_text {
            return;
        }
        if let Err(err) = self
            .modal_renderer
            .render(&self.atlas, &self.viewport, render_pass)
        {
            tracing::warn!(%err, "glyphon modal text render failed");
        }
    }

    pub fn trim_atlas(&mut self) {
        self.atlas.trim();
    }

    fn build_chrome_item(&mut self, spec: ChromeLineSpec<'_>) -> Option<ChromeTextItem> {
        if spec.max_w <= 1.0 || spec.bounds.width <= 0.0 || spec.bounds.height <= 0.0 {
            return None;
        }
        let s = self.scale_factor;
        let metrics = Metrics::new(spec.font_size_logical * s, spec.line_height_logical * s);
        let family = self
            .mono_family_name
            .as_deref()
            .map_or(Family::Monospace, Family::Name);
        let mut attrs = Attrs::new()
            .family(family)
            .color(spec.color.to_glyphon_color());
        if spec.bold {
            attrs = attrs.weight(Weight::BOLD);
        }

        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        buffer.set_wrap(Wrap::None);
        buffer.set_size(
            Some(spec.max_w.max(1.0)),
            Some((spec.line_height_logical * s) * 1.5),
        );
        buffer.set_text(spec.text, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.font_system, false);

        Some(ChromeTextItem {
            buffer,
            left: spec.left,
            top: spec.top,
            bounds: rect_to_bounds(spec.bounds),
            default_color: spec.color.to_glyphon_color(),
        })
    }

    fn push_chrome_line(&mut self, spec: ChromeLineSpec<'_>) {
        if let Some(item) = self.build_chrome_item(spec) {
            self.chrome_items.push(item);
        }
    }

    fn push_modal_line(&mut self, spec: ChromeLineSpec<'_>) {
        if let Some(item) = self.build_chrome_item(spec) {
            self.modal_items.push(item);
        }
    }

    fn build_chrome_items(&mut self, state: &AppState, layout: &WorkspaceLayout, theme: &Theme) {
        let s = self.scale_factor;

        // Sidebar text
        if let Some(sidebar) = layout.sidebar {
            let m = LayoutMetrics::for_scale(s);
            let header_rect =
                Rect::new(sidebar.x, sidebar.y, sidebar.width, m.sidebar_header_height);
            let header = state
                .remote_host
                .as_ref()
                .map(|host| format!("[REMOTE: {host}]"));
            self.push_chrome_line(ChromeLineSpec {
                text: header.as_deref().unwrap_or("WORKSPACES"),
                left: header_rect.x + m.sidebar_pad + 4.0 * s,
                top: header_rect.y
                    + (header_rect.height - CHROME_SUB_LINE_HEIGHT_LOGICAL * s) * 0.5,
                max_w: header_rect.width - 2.0 * m.sidebar_pad,
                bounds: header_rect,
                font_size_logical: CHROME_SUB_FONT_SIZE_LOGICAL,
                line_height_logical: CHROME_SUB_LINE_HEIGHT_LOGICAL,
                color: theme.text_muted,
                bold: true,
            });

            for (idx, ws) in state.workspaces.iter().enumerate() {
                let row_rect = sidebar_workspace_rect(sidebar, idx, s);
                if row_rect.y > sidebar.bottom() {
                    break;
                }
                self.build_sidebar_workspace_text(ws, row_rect, state, theme);
            }

            let new_btn = sidebar_new_workspace_rect(sidebar, state.workspaces.len(), s);
            if new_btn.y < sidebar.bottom() {
                self.push_chrome_line(ChromeLineSpec {
                    text: "+ Workspace",
                    left: new_btn.x + 8.0 * s,
                    top: new_btn.y + (new_btn.height - CHROME_LINE_HEIGHT_LOGICAL * s) * 0.5,
                    max_w: new_btn.width - 12.0 * s,
                    bounds: new_btn,
                    font_size_logical: CHROME_BTN_FONT_SIZE_LOGICAL,
                    line_height_logical: CHROME_LINE_HEIGHT_LOGICAL,
                    color: theme.text_primary,
                    bold: false,
                });
            }

            let new_wt_btn = sidebar_new_worktree_rect(sidebar, state.workspaces.len(), s);
            if new_wt_btn.y < sidebar.bottom() {
                self.push_chrome_line(ChromeLineSpec {
                    text: "+ Worktree",
                    left: new_wt_btn.x + 8.0 * s,
                    top: new_wt_btn.y + (new_wt_btn.height - CHROME_LINE_HEIGHT_LOGICAL * s) * 0.5,
                    max_w: new_wt_btn.width - 12.0 * s,
                    bounds: new_wt_btn,
                    font_size_logical: CHROME_BTN_FONT_SIZE_LOGICAL,
                    line_height_logical: CHROME_LINE_HEIGHT_LOGICAL,
                    color: theme.border_focused,
                    bold: true,
                });
            }
        }

        // Top banner (disconnected or toast)
        if let Some(banner) = layout.banner
            && let (_, Some((msg, color))) = super::banner_appearance(state, theme)
            && !msg.is_empty()
        {
            self.push_chrome_line(ChromeLineSpec {
                text: &msg,
                left: banner.x + 10.0 * s,
                top: banner.y + (banner.height - CHROME_LINE_HEIGHT_LOGICAL * s) * 0.5,
                max_w: banner.width - 20.0 * s,
                bounds: banner,
                font_size_logical: CHROME_FONT_SIZE_LOGICAL,
                line_height_logical: CHROME_LINE_HEIGHT_LOGICAL,
                color,
                bold: true,
            });
        }

        // Pane headers & action pills
        for pane_rect in &layout.panes {
            self.build_pane_header_text(pane_rect, state, theme);
        }
    }

    fn build_sidebar_workspace_text(
        &mut self,
        ws: &WorkspaceInfo,
        row_rect: Rect,
        state: &AppState,
        theme: &Theme,
    ) {
        let s = self.scale_factor;
        let is_active = state.active_workspace_id == Some(ws.id);
        let left_x = row_rect.x + 22.0 * s;

        // Runner status pill on right side of line 1
        let runner_summary = ws.panes.iter().rev().find(|p| p.role == PaneRole::Runner);
        let pill_text = runner_summary.and_then(|r| {
            if r.has_failure_trace {
                Some((
                    format!("✗ {}", r.last_exit_code.unwrap_or(1)),
                    theme.status_err,
                ))
            } else if r.last_exit_code == Some(0) {
                Some(("✓ 0".to_string(), theme.status_ok))
            } else {
                None
            }
        });

        let pill_reserve = if pill_text.is_some() {
            74.0 * s
        } else {
            24.0 * s
        };
        let name_max_w = (row_rect.right() - left_x - pill_reserve).max(1.0);
        let title_color = if is_active {
            theme.text_primary
        } else {
            theme.text_primary.scale_luminance(0.85)
        };

        self.push_chrome_line(ChromeLineSpec {
            text: &ws.name,
            left: left_x,
            top: row_rect.y + 6.0 * s,
            max_w: name_max_w,
            bounds: Rect::new(left_x, row_rect.y, name_max_w, row_rect.height),
            font_size_logical: CHROME_FONT_SIZE_LOGICAL,
            line_height_logical: CHROME_LINE_HEIGHT_LOGICAL,
            color: title_color,
            bold: is_active,
        });

        // Compact `×` close button in top-right corner
        let close_rect = sidebar_workspace_close_rect(row_rect, s);
        self.push_chrome_line(ChromeLineSpec {
            text: "×",
            left: close_rect.x + 4.0 * s,
            top: close_rect.y + 1.0 * s,
            max_w: (close_rect.width - 4.0 * s).max(1.0),
            bounds: close_rect,
            font_size_logical: CHROME_BTN_FONT_SIZE_LOGICAL,
            line_height_logical: CHROME_SUB_LINE_HEIGHT_LOGICAL,
            color: theme.text_muted,
            bold: true,
        });

        if let Some((label, color)) = pill_text {
            let pill_x = row_rect.right() - 66.0 * s;
            self.push_chrome_line(ChromeLineSpec {
                text: &label,
                left: pill_x,
                top: row_rect.y + 7.0 * s,
                max_w: 44.0 * s,
                bounds: row_rect,
                font_size_logical: CHROME_SUB_FONT_SIZE_LOGICAL,
                line_height_logical: CHROME_SUB_LINE_HEIGHT_LOGICAL,
                color,
                bold: true,
            });
        }

        // Line 2: Git branch / worktree slot badge + compact Agent state badge on right
        let agent_pill_reserve = if let Some(ast) = state.workspace_agent_state(ws) {
            let (label, color) = agent_state_badge(ast, theme);
            let pill_rect = sidebar_agent_state_pill_rect(row_rect, label, s);
            self.push_chrome_line(ChromeLineSpec {
                text: label,
                left: pill_rect.x + 5.0 * s,
                top: pill_rect.y + 1.5 * s,
                max_w: (pill_rect.width - 8.0 * s).max(1.0),
                bounds: pill_rect,
                font_size_logical: CHROME_BADGE_FONT_SIZE_LOGICAL,
                line_height_logical: CHROME_BADGE_LINE_HEIGHT_LOGICAL,
                color,
                bold: true,
            });
            pill_rect.width + 6.0 * s
        } else {
            0.0
        };

        if let Some((branch_label, is_wt)) = workspace_branch_badge(ws) {
            let badge_rect = sidebar_branch_badge_rect_with_reserve(
                row_rect,
                &branch_label,
                agent_pill_reserve,
                s,
            );
            self.push_chrome_line(ChromeLineSpec {
                text: &branch_label,
                left: badge_rect.x + 5.0 * s,
                top: badge_rect.y + 1.0 * s,
                max_w: (badge_rect.width - 8.0 * s).max(1.0),
                bounds: badge_rect,
                font_size_logical: CHROME_BADGE_FONT_SIZE_LOGICAL,
                line_height_logical: CHROME_BADGE_LINE_HEIGHT_LOGICAL,
                color: if is_wt {
                    theme.border_focused
                } else {
                    theme.text_muted
                },
                bold: is_wt,
            });
        } else {
            let dir_name = ws
                .root_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(".");
            let agent_status = ws
                .panes
                .iter()
                .rev()
                .find(|p| p.role == PaneRole::Agent)
                .map(|p| match p.status {
                    PaneStatus::Starting => "starting",
                    PaneStatus::Idle => "idle",
                    PaneStatus::Busy => "busy",
                    PaneStatus::Exited { .. } => "exited",
                })
                .unwrap_or("none");
            let subtitle = format!("{dir_name} · agent:{agent_status}");
            let sub_max_w = (row_rect.right() - left_x - 8.0 * s - agent_pill_reserve).max(1.0);
            self.push_chrome_line(ChromeLineSpec {
                text: &subtitle,
                left: left_x,
                top: row_rect.y + 24.0 * s,
                max_w: sub_max_w,
                bounds: Rect::new(left_x, row_rect.y + 22.0 * s, sub_max_w, 18.0 * s),
                font_size_logical: CHROME_SUB_FONT_SIZE_LOGICAL,
                line_height_logical: CHROME_SUB_LINE_HEIGHT_LOGICAL,
                color: theme.text_muted,
                bold: false,
            });
        }

        // Line 3: Bound ports summary + Docker container health
        let docker_health = format_docker_health(&ws.containers);
        let docker_reserve = if docker_health.is_some() {
            72.0 * s
        } else {
            8.0 * s
        };
        let (ports_label, has_ports) = format_ports_summary(&ws.bound_ports);
        let ports_left = row_rect.x + 10.0 * s;
        let ports_max_w = (row_rect.right() - ports_left - docker_reserve).max(1.0);
        self.push_chrome_line(ChromeLineSpec {
            text: &ports_label,
            left: ports_left,
            top: row_rect.y + 42.0 * s,
            max_w: ports_max_w,
            bounds: Rect::new(ports_left, row_rect.y + 40.0 * s, ports_max_w, 16.0 * s),
            font_size_logical: CHROME_BADGE_FONT_SIZE_LOGICAL,
            line_height_logical: CHROME_BADGE_LINE_HEIGHT_LOGICAL,
            color: if has_ports {
                theme.status_ok
            } else {
                theme.text_muted
            },
            bold: has_ports,
        });

        if let Some((docker_label, is_healthy)) = docker_health {
            let docker_rect = sidebar_docker_pill_rect(row_rect, s);
            self.push_chrome_line(ChromeLineSpec {
                text: &docker_label,
                left: docker_rect.x + 5.0 * s,
                top: docker_rect.y + 1.5 * s,
                max_w: (docker_rect.width - 8.0 * s).max(1.0),
                bounds: docker_rect,
                font_size_logical: CHROME_BADGE_FONT_SIZE_LOGICAL,
                line_height_logical: CHROME_BADGE_LINE_HEIGHT_LOGICAL,
                color: if is_healthy {
                    theme.status_ok
                } else {
                    theme.status_err
                },
                bold: true,
            });
        }

        // Line 4: Up to 2 color-coded external status badges with compact source tags
        let ext_statuses = visible_external_statuses(ws);
        for (badge_idx, ext) in ext_statuses.iter().enumerate() {
            let badge_rect =
                sidebar_external_badge_rect(row_rect, badge_idx, ext_statuses.len(), s);
            let badge_text = format!("{} {}", format_external_source_tag(&ext.source), ext.label);
            let color = external_status_color(ext.level, theme);
            self.push_chrome_line(ChromeLineSpec {
                text: &badge_text,
                left: badge_rect.x + 5.0 * s,
                top: badge_rect.y + 2.0 * s,
                max_w: (badge_rect.width - 8.0 * s).max(1.0),
                bounds: badge_rect,
                font_size_logical: CHROME_BADGE_FONT_SIZE_LOGICAL,
                line_height_logical: CHROME_BADGE_LINE_HEIGHT_LOGICAL,
                color,
                bold: true,
            });
        }

        // Line 5 (active workspace only): truncated external status detail preview
        if is_active && let Some(preview) = active_external_detail_preview(ws) {
            let detail_left = row_rect.x + 10.0 * s;
            let detail_max_w = (row_rect.right() - detail_left - 8.0 * s).max(1.0);
            let approx_char_w = (CHROME_BADGE_FONT_SIZE_LOGICAL * s * 0.62).max(1.0);
            let max_chars = ((detail_max_w / approx_char_w).floor() as usize).max(8);
            let truncated = truncate_right_with_ellipsis(&preview, max_chars);
            self.push_chrome_line(ChromeLineSpec {
                text: &truncated,
                left: detail_left,
                top: row_rect.y + 79.0 * s,
                max_w: detail_max_w,
                bounds: Rect::new(detail_left, row_rect.y + 77.0 * s, detail_max_w, 17.0 * s),
                font_size_logical: CHROME_BADGE_FONT_SIZE_LOGICAL,
                line_height_logical: CHROME_BADGE_LINE_HEIGHT_LOGICAL,
                color: theme.text_muted,
                bold: false,
            });
        }
    }

    fn build_pane_header_text(&mut self, pane_rect: &PaneRect, state: &AppState, theme: &Theme) {
        let s = self.scale_factor;
        let header = pane_rect.header;
        let is_focused = pane_rect.role == state.focused_role;
        let pane_state = state.pane_for_role(pane_rect.role);

        let role_label = match pane_rect.role {
            PaneRole::Agent => {
                let preset_name = pane_state
                    .and_then(|p| p.agent_preset)
                    .map(|p| p.short_name())
                    .unwrap_or("agent");
                format!("AGENT [{preset_name}]")
            }
            PaneRole::Service => "SERVICE".to_string(),
            PaneRole::Runner => "RUNNER".to_string(),
        };

        let cwd_suffix = pane_state
            .and_then(|p| p.summary.cwd.as_ref())
            .and_then(|cwd| cwd.file_name())
            .and_then(|n| n.to_str())
            .map(|name| format!("  {name}"))
            .unwrap_or_default();

        let scroll_suffix = pane_state
            .filter(|p| p.scroll_offset > 0)
            .map(|p| format!("  [SCROLL -{}]", p.scroll_offset))
            .unwrap_or_default();

        let agent_badge = if pane_rect.role == PaneRole::Agent {
            pane_state
                .and_then(|p| p.summary.agent_state)
                .and_then(|ast| {
                    let (label, color) = agent_state_badge(ast, theme);
                    pane_agent_state_pill_rect(pane_rect, label, s).map(|rect| (rect, label, color))
                })
        } else {
            None
        };

        let title = format!("{role_label}{cwd_suffix}{scroll_suffix}");
        let text_left = header.x + 22.0 * s;
        let default_right = pane_rect
            .action_pill
            .map_or(header.right() - 8.0 * s, |pill| pill.x - 8.0 * s);
        let text_right = agent_badge.map_or(default_right, |(rect, _, _)| rect.x - 8.0 * s);
        let text_w = (text_right - text_left).max(1.0);
        let title_bounds = Rect::new(text_left, header.y, text_w, header.height);

        self.push_chrome_line(ChromeLineSpec {
            text: &title,
            left: text_left,
            top: header.y + (header.height - CHROME_LINE_HEIGHT_LOGICAL * s) * 0.5,
            max_w: text_w,
            bounds: title_bounds,
            font_size_logical: CHROME_FONT_SIZE_LOGICAL,
            line_height_logical: CHROME_LINE_HEIGHT_LOGICAL,
            color: if is_focused {
                theme.text_primary
            } else {
                theme.text_muted
            },
            bold: is_focused,
        });

        if let Some((pill, label, color)) = agent_badge {
            self.push_chrome_line(ChromeLineSpec {
                text: label,
                left: pill.x + 6.0 * s,
                top: pill.y + (pill.height - CHROME_BADGE_LINE_HEIGHT_LOGICAL * s) * 0.5,
                max_w: (pill.width - 10.0 * s).max(1.0),
                bounds: pill,
                font_size_logical: CHROME_BADGE_FONT_SIZE_LOGICAL,
                line_height_logical: CHROME_BADGE_LINE_HEIGHT_LOGICAL,
                color,
                bold: true,
            });
        }

        if let Some(pill) = pane_rect.action_pill {
            let (pill_label, pill_color) = match pane_rect.role {
                PaneRole::Agent => {
                    let preset_name = pane_state
                        .and_then(|p| p.agent_preset)
                        .map(|p| p.short_name())
                        .unwrap_or("preset");
                    (format!("Preset: {preset_name}"), theme.border_focused)
                }
                PaneRole::Runner => (
                    if cfg!(target_os = "macos") {
                        "Send Error → Agent ⌘⇧E".to_string()
                    } else {
                        "Send Error → Agent ^⇧E".to_string()
                    },
                    theme.status_err,
                ),
                PaneRole::Service => (String::new(), theme.text_primary),
            };
            if !pill_label.is_empty() {
                self.push_chrome_line(ChromeLineSpec {
                    text: &pill_label,
                    left: pill.x + 8.0 * s,
                    top: pill.y + (pill.height - CHROME_SUB_LINE_HEIGHT_LOGICAL * s) * 0.5,
                    max_w: (pill.width - 12.0 * s).max(1.0),
                    bounds: pill,
                    font_size_logical: CHROME_SUB_FONT_SIZE_LOGICAL,
                    line_height_logical: CHROME_SUB_LINE_HEIGHT_LOGICAL,
                    color: pill_color,
                    bold: true,
                });
            }
        }
    }

    fn build_picker_items(
        &mut self,
        state: &AppState,
        theme: &Theme,
        screen_w: f32,
        screen_h: f32,
    ) {
        let s = self.scale_factor;
        let palette = state.action_palette.open;
        let actions = state.active_actions();
        let (modal, visible) = if palette {
            action_palette_layout(
                screen_w,
                screen_h,
                s,
                actions.len(),
                state.action_palette.selected_index,
            )
        } else {
            (
                picker_modal_rect(screen_w, screen_h, s),
                0..AgentPreset::ALL.len(),
            )
        };

        self.push_modal_line(ChromeLineSpec {
            text: if palette {
                "Action Palette  (↑/↓ Enter · Esc)"
            } else {
                "Select Agent Preset  (↑/↓ Enter · Esc)"
            },
            left: modal.x + 16.0 * s,
            top: modal.y + 12.0 * s,
            max_w: (modal.width - 32.0 * s).max(1.0),
            bounds: modal,
            font_size_logical: MODAL_TITLE_FONT_SIZE_LOGICAL,
            line_height_logical: MODAL_TITLE_LINE_HEIGHT_LOGICAL,
            color: theme.text_primary,
            bold: true,
        });

        if visible.is_empty() {
            let row = picker_item_rect(modal, 0, s);
            self.push_modal_line(ChromeLineSpec {
                text: "No actions configured for this workspace",
                left: row.x + 10.0 * s,
                top: row.y + 6.0 * s,
                max_w: (row.width - 20.0 * s).max(1.0),
                bounds: row,
                font_size_logical: CHROME_FONT_SIZE_LOGICAL,
                line_height_logical: CHROME_LINE_HEIGHT_LOGICAL,
                color: theme.text_muted,
                bold: false,
            });
        }
        for (row, idx) in visible.enumerate() {
            let item_rect = picker_item_rect(modal, row, s);
            let (selected, label) = if palette {
                let action = &actions[idx];
                let shortcut = action.shortcut.as_deref().unwrap_or("");
                (
                    idx == state.action_palette.selected_index,
                    format!("{} · {:?} {}", action.label, action.target, shortcut),
                )
            } else {
                (
                    idx == state.picker.selected_index,
                    AgentPreset::ALL[idx].label().to_owned(),
                )
            };
            let prefix = if selected { "▸ " } else { "  " };
            let label = format!("{prefix}{label}");
            self.push_modal_line(ChromeLineSpec {
                text: &label,
                left: item_rect.x + 10.0 * s,
                top: item_rect.y + (item_rect.height - CHROME_LINE_HEIGHT_LOGICAL * s) * 0.5,
                max_w: (item_rect.width - 20.0 * s).max(1.0),
                bounds: item_rect,
                font_size_logical: CHROME_FONT_SIZE_LOGICAL,
                line_height_logical: CHROME_LINE_HEIGHT_LOGICAL,
                color: if selected {
                    theme.text_primary
                } else {
                    theme.text_muted
                },
                bold: selected,
            });
        }
    }

    fn build_worktree_modal_items(
        &mut self,
        state: &AppState,
        theme: &Theme,
        screen_w: f32,
        screen_h: f32,
    ) {
        let s = self.scale_factor;
        let modal = worktree_modal_rect(screen_w, screen_h, s);
        let input_rect = worktree_input_rect(modal, s);

        self.push_modal_line(ChromeLineSpec {
            text: "New Git Worktree",
            left: modal.x + 14.0 * s,
            top: modal.y + 10.0 * s,
            max_w: (modal.width - 28.0 * s).max(1.0),
            bounds: modal,
            font_size_logical: MODAL_TITLE_FONT_SIZE_LOGICAL,
            line_height_logical: MODAL_TITLE_LINE_HEIGHT_LOGICAL,
            color: theme.text_primary,
            bold: true,
        });

        let text_top = input_rect.y + (input_rect.height - CHROME_LINE_HEIGHT_LOGICAL * s) * 0.5;
        if state.worktree_modal.branch_input.is_empty() {
            self.push_modal_line(ChromeLineSpec {
                text: "|",
                left: input_rect.x + 10.0 * s,
                top: text_top,
                max_w: 12.0 * s,
                bounds: input_rect,
                font_size_logical: CHROME_FONT_SIZE_LOGICAL,
                line_height_logical: CHROME_LINE_HEIGHT_LOGICAL,
                color: theme.border_focused,
                bold: true,
            });
            self.push_modal_line(ChromeLineSpec {
                text: "feat/...",
                left: input_rect.x + 20.0 * s,
                top: text_top,
                max_w: (input_rect.width - 28.0 * s).max(1.0),
                bounds: input_rect,
                font_size_logical: CHROME_FONT_SIZE_LOGICAL,
                line_height_logical: CHROME_LINE_HEIGHT_LOGICAL,
                color: theme.text_muted,
                bold: false,
            });
        } else {
            let usable_w = (input_rect.width - 24.0 * s).max(16.0);
            let approx_char_w = (CHROME_FONT_SIZE_LOGICAL * s * 0.62).max(1.0);
            let max_chars = ((usable_w / approx_char_w).floor() as usize)
                .saturating_sub(1)
                .max(4);
            let truncated =
                truncate_left_with_ellipsis(&state.worktree_modal.branch_input, max_chars);
            let typed = format!("{truncated}|");
            self.push_modal_line(ChromeLineSpec {
                text: &typed,
                left: input_rect.x + 10.0 * s,
                top: text_top,
                max_w: (input_rect.width - 20.0 * s).max(1.0),
                bounds: input_rect,
                font_size_logical: CHROME_FONT_SIZE_LOGICAL,
                line_height_logical: CHROME_LINE_HEIGHT_LOGICAL,
                color: theme.text_primary,
                bold: false,
            });
        }

        self.push_modal_line(ChromeLineSpec {
            text: "Enter to create • Esc to cancel",
            left: modal.x + 14.0 * s,
            top: input_rect.bottom() + 12.0 * s,
            max_w: (modal.width - 28.0 * s).max(1.0),
            bounds: modal,
            font_size_logical: CHROME_SUB_FONT_SIZE_LOGICAL,
            line_height_logical: CHROME_SUB_LINE_HEIGHT_LOGICAL,
            color: theme.text_muted,
            bold: false,
        });
    }

    fn build_close_modal_items(
        &mut self,
        state: &AppState,
        theme: &Theme,
        screen_w: f32,
        screen_h: f32,
    ) {
        let s = self.scale_factor;
        let cm = &state.close_modal;
        let modal = close_workspace_modal_rect(screen_w, screen_h, s);

        self.push_modal_line(ChromeLineSpec {
            text: "Close Workspace",
            left: modal.x + 16.0 * s,
            top: modal.y + 10.0 * s,
            max_w: (modal.width - 32.0 * s).max(1.0),
            bounds: modal,
            font_size_logical: MODAL_TITLE_FONT_SIZE_LOGICAL,
            line_height_logical: MODAL_TITLE_LINE_HEIGHT_LOGICAL,
            color: theme.text_primary,
            bold: true,
        });

        let summary_line = match &cm.branch {
            Some(branch) => format!("{}  ({branch})", cm.workspace_name),
            None => cm.workspace_name.clone(),
        };
        self.push_modal_line(ChromeLineSpec {
            text: &summary_line,
            left: modal.x + 16.0 * s,
            top: modal.y + (CLOSE_MODAL_HEADER_LOGICAL_HEIGHT + 8.0) * s,
            max_w: (modal.width - 32.0 * s).max(1.0),
            bounds: modal,
            font_size_logical: CHROME_FONT_SIZE_LOGICAL,
            line_height_logical: CHROME_LINE_HEIGHT_LOGICAL,
            color: theme.text_primary,
            bold: true,
        });

        if cm.is_dirty {
            let warn_rect = close_modal_dirty_banner_rect(modal, s);
            self.push_modal_line(ChromeLineSpec {
                text: "⚠ Worktree has uncommitted changes",
                left: warn_rect.x + 10.0 * s,
                top: warn_rect.y + (warn_rect.height - CHROME_SUB_LINE_HEIGHT_LOGICAL * s) * 0.5,
                max_w: (warn_rect.width - 20.0 * s).max(1.0),
                bounds: warn_rect,
                font_size_logical: CHROME_SUB_FONT_SIZE_LOGICAL,
                line_height_logical: CHROME_SUB_LINE_HEIGHT_LOGICAL,
                color: theme.status_busy,
                bold: true,
            });
        }

        let mut push_checkbox_text =
            |rect: Rect, checked: bool, label: &str, color: theme::Rgba| {
                let mark = if checked { "[x]" } else { "[ ]" };
                let line = format!("{mark} {label}");
                self.push_modal_line(ChromeLineSpec {
                    text: &line,
                    left: rect.x + 10.0 * s,
                    top: rect.y + (rect.height - CHROME_LINE_HEIGHT_LOGICAL * s) * 0.5,
                    max_w: (rect.width - 20.0 * s).max(1.0),
                    bounds: rect,
                    font_size_logical: CHROME_FONT_SIZE_LOGICAL,
                    line_height_logical: CHROME_LINE_HEIGHT_LOGICAL,
                    color,
                    bold: checked,
                });
            };

        let compose_rect = close_modal_compose_rect(modal, cm, s);
        push_checkbox_text(
            compose_rect,
            cm.compose_down,
            "Stop Docker Compose (c)",
            if cm.compose_down {
                theme.text_primary
            } else {
                theme.text_muted
            },
        );

        if let Some(wt_rect) = close_modal_worktree_rect(modal, cm, s) {
            push_checkbox_text(
                wt_rect,
                cm.remove_worktree,
                "Remove git worktree (w)",
                if cm.remove_worktree {
                    theme.status_busy
                } else {
                    theme.text_muted
                },
            );
        }

        if let Some(force_rect) = close_modal_force_rect(modal, cm, s) {
            push_checkbox_text(
                force_rect,
                cm.force_remove_worktree,
                "Force remove dirty worktree (f)",
                if cm.force_remove_worktree {
                    theme.status_err
                } else {
                    theme.text_muted
                },
            );
        }

        let cancel_rect = close_modal_cancel_rect(modal, s);
        self.push_modal_line(ChromeLineSpec {
            text: "Cancel",
            left: cancel_rect.x + 12.0 * s,
            top: cancel_rect.y + (cancel_rect.height - CHROME_LINE_HEIGHT_LOGICAL * s) * 0.5,
            max_w: (cancel_rect.width - 24.0 * s).max(1.0),
            bounds: cancel_rect,
            font_size_logical: CHROME_FONT_SIZE_LOGICAL,
            line_height_logical: CHROME_LINE_HEIGHT_LOGICAL,
            color: theme.text_primary,
            bold: false,
        });

        let confirm_rect = close_modal_confirm_rect(modal, s);
        self.push_modal_line(ChromeLineSpec {
            text: "Confirm Close",
            left: confirm_rect.x + 12.0 * s,
            top: confirm_rect.y + (confirm_rect.height - CHROME_LINE_HEIGHT_LOGICAL * s) * 0.5,
            max_w: (confirm_rect.width - 24.0 * s).max(1.0),
            bounds: confirm_rect,
            font_size_logical: CHROME_FONT_SIZE_LOGICAL,
            line_height_logical: CHROME_LINE_HEIGHT_LOGICAL,
            color: if cm.remove_worktree {
                theme.status_err
            } else {
                theme.border_focused
            },
            bold: true,
        });

        self.push_modal_line(ChromeLineSpec {
            text: "Enter: Confirm · Esc: Cancel",
            left: modal.x + 16.0 * s,
            top: modal.bottom() - 20.0 * s,
            max_w: (modal.width - 32.0 * s).max(1.0),
            bounds: modal,
            font_size_logical: CHROME_SUB_FONT_SIZE_LOGICAL,
            line_height_logical: CHROME_SUB_LINE_HEIGHT_LOGICAL,
            color: theme.text_muted,
            bold: false,
        });
    }

    fn build_search_bar_items(
        &mut self,
        search: &SearchState,
        state: &AppState,
        layout: &WorkspaceLayout,
        theme: &Theme,
    ) {
        let Some(pane_rect) = layout
            .panes
            .iter()
            .find(|p| state.pane_id_for_role(p.role) == Some(search.pane_id))
            .or_else(|| layout.pane(state.focused_role))
        else {
            return;
        };
        let s = self.scale_factor;
        let bar = search_bar_rect(pane_rect, s);
        let (cur, total) = search.display_counter();
        let counter = format!("({cur}/{total})");
        let counter_w = ((counter.chars().count() as f32)
            * (CHROME_SUB_FONT_SIZE_LOGICAL * s * 0.65)
            + 12.0 * s)
            .min(bar.width * 0.45);
        let query_max_w = (bar.width - 16.0 * s - counter_w).max(24.0);
        let approx_char_w = (CHROME_FONT_SIZE_LOGICAL * s * 0.62).max(1.0);
        let prefix = "Search: ";
        let max_query_chars = ((query_max_w / approx_char_w).floor() as usize)
            .saturating_sub(prefix.chars().count() + 1)
            .max(3);
        let display_query = truncate_left_with_ellipsis(&search.query, max_query_chars);
        let query_line = format!("{prefix}{display_query}|");

        let query_bounds = Rect::new(bar.x + 8.0 * s, bar.y, query_max_w, bar.height);
        self.push_modal_line(ChromeLineSpec {
            text: &query_line,
            left: bar.x + 8.0 * s,
            top: bar.y + (bar.height - CHROME_LINE_HEIGHT_LOGICAL * s) * 0.5,
            max_w: query_max_w,
            bounds: query_bounds,
            font_size_logical: CHROME_FONT_SIZE_LOGICAL,
            line_height_logical: CHROME_LINE_HEIGHT_LOGICAL,
            color: theme.text_primary,
            bold: false,
        });

        let counter_color = if !search.query.is_empty() && total == 0 {
            theme.status_err
        } else {
            theme.text_muted
        };
        let counter_left = (bar.right() - counter_w + 4.0 * s).max(query_bounds.right() + 4.0 * s);
        let counter_bounds = Rect::new(
            counter_left,
            bar.y,
            (bar.right() - counter_left - 6.0 * s).max(1.0),
            bar.height,
        );
        self.push_modal_line(ChromeLineSpec {
            text: &counter,
            left: counter_left,
            top: bar.y + (bar.height - CHROME_SUB_LINE_HEIGHT_LOGICAL * s) * 0.5,
            max_w: counter_bounds.width,
            bounds: counter_bounds,
            font_size_logical: CHROME_SUB_FONT_SIZE_LOGICAL,
            line_height_logical: CHROME_SUB_LINE_HEIGHT_LOGICAL,
            color: counter_color,
            bold: false,
        });
    }
}

/// Coalesced styled text run within a single terminal row.
#[derive(Debug, PartialEq, Eq)]
struct RowSpan {
    text: String,
    color: GlyphonColor,
    bold: bool,
    italic: bool,
}

fn build_row_spans(row: &GridRow, theme: &Theme) -> Vec<RowSpan> {
    let mut spans: Vec<RowSpan> = Vec::new();
    for cell in &row.cells {
        if cell.width == 0 {
            continue;
        }
        let (fg_rgba, _bg) = theme.resolve_cell_colors(cell.fg, cell.bg, cell.flags);
        let color = fg_rgba.to_glyphon_color();
        let bold = cell.flags.contains(CellFlags::BOLD);
        let italic = cell.flags.contains(CellFlags::ITALIC);
        let text = if cell.text.is_empty() {
            " "
        } else {
            cell.text.as_str()
        };

        if let Some(last) = spans.last_mut()
            && last.color == color
            && last.bold == bold
            && last.italic == italic
        {
            last.text.push_str(text);
        } else {
            spans.push(RowSpan {
                text: text.to_string(),
                color,
                bold,
                italic,
            });
        }
    }
    spans
}

fn shape_terminal_row(
    font_system: &mut FontSystem,
    buffer: &mut Buffer,
    row: &GridRow,
    metrics: Metrics,
    viewport_width: f32,
    family: Family<'_>,
    theme: &Theme,
) {
    buffer.set_metrics(metrics);
    buffer.set_wrap(Wrap::None);
    buffer.set_size(
        Some(viewport_width.max(1.0)),
        Some(metrics.line_height * 2.0),
    );

    let spans = build_row_spans(row, theme);
    let default_attrs = Attrs::new()
        .family(family)
        .color(theme.text_primary.to_glyphon_color());

    if spans.is_empty() {
        buffer.set_text("", &default_attrs, Shaping::Advanced, None);
    } else {
        buffer.set_rich_text(
            spans.iter().map(|span| {
                let mut attrs = Attrs::new().family(family).color(span.color);
                if span.bold {
                    attrs = attrs.weight(Weight::BOLD);
                }
                if span.italic {
                    attrs = attrs.style(Style::Italic);
                }
                (span.text.as_str(), attrs)
            }),
            &default_attrs,
            Shaping::Advanced,
            None,
        );
    }
    buffer.shape_until_scroll(font_system, false);
}

#[cfg(test)]
mod tests {
    use super::*;
    use daedalus_proto::{Cell, Color};

    #[test]
    fn build_row_spans_coalesces_adjacent_cells_and_skips_wide_continuations() {
        let row = GridRow {
            row_index: 0,
            abs_row: 0,
            cells: vec![
                Cell {
                    text: "H".into(),
                    fg: Color::Rgb(255, 0, 0),
                    bg: Color::Default,
                    flags: CellFlags::BOLD,
                    width: 1,
                },
                Cell {
                    text: "i".into(),
                    fg: Color::Rgb(255, 0, 0),
                    bg: Color::Default,
                    flags: CellFlags::BOLD,
                    width: 1,
                },
                Cell {
                    text: "!".into(),
                    fg: Color::Default,
                    bg: Color::Default,
                    flags: CellFlags::ITALIC,
                    width: 1,
                },
                Cell {
                    text: "界".into(),
                    fg: Color::Default,
                    bg: Color::Default,
                    flags: CellFlags::ITALIC,
                    width: 2,
                },
                Cell {
                    text: "".into(),
                    fg: Color::Default,
                    bg: Color::Default,
                    flags: CellFlags::ITALIC,
                    width: 0,
                },
            ],
        };

        let spans = build_row_spans(&row, &Theme::dark());
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].text, "Hi");
        assert!(spans[0].bold);
        assert!(!spans[0].italic);
        assert_eq!(spans[0].color, GlyphonColor::rgb(255, 0, 0));

        assert_eq!(spans[1].text, "!界");
        assert!(!spans[1].bold);
        assert!(spans[1].italic);

        // Light theme uses SolarizedLight default foreground for Color::Default
        let light = Theme::from_preset(crate::theme::ThemeName::SolarizedLight);
        let light_spans = build_row_spans(&row, &light);
        assert_eq!(light_spans[1].color, light.text_primary.to_glyphon_color());
    }

    #[test]
    fn measure_cell_size_scales_with_dpi_and_font_size_clamp() {
        let mut font_system = FontSystem::new();
        let family = select_mono_family(&font_system);
        let (w1, h1) = measure_cell_size(&mut font_system, family.as_deref(), 1.0);
        let (w2, h2) = measure_cell_size(&mut font_system, family.as_deref(), 2.0);
        assert!(w1 > 0.0 && h1 > 0.0);
        assert!(w2 > w1 && h2 > h1);

        // Clamped min (<= 8.0) and max (>= 32.0) match exact 8.0 and 32.0 measurements
        let min_clamped = measure_cell_size_for_font(&mut font_system, family.as_deref(), 2.0, 1.0);
        let min_exact = measure_cell_size_for_font(&mut font_system, family.as_deref(), 8.0, 1.0);
        assert_eq!(min_clamped, min_exact);

        let max_clamped =
            measure_cell_size_for_font(&mut font_system, family.as_deref(), 72.0, 1.0);
        let max_exact = measure_cell_size_for_font(&mut font_system, family.as_deref(), 32.0, 1.0);
        assert_eq!(max_clamped, max_exact);
        assert!(max_exact.0 > min_exact.0 && max_exact.1 > min_exact.1);
    }

    #[test]
    fn truncate_left_with_ellipsis_handles_long_ascii_and_multibyte_utf8() {
        assert_eq!(truncate_left_with_ellipsis("feat/auth", 20), "feat/auth");
        assert_eq!(truncate_left_with_ellipsis("feat/auth", 9), "feat/auth");
        assert_eq!(
            truncate_left_with_ellipsis("feat/very-long-branch", 12),
            "…long-branch"
        );
        assert_eq!(
            truncate_left_with_ellipsis("feat/日本語テスト🚀", 6),
            "…語テスト🚀"
        );
        assert_eq!(truncate_left_with_ellipsis("abcdef", 1), "…");
        assert_eq!(truncate_left_with_ellipsis("abcdef", 0), "");
    }

    #[test]
    fn truncate_right_with_ellipsis_handles_ascii_and_multibyte_utf8() {
        assert_eq!(truncate_right_with_ellipsis("clean", 10), "clean");
        assert_eq!(truncate_right_with_ellipsis("clean", 5), "clean");
        assert_eq!(
            truncate_right_with_ellipsis("[PHR] 2 modified, 1 untracked", 14),
            "[PHR] 2 modif…"
        );
        assert_eq!(truncate_right_with_ellipsis("日本語テスト🚀", 4), "日本語…");
        assert_eq!(truncate_right_with_ellipsis("abcdef", 1), "…");
        assert_eq!(truncate_right_with_ellipsis("abcdef", 0), "");
    }
}
