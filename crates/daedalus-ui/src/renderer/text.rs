use std::collections::{HashMap, HashSet};

use daedalus_proto::{CellFlags, GridRow, PaneId, PaneRole, PaneStatus, WorkspaceInfo};
use glyphon::{
    Attrs, Buffer, Cache, Color as GlyphonColor, Family, FontSystem, Metrics, Resolution, Shaping,
    Style, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer as GlyphonTextRenderer,
    Viewport, Weight, Wrap,
};

use crate::layout::{
    LayoutMetrics, PaneRect, Rect, WorkspaceLayout, sidebar_new_workspace_rect,
    sidebar_workspace_rect,
};
use crate::state::{AgentPreset, AppState};
use crate::theme::{self, Theme};

use super::{picker_item_rect, picker_modal_rect};

pub const FONT_SIZE_LOGICAL: f32 = 14.0;
pub const LINE_HEIGHT_LOGICAL: f32 = 19.0;
const CHROME_FONT_SIZE_LOGICAL: f32 = 12.5;
const CHROME_LINE_HEIGHT_LOGICAL: f32 = 16.0;
const CHROME_SUB_FONT_SIZE_LOGICAL: f32 = 11.0;
const CHROME_SUB_LINE_HEIGHT_LOGICAL: f32 = 14.0;
const MODAL_TITLE_FONT_SIZE_LOGICAL: f32 = 13.5;
const MODAL_TITLE_LINE_HEIGHT_LOGICAL: f32 = 18.0;

const PREFERRED_MONO_FAMILIES: [&str; 3] =
    ["JetBrains Mono", "Cascadia Mono", "DejaVu Sans Mono"];

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

/// Measures the physical `(cell_w, cell_h)` of the chosen monospace font at `scale_factor`.
pub fn measure_cell_size(
    font_system: &mut FontSystem,
    mono_family_name: Option<&str>,
    scale_factor: f32,
) -> (f32, f32) {
    let s = sanitize_scale(scale_factor);
    let font_size = FONT_SIZE_LOGICAL * s;
    let line_height = LINE_HEIGHT_LOGICAL * s;

    let family = mono_family_name.map_or(Family::Monospace, Family::Name);
    let mut buffer = Buffer::new(font_system, Metrics::new(font_size, line_height));
    buffer.set_wrap(font_system, Wrap::None);
    buffer.set_size(font_system, Some(font_size * 20.0), Some(line_height * 2.0));
    buffer.set_text(
        font_system,
        "MMMMMMMMMM",
        Attrs::new().family(family),
        Shaping::Advanced,
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

struct CachedPaneRows {
    rows: Vec<Buffer>,
    scale_factor: f32,
    viewport_width: f32,
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
    font_size: f32,
    line_height: f32,
    cell_w: f32,
    cell_h: f32,
    pane_buffers: HashMap<PaneId, CachedPaneRows>,
    chrome_items: Vec<ChromeTextItem>,
    modal_items: Vec<ChromeTextItem>,
    has_modal_text: bool,
}

impl TextRenderer {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
        scale_factor: f32,
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

        let mono_family_name = select_mono_family(&font_system);
        let s = sanitize_scale(scale_factor);
        let (cell_w, cell_h) = measure_cell_size(&mut font_system, mono_family_name.as_deref(), s);

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
            font_size: FONT_SIZE_LOGICAL * s,
            line_height: LINE_HEIGHT_LOGICAL * s,
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
            self.font_size = FONT_SIZE_LOGICAL * s;
            self.line_height = LINE_HEIGHT_LOGICAL * s;
            let (cw, ch) = measure_cell_size(
                &mut self.font_system,
                self.mono_family_name.as_deref(),
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

            let metrics = Metrics::new(self.font_size, self.line_height);
            let viewport_width = pane_rect.viewport.width.max(1.0);

            let entry = self
                .pane_buffers
                .entry(pane_id)
                .or_insert_with(|| CachedPaneRows {
                    rows: Vec::new(),
                    scale_factor: 0.0,
                    viewport_width: 0.0,
                });

            let needs_full_rebuild = entry.rows.len() != row_count
                || (entry.scale_factor - self.scale_factor).abs() > 1e-4
                || (entry.viewport_width - viewport_width).abs() > 1.0;

            if entry.rows.len() < row_count {
                while entry.rows.len() < row_count {
                    entry.rows.push(Buffer::new(&mut self.font_system, metrics));
                }
            } else if entry.rows.len() > row_count {
                entry.rows.truncate(row_count);
            }
            entry.scale_factor = self.scale_factor;
            entry.viewport_width = viewport_width;

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

        // 4. Rebuild & prepare modal text pass if the Agent preset picker is open.
        self.modal_items.clear();
        self.has_modal_text = state.picker.open;
        if state.picker.open {
            self.build_picker_items(state, theme, screen_w, screen_h);
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
        buffer.set_wrap(&mut self.font_system, Wrap::None);
        buffer.set_size(
            &mut self.font_system,
            Some(spec.max_w.max(1.0)),
            Some((spec.line_height_logical * s) * 1.5),
        );
        buffer.set_text(&mut self.font_system, spec.text, attrs, Shaping::Advanced);
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

    fn build_chrome_items(
        &mut self,
        state: &AppState,
        layout: &WorkspaceLayout,
        theme: &Theme,
    ) {
        let s = self.scale_factor;

        // Sidebar text
        if let Some(sidebar) = layout.sidebar {
            let m = LayoutMetrics::for_scale(s);
            let header_rect = Rect::new(sidebar.x, sidebar.y, sidebar.width, m.sidebar_header_height);
            self.push_chrome_line(ChromeLineSpec {
                text: "WORKSPACES",
                left: header_rect.x + m.sidebar_pad + 4.0 * s,
                top: header_rect.y + (header_rect.height - CHROME_SUB_LINE_HEIGHT_LOGICAL * s) * 0.5,
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
                    text: "+ New Workspace",
                    left: new_btn.x + 12.0 * s,
                    top: new_btn.y + (new_btn.height - CHROME_LINE_HEIGHT_LOGICAL * s) * 0.5,
                    max_w: new_btn.width - 20.0 * s,
                    bounds: new_btn,
                    font_size_logical: CHROME_FONT_SIZE_LOGICAL,
                    line_height_logical: CHROME_LINE_HEIGHT_LOGICAL,
                    color: theme.text_primary,
                    bold: false,
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

        let pill_reserve = if pill_text.is_some() { 54.0 * s } else { 8.0 * s };
        let name_max_w = (row_rect.right() - left_x - pill_reserve).max(1.0);
        let title_color = if is_active {
            theme.text_primary
        } else {
            theme.text_primary.scale_luminance(0.85)
        };

        self.push_chrome_line(ChromeLineSpec {
            text: &ws.name,
            left: left_x,
            top: row_rect.y + 7.0 * s,
            max_w: name_max_w,
            bounds: Rect::new(left_x, row_rect.y, name_max_w, row_rect.height),
            font_size_logical: CHROME_FONT_SIZE_LOGICAL,
            line_height_logical: CHROME_LINE_HEIGHT_LOGICAL,
            color: title_color,
            bold: is_active,
        });

        if let Some((label, color)) = pill_text {
            let pill_x = row_rect.right() - 48.0 * s;
            self.push_chrome_line(ChromeLineSpec {
                text: &label,
                left: pill_x,
                top: row_rect.y + 8.0 * s,
                max_w: 44.0 * s,
                bounds: row_rect,
                font_size_logical: CHROME_SUB_FONT_SIZE_LOGICAL,
                line_height_logical: CHROME_SUB_LINE_HEIGHT_LOGICAL,
                color,
                bold: true,
            });
        }

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
        let sub_max_w = (row_rect.right() - left_x - 8.0 * s).max(1.0);
        self.push_chrome_line(ChromeLineSpec {
            text: &subtitle,
            left: left_x,
            top: row_rect.y + 27.0 * s,
            max_w: sub_max_w,
            bounds: row_rect,
            font_size_logical: CHROME_SUB_FONT_SIZE_LOGICAL,
            line_height_logical: CHROME_SUB_LINE_HEIGHT_LOGICAL,
            color: theme.text_muted,
            bold: false,
        });
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

        let title = format!("{role_label}{cwd_suffix}{scroll_suffix}");
        let text_left = header.x + 22.0 * s;
        let text_right = pane_rect
            .action_pill
            .map_or(header.right() - 8.0 * s, |pill| pill.x - 8.0 * s);
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

        if let Some(pill) = pane_rect.action_pill {
            let (pill_label, pill_color) = match pane_rect.role {
                PaneRole::Agent => {
                    let preset_name = pane_state
                        .and_then(|p| p.agent_preset)
                        .map(|p| p.short_name())
                        .unwrap_or("preset");
                    (format!("Preset: {preset_name}"), theme.border_focused)
                }
                PaneRole::Runner => ("Send Error → Agent ^⇧E".to_string(), theme.status_err),
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
        let modal = picker_modal_rect(screen_w, screen_h, s);

        self.push_modal_line(ChromeLineSpec {
            text: "Select Agent Preset  (↑/↓ Enter · Esc)",
            left: modal.x + 16.0 * s,
            top: modal.y + 12.0 * s,
            max_w: (modal.width - 32.0 * s).max(1.0),
            bounds: modal,
            font_size_logical: MODAL_TITLE_FONT_SIZE_LOGICAL,
            line_height_logical: MODAL_TITLE_LINE_HEIGHT_LOGICAL,
            color: theme.text_primary,
            bold: true,
        });

        for (idx, preset) in AgentPreset::ALL.iter().enumerate() {
            let item_rect = picker_item_rect(modal, idx, s);
            let selected = idx == state.picker.selected_index;
            let prefix = if selected { "▸ " } else { "  " };
            let label = format!("{prefix}{}", preset.label());
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
}

/// Coalesced styled text run within a single terminal row.
#[derive(Debug, PartialEq, Eq)]
struct RowSpan {
    text: String,
    color: GlyphonColor,
    bold: bool,
    italic: bool,
}

fn build_row_spans(row: &GridRow) -> Vec<RowSpan> {
    let mut spans: Vec<RowSpan> = Vec::new();
    for cell in &row.cells {
        if cell.width == 0 {
            continue;
        }
        let (fg_rgba, _bg) = theme::resolve_cell_colors(cell.fg, cell.bg, cell.flags);
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
) {
    buffer.set_metrics(font_system, metrics);
    buffer.set_wrap(font_system, Wrap::None);
    buffer.set_size(
        font_system,
        Some(viewport_width.max(1.0)),
        Some(metrics.line_height * 2.0),
    );

    let spans = build_row_spans(row);
    let default_attrs = Attrs::new()
        .family(family)
        .color(theme::TEXT_PRIMARY.to_glyphon_color());

    if spans.is_empty() {
        buffer.set_text(font_system, "", default_attrs, Shaping::Advanced);
    } else {
        buffer.set_rich_text(
            font_system,
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
            default_attrs,
            Shaping::Advanced,
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

        let spans = build_row_spans(&row);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].text, "Hi");
        assert!(spans[0].bold);
        assert!(!spans[0].italic);
        assert_eq!(spans[0].color, GlyphonColor::rgb(255, 0, 0));

        assert_eq!(spans[1].text, "!界");
        assert!(!spans[1].bold);
        assert!(spans[1].italic);
    }

    #[test]
    fn measure_cell_size_scales_with_dpi() {
        let mut font_system = FontSystem::new();
        let family = select_mono_family(&font_system);
        let (w1, h1) = measure_cell_size(&mut font_system, family.as_deref(), 1.0);
        let (w2, h2) = measure_cell_size(&mut font_system, family.as_deref(), 2.0);
        assert!(w1 > 0.0 && h1 > 0.0);
        assert!(w2 > w1 && h2 > h1);
    }
}
