mod chrome;
#[cfg(test)]
mod frame_tests;
mod modal;
mod settings;
mod setup_review;
mod workspace_form;

use std::collections::{HashMap, HashSet};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use daedalus_proto::{CellFlags, GridRow, PaneId};
use glyphon::{
    Attrs, Buffer, Cache, Color as GlyphonColor, Family, FontSystem, Metrics, Resolution, Shaping,
    Style, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer as GlyphonTextRenderer,
    Viewport, Weight, Wrap,
};

use crate::config::clamp_font_size;
use crate::layout::overlays::sanitize_scale;
use crate::layout::{Rect, WorkspaceLayout};
use crate::state::AppState;
use crate::theme::{self, Theme};

pub const FONT_SIZE_LOGICAL: f32 = 14.0;
pub const LINE_HEIGHT_LOGICAL: f32 = 19.0;
const CHROME_FONT_SIZE_LOGICAL: f32 = 14.0;
const CHROME_LINE_HEIGHT_LOGICAL: f32 = 19.0;
const CHROME_BTN_FONT_SIZE_LOGICAL: f32 = CHROME_FONT_SIZE_LOGICAL;
const CHROME_SUB_FONT_SIZE_LOGICAL: f32 = 12.0;
const CHROME_SUB_LINE_HEIGHT_LOGICAL: f32 = 16.0;
const CHROME_BADGE_FONT_SIZE_LOGICAL: f32 = CHROME_SUB_FONT_SIZE_LOGICAL;
const CHROME_BADGE_LINE_HEIGHT_LOGICAL: f32 = CHROME_SUB_LINE_HEIGHT_LOGICAL;
const MODAL_TITLE_FONT_SIZE_LOGICAL: f32 = 18.0;
const MODAL_TITLE_LINE_HEIGHT_LOGICAL: f32 = 24.0;

const PREFERRED_MONO_FAMILIES: [&str; 3] = ["JetBrains Mono", "Cascadia Mono", "DejaVu Sans Mono"];

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

/// Keeps the editable suffix in view, budgeting monospace columns and retaining whole graphemes.
pub fn truncate_left_with_ellipsis(input: &str, columns: usize) -> String {
    if columns == 0 {
        return String::new();
    }
    if input.width() <= columns {
        return input.to_owned();
    }
    let mut remaining = columns - 1;
    let mut start = input.len();
    for (offset, cluster) in input.grapheme_indices(true).rev() {
        let Some(rest) = remaining.checked_sub(cluster.width()) else {
            break;
        };
        remaining = rest;
        start = offset;
    }
    format!("…{}", &input[start..])
}

/// Fits a status preview to its column budget without splitting a grapheme.
pub fn truncate_right_with_ellipsis(input: &str, columns: usize) -> String {
    if columns == 0 {
        return String::new();
    }
    if input.width() <= columns {
        return input.to_owned();
    }
    let mut remaining = columns - 1;
    let mut end = 0;
    for cluster in input.graphemes(true) {
        let Some(rest) = remaining.checked_sub(cluster.width()) else {
            break;
        };
        remaining = rest;
        end += cluster.len();
    }
    format!("{}…", &input[..end])
}

fn label_with_trailing_detail(name: &str, detail: &str, columns: usize) -> String {
    let suffix = format!(" · {detail}");
    if columns <= suffix.width() {
        return truncate_right_with_ellipsis(detail, columns);
    }
    format!(
        "{}{suffix}",
        truncate_right_with_ellipsis(name, columns - suffix.width())
    )
}

/// Shows the field name and scrolls the value so the insertion point stays in view.
pub(crate) fn editable_label(name: &str, value: &str, caret: usize, columns: usize) -> String {
    let prefix = format!("{name}   ");
    let available = columns.saturating_sub(prefix.width() + 1);
    let (before, after) = value.split_at(caret);
    let left = truncate_left_with_ellipsis(before, available);
    let right = truncate_right_with_ellipsis(after, available.saturating_sub(left.width()));
    format!("{prefix}{left}│{right}")
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
    pub(super) has_modal_text: bool,
    pub(super) recap_drawn: bool,
    pub(super) recap_targets: Vec<(Rect, crate::recap::RecapEntry)>,
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
            recap_drawn: false,
            recap_targets: Vec::new(),
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
        let cards = layout.sidebar.map(|sidebar| {
            super::SidebarCards::new(
                state,
                sidebar,
                self.scale_factor,
                theme,
                layout.sidebar_content.as_ref(),
            )
        });
        self.prepare_with_sidebar(
            device,
            queue,
            state,
            layout,
            theme,
            screen_w,
            screen_h,
            cards.as_ref(),
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn prepare_with_sidebar(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        state: &mut AppState,
        layout: &WorkspaceLayout,
        theme: &Theme,
        screen_w: f32,
        screen_h: f32,
        cards: Option<&super::SidebarCards>,
    ) {
        let visible_panes = self.update_visible_pane_buffers(state, layout, theme);
        self.chrome_items.clear();
        self.recap_drawn = false;
        self.recap_targets.clear();
        self.build_chrome_items(state, layout, theme, cards);

        self.prepare_base_text_pass(device, queue, &visible_panes, theme);
        self.prepare_overlay_text_pass(device, queue, state, layout, theme, (screen_w, screen_h));
    }

    fn update_visible_pane_buffers(
        &mut self,
        state: &mut AppState,
        layout: &WorkspaceLayout,
        theme: &Theme,
    ) -> Vec<VisiblePaneDraw> {
        // Prune caches for closed panes.
        self.pane_buffers
            .retain(|pane_id, _| state.panes.contains_key(pane_id));

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

        visible_panes
    }

    fn prepare_base_text_pass(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        visible_panes: &[VisiblePaneDraw],
        theme: &Theme,
    ) {
        let default_fg = theme.text_primary.to_glyphon_color();
        let mut base_areas: Vec<TextArea<'_>> = Vec::new();

        for vp in visible_panes {
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
            self.recap_drawn = false;
            self.recap_targets.clear();
            tracing::warn!(%err, "glyphon base text prepare failed");
        }
    }

    fn prepare_overlay_text_pass(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        state: &AppState,
        layout: &WorkspaceLayout,
        theme: &Theme,
        screen_size: (f32, f32),
    ) {
        let (screen_w, screen_h) = screen_size;
        self.modal_items.clear();
        let preview = layout
            .preview_panel(state, self.scale_factor)
            .map(|rect| (rect, "×  Image preview"))
            .or_else(|| {
                layout
                    .preview_button(state, self.scale_factor)
                    .map(|rect| (rect, "Preview image"))
            });
        let modal_open = state.has_input_overlay() || preview.is_some();
        self.has_modal_text = modal_open;
        if modal_open {
            if let Some(choices) = &state.failure_recipients {
                self.build_failure_recipient_items(state, choices, theme, screen_w, screen_h);
            } else if state.setup_review.open {
                self.build_setup_review_items(state, theme, screen_w, screen_h);
            } else if state.workspace_form.open {
                self.build_workspace_form_items(state, theme, screen_w, screen_h);
            } else if state.settings.open {
                self.build_settings_items(state, theme, screen_w, screen_h);
            } else if state.picker.open || state.action_palette.open {
                self.build_picker_items(state, theme, screen_w, screen_h);
            } else if state.worktree_modal.open {
                self.build_worktree_modal_items(state, theme, screen_w, screen_h);
            } else if state.close_modal.open {
                self.build_close_modal_items(state, theme, screen_w, screen_h);
            } else if let Some(ref search) = state.search {
                self.build_search_bar_items(search, state, layout, theme);
            } else if let Some((panel, label)) = preview {
                self.push_modal_line(ChromeLineSpec {
                    text: label,
                    left: panel.x + 5. * self.scale_factor,
                    top: panel.y + 2. * self.scale_factor,
                    bounds: panel,
                    max_w: panel.width,
                    font_size_logical: CHROME_SUB_FONT_SIZE_LOGICAL,
                    line_height_logical: CHROME_SUB_LINE_HEIGHT_LOGICAL,
                    color: theme.text_primary,
                    bold: true,
                });
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

    fn build_chrome_item(
        &mut self,
        spec: ChromeLineSpec<'_>,
        centered: bool,
    ) -> Option<ChromeTextItem> {
        if spec.max_w <= 1.0 || spec.bounds.width <= 0.0 || spec.bounds.height <= 0.0 {
            return None;
        }
        let s = self.scale_factor;
        let metrics = Metrics::new(spec.font_size_logical * s, spec.line_height_logical * s);
        let mut attrs = Attrs::new()
            .family(Family::SansSerif)
            .color(spec.color.to_glyphon_color());
        if spec.bold {
            attrs = attrs.weight(Weight::BOLD);
        }

        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        buffer.set_wrap(Wrap::None);
        use glyphon::cosmic_text::{Ellipsize, EllipsizeHeightLimit};
        buffer.set_ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1)));
        buffer.set_size(
            Some(spec.max_w.max(1.0)),
            Some((spec.line_height_logical * s) * 1.5),
        );
        buffer.set_text(spec.text, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.font_system, false);

        // Some fallback fonts provide unusable advances. Retry with the primary
        // font before glyphon converts the shaped positions to integer pixels.
        if buffer.layout_runs().flat_map(|run| run.glyphs).any(|g| {
            let x = spec.left + g.x + g.font_size * g.x_offset;
            let y = spec.top + g.y - g.font_size * g.y_offset;
            !(x.abs() < i32::MAX as f32 && y.abs() < i32::MAX as f32)
        }) {
            buffer.set_text(spec.text, &attrs, Shaping::Basic, None);
            buffer.shape_until_scroll(&mut self.font_system, false);
        }

        let (left, top) = if centered {
            let width = buffer.layout_runs().next().map_or(0.0, |run| run.line_w);
            let inset = (spec.bounds.width - width.min(spec.max_w)) * 0.5;
            (
                (spec.bounds.x + inset).round(),
                (spec.bounds.y + (spec.bounds.height - metrics.line_height) * 0.5).round(),
            )
        } else {
            (spec.left, spec.top)
        };
        Some(ChromeTextItem {
            buffer,
            left,
            top,
            bounds: rect_to_bounds(spec.bounds),
            default_color: spec.color.to_glyphon_color(),
        })
    }

    fn push_chrome_line(&mut self, spec: ChromeLineSpec<'_>) {
        if let Some(item) = self.build_chrome_item(spec, false) {
            self.chrome_items.push(item);
        }
    }

    fn push_chrome_button(&mut self, spec: ChromeLineSpec<'_>) {
        if let Some(item) = self.build_chrome_item(spec, true) {
            self.chrome_items.push(item);
        }
    }

    fn push_modal_button(&mut self, spec: ChromeLineSpec<'_>) {
        if let Some(item) = self.build_chrome_item(spec, true) {
            self.modal_items.push(item);
        }
    }

    fn push_modal_line(&mut self, spec: ChromeLineSpec<'_>) {
        if let Some(item) = self.build_chrome_item(spec, false) {
            self.modal_items.push(item);
        }
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
    fn prepared_frames_update_dirty_terminal_text_and_clear_a_closed_recipient_picker() {
        use crate::renderer::tests::make_state_with_agent_row;
        use crate::state::FailureRecipients;
        use daedalus_proto::GridDelta;

        let (device, queue) = wgpu::Device::noop(&Default::default());
        let mut renderer = TextRenderer::new(
            &device,
            &queue,
            wgpu::TextureFormat::Bgra8UnormSrgb,
            1200,
            800,
            1.,
            14.,
            None,
        );
        let (mut state, layout) = make_state_with_agent_row();
        let agent = state.focused_pane_id().unwrap();
        let frame = |renderer: &mut TextRenderer, state: &mut AppState| {
            renderer.prepare(&device, &queue, state, &layout, &Theme::dark(), 1200., 800.);
        };

        frame(&mut renderer, &mut state);
        assert_eq!(renderer.pane_buffers[&agent].rows[0].lines[0].text(), "abc");
        assert!(!renderer.has_modal_text);

        let pane = state.panes.get_mut(&agent).unwrap();
        let mut changed = pane.viewport_rows[0].clone();
        changed.cells[0].text = "z".into();
        pane.apply_delta(GridDelta {
            activity: None,
            insight: None,
            pane_id: agent,
            seq: 2,
            cols: 20,
            rows: 5,
            cursor: Default::default(),
            dirty_rows: vec![changed],
            history_generation: 0,
            scrollback_len: None,
        });
        state.session_names.insert(agent, "Reviewer".into());
        state.failure_recipients = Some(FailureRecipients {
            workspace: state.active_workspace_id.unwrap(),
            runner: PaneId::new(),
            agents: vec![agent],
            selected: 0,
        });
        frame(&mut renderer, &mut state);
        assert_eq!(renderer.pane_buffers[&agent].rows[0].lines[0].text(), "zbc");
        assert!(renderer.has_modal_text);
        let labels: Vec<_> = renderer
            .modal_items
            .iter()
            .map(|item| item.buffer.lines[0].text())
            .collect();
        assert_eq!(labels.first().copied(), Some("Send failure to agent"));
        assert_eq!(
            labels.last().copied(),
            Some(state.failure_recipient_label(agent).as_str())
        );

        state.failure_recipients = None;
        frame(&mut renderer, &mut state);
        assert!(!renderer.has_modal_text);
        assert!(renderer.modal_items.is_empty());
        assert_eq!(renderer.pane_buffers[&agent].rows[0].lines[0].text(), "zbc");
    }

    #[test]
    fn build_row_spans_coalesces_adjacent_cells_and_skips_wide_continuations() {
        let row = GridRow {
            links: Vec::new(),
            wraps: false,
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
            "…ト🚀"
        );
        assert_eq!(truncate_left_with_ellipsis("abcdef", 1), "…");
        assert_eq!(truncate_left_with_ellipsis("abcdef", 0), "");
    }

    #[test]
    fn chrome_truncation_keeps_combining_sequences_and_wide_glyphs_whole() {
        // A base letter with a combining accent occupies one column.
        assert_eq!(truncate_left_with_ellipsis("long-é", 2), "…é");
        assert_eq!(truncate_right_with_ellipsis("é-終", 2), "é…");
        assert_eq!(truncate_left_with_ellipsis("long-終", 2), "…");
        assert_eq!(truncate_right_with_ellipsis("終日", 2), "…");
    }

    #[test]
    fn compact_recipient_rows_keep_the_disambiguating_id_after_long_names() {
        for id in ["deadbeef", "cafefeed"] {
            let label = label_with_trailing_detail("長い同じ名前を持っているエージェント", id, 22);
            assert!(label.ends_with(&format!("… · {id}")));
            assert!(label.width() <= 22);
        }
    }

    #[test]
    fn truncate_right_with_ellipsis_handles_ascii_and_multibyte_utf8() {
        assert_eq!(truncate_right_with_ellipsis("clean", 10), "clean");
        assert_eq!(truncate_right_with_ellipsis("clean", 5), "clean");
        assert_eq!(
            truncate_right_with_ellipsis("[PHR] 2 modified, 1 untracked", 14),
            "[PHR] 2 modif…"
        );
        assert_eq!(truncate_right_with_ellipsis("日本語テスト🚀", 4), "日…");
        assert_eq!(truncate_right_with_ellipsis("abcdef", 1), "…");
        assert_eq!(truncate_right_with_ellipsis("abcdef", 0), "");
    }
}
