use daedalus_proto::{CellFlags, Color, PaneRole};

/// Linear/normalized RGBA color in `[0.0, 1.0]` used by the GPU quad renderer and UI chrome.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgba(pub [f32; 4]);

impl Rgba {
    pub const fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self([r, g, b, a])
    }

    pub const fn from_rgb_u8(r: u8, g: u8, b: u8) -> Self {
        Self([r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0])
    }

    pub const fn from_rgba_u8(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self([
            r as f32 / 255.0,
            g as f32 / 255.0,
            b as f32 / 255.0,
            a as f32 / 255.0,
        ])
    }

    pub const fn to_array(self) -> [f32; 4] {
        self.0
    }

    pub fn with_alpha(self, alpha: f32) -> Self {
        Self([self.0[0], self.0[1], self.0[2], alpha.clamp(0.0, 1.0)])
    }

    /// Scales RGB luminance by `factor` (clamped to `[0.0, 1.0]`) while preserving alpha.
    pub fn scale_luminance(self, factor: f32) -> Self {
        let f = factor.max(0.0);
        Self([
            (self.0[0] * f).clamp(0.0, 1.0),
            (self.0[1] * f).clamp(0.0, 1.0),
            (self.0[2] * f).clamp(0.0, 1.0),
            self.0[3],
        ])
    }

    pub fn to_glyphon_color(self) -> glyphon::Color {
        let to_u8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        glyphon::Color::rgba(
            to_u8(self.0[0]),
            to_u8(self.0[1]),
            to_u8(self.0[2]),
            to_u8(self.0[3]),
        )
    }
}

/// Luminance multiplier applied to foreground colors when `CellFlags::DIM` is set.
pub const DIM_FACTOR: f32 = 0.6;

// Dark workbench palette constants
pub const BG_WINDOW: Rgba = Rgba::from_rgb_u8(0x0E, 0x11, 0x16);
pub const BG_SIDEBAR: Rgba = Rgba::from_rgb_u8(0x13, 0x17, 0x1F);
pub const BG_PANE: Rgba = Rgba::from_rgb_u8(0x16, 0x1B, 0x22);
pub const BG_HEADER: Rgba = Rgba::from_rgb_u8(0x1C, 0x22, 0x2D);
pub const BORDER_UNFOCUSED: Rgba = Rgba::from_rgb_u8(0x2B, 0x33, 0x42);
pub const BORDER_FOCUSED: Rgba = Rgba::from_rgb_u8(0x58, 0xA6, 0xFF);
pub const TEXT_PRIMARY: Rgba = Rgba::from_rgb_u8(0xE6, 0xED, 0xF3);
pub const TEXT_MUTED: Rgba = Rgba::from_rgb_u8(0x8B, 0x94, 0x9E);
pub const STATUS_OK: Rgba = Rgba::from_rgb_u8(0x3F, 0xB9, 0x50);
pub const STATUS_BUSY: Rgba = Rgba::from_rgb_u8(0xD2, 0x99, 0x22);
pub const STATUS_ERR: Rgba = Rgba::from_rgb_u8(0xF8, 0x51, 0x49);
pub const SELECTION_BG: Rgba = Rgba::from_rgba_u8(0x38, 0x8B, 0xFD, 0x66);
pub const CURSOR_BG: Rgba = Rgba::from_rgb_u8(0x58, 0xA6, 0xFF);
pub const PICKER_BG: Rgba = Rgba::from_rgba_u8(0x18, 0x1E, 0x28, 0xF5);

/// Standard 16-color ANSI palette tuned for a dark terminal workbench.
const ANSI_16: [Rgba; 16] = [
    // Normal (0..=7)
    Rgba::from_rgb_u8(0x21, 0x26, 0x2D), // 0: Black
    Rgba::from_rgb_u8(0xF8, 0x51, 0x49), // 1: Red
    Rgba::from_rgb_u8(0x3F, 0xB9, 0x50), // 2: Green
    Rgba::from_rgb_u8(0xD2, 0x99, 0x22), // 3: Yellow
    Rgba::from_rgb_u8(0x58, 0xA6, 0xFF), // 4: Blue
    Rgba::from_rgb_u8(0xBC, 0x8C, 0xFF), // 5: Magenta
    Rgba::from_rgb_u8(0x39, 0xC5, 0xCF), // 6: Cyan
    Rgba::from_rgb_u8(0xB1, 0xBA, 0xC4), // 7: White
    // Bright (8..=15)
    Rgba::from_rgb_u8(0x6E, 0x76, 0x81), // 8: Bright Black
    Rgba::from_rgb_u8(0xFF, 0x7B, 0x72), // 9: Bright Red
    Rgba::from_rgb_u8(0x56, 0xD3, 0x64), // 10: Bright Green
    Rgba::from_rgb_u8(0xE3, 0xB3, 0x41), // 11: Bright Yellow
    Rgba::from_rgb_u8(0x79, 0xC0, 0xFF), // 12: Bright Blue
    Rgba::from_rgb_u8(0xD2, 0xA8, 0xFF), // 13: Bright Magenta
    Rgba::from_rgb_u8(0x56, 0xD4, 0xDD), // 14: Bright Cyan
    Rgba::from_rgb_u8(0xF0, 0xF6, 0xFC), // 15: Bright White
];

/// Workbench color theme.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    pub bg_window: Rgba,
    pub bg_sidebar: Rgba,
    pub bg_pane: Rgba,
    pub bg_header: Rgba,
    pub border_unfocused: Rgba,
    pub border_focused: Rgba,
    pub text_primary: Rgba,
    pub text_muted: Rgba,
    pub status_ok: Rgba,
    pub status_busy: Rgba,
    pub status_err: Rgba,
    pub selection_bg: Rgba,
    pub cursor_bg: Rgba,
    pub picker_bg: Rgba,
}

impl Theme {
    pub const fn dark() -> Self {
        Self {
            bg_window: BG_WINDOW,
            bg_sidebar: BG_SIDEBAR,
            bg_pane: BG_PANE,
            bg_header: BG_HEADER,
            border_unfocused: BORDER_UNFOCUSED,
            border_focused: BORDER_FOCUSED,
            text_primary: TEXT_PRIMARY,
            text_muted: TEXT_MUTED,
            status_ok: STATUS_OK,
            status_busy: STATUS_BUSY,
            status_err: STATUS_ERR,
            selection_bg: SELECTION_BG,
            cursor_bg: CURSOR_BG,
            picker_bg: PICKER_BG,
        }
    }

    pub const fn role_accent(&self, role: PaneRole) -> Rgba {
        match role {
            PaneRole::Agent => self.border_focused,
            PaneRole::Service => self.status_ok,
            PaneRole::Runner => self.status_busy,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

/// Maps an xterm 256-color palette index (`0..=255`) to an `Rgba` color.
///
/// - `0..=15`: standard + bright ANSI colors
/// - `16..=231`: 6×6×6 RGB color cube (`0, 95, 135, 175, 215, 255`)
/// - `232..=255`: 24-step grayscale ramp (`8, 18, ..., 238`)
pub fn ansi_indexed_color(idx: u8) -> Rgba {
    match idx {
        0..=15 => ANSI_16[idx as usize],
        16..=231 => {
            let cube = idx - 16;
            let r_idx = cube / 36;
            let g_idx = (cube / 6) % 6;
            let b_idx = cube % 6;
            let level = |n: u8| if n == 0 { 0 } else { 55 + 40 * n };
            Rgba::from_rgb_u8(level(r_idx), level(g_idx), level(b_idx))
        }
        232..=255 => {
            let v = 8 + (idx - 232) * 10;
            Rgba::from_rgb_u8(v, v, v)
        }
    }
}

fn resolve_color(color: Color, default: Rgba) -> Rgba {
    match color {
        Color::Default => default,
        Color::Indexed(idx) => ansi_indexed_color(idx),
        Color::Rgb(r, g, b) => Rgba::from_rgb_u8(r, g, b),
    }
}

/// Resolves a terminal cell's `(fg, bg)` colors and `CellFlags` into concrete RGBA values.
///
/// Returns `(foreground, background)` where `background` is `None` when the cell uses the
/// pane's default background (allowing the renderer to skip drawing redundant background quads).
///
/// - `CellFlags::INVERSE` swaps foreground and background (resolving default fg/bg to
///   `TEXT_PRIMARY` and `BG_PANE` respectively).
/// - `CellFlags::DIM` scales foreground luminance to 60% (`DIM_FACTOR`).
pub fn resolve_cell_colors(fg: Color, bg: Color, flags: CellFlags) -> (Rgba, Option<Rgba>) {
    let (mut resolved_fg, resolved_bg) = if flags.contains(CellFlags::INVERSE) {
        let swapped_fg = resolve_color(bg, BG_PANE);
        let swapped_bg = resolve_color(fg, TEXT_PRIMARY);
        (swapped_fg, Some(swapped_bg))
    } else {
        let fg_rgba = resolve_color(fg, TEXT_PRIMARY);
        let bg_rgba = match bg {
            Color::Default => None,
            other => Some(resolve_color(other, BG_PANE)),
        };
        (fg_rgba, bg_rgba)
    };

    if flags.contains(CellFlags::DIM) {
        resolved_fg = resolved_fg.scale_luminance(DIM_FACTOR);
    }

    (resolved_fg, resolved_bg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_rgba_approx(actual: Rgba, expected: Rgba) {
        for i in 0..4 {
            assert!(
                (actual.0[i] - expected.0[i]).abs() < 1e-5,
                "channel {i} mismatch: actual={:?}, expected={:?}",
                actual.0,
                expected.0
            );
        }
    }

    #[test]
    fn ansi_standard_16_colors_match_palette() {
        for idx in 0..=15u8 {
            assert_eq!(ansi_indexed_color(idx), ANSI_16[idx as usize]);
        }
        // Sanity check distinct normal vs bright red
        assert_ne!(ansi_indexed_color(1), ansi_indexed_color(9));
    }

    #[test]
    fn ansi_6x6x6_cube_endpoints_and_midpoints() {
        // 16 -> (0, 0, 0)
        assert_eq!(ansi_indexed_color(16), Rgba::from_rgb_u8(0, 0, 0));
        // 196 -> r=5, g=0, b=0 -> (255, 0, 0)
        assert_eq!(ansi_indexed_color(196), Rgba::from_rgb_u8(255, 0, 0));
        // 46 -> r=0, g=5, b=0 -> (0, 255, 0)
        assert_eq!(ansi_indexed_color(46), Rgba::from_rgb_u8(0, 255, 0));
        // 21 -> r=0, g=0, b=5 -> (0, 0, 255)
        assert_eq!(ansi_indexed_color(21), Rgba::from_rgb_u8(0, 0, 255));
        // 231 -> (5, 5, 5) -> (255, 255, 255)
        assert_eq!(ansi_indexed_color(231), Rgba::from_rgb_u8(255, 255, 255));
        // 102 -> cube=86 -> r=2 (135), g=2 (135), b=2 (135)
        assert_eq!(ansi_indexed_color(102), Rgba::from_rgb_u8(135, 135, 135));
    }

    #[test]
    fn ansi_24_step_grayscale_ramp() {
        // 232 -> 8
        assert_eq!(ansi_indexed_color(232), Rgba::from_rgb_u8(8, 8, 8));
        // 244 -> 8 + 12*10 = 128
        assert_eq!(ansi_indexed_color(244), Rgba::from_rgb_u8(128, 128, 128));
        // 255 -> 8 + 23*10 = 238
        assert_eq!(ansi_indexed_color(255), Rgba::from_rgb_u8(238, 238, 238));
        // Strictly monotonically increasing across 232..=255
        for idx in 232..255u8 {
            assert!(ansi_indexed_color(idx).0[0] < ansi_indexed_color(idx + 1).0[0]);
        }
    }

    #[test]
    fn resolve_default_and_explicit_colors() {
        let (fg, bg) = resolve_cell_colors(Color::Default, Color::Default, CellFlags::empty());
        assert_eq!(fg, TEXT_PRIMARY);
        assert_eq!(bg, None);

        let (fg, bg) = resolve_cell_colors(
            Color::Rgb(10, 20, 30),
            Color::Indexed(4),
            CellFlags::empty(),
        );
        assert_eq!(fg, Rgba::from_rgb_u8(10, 20, 30));
        assert_eq!(bg, Some(ansi_indexed_color(4)));
    }

    #[test]
    fn resolve_inverse_swaps_default_and_explicit_colors() {
        let (fg, bg) = resolve_cell_colors(Color::Default, Color::Default, CellFlags::INVERSE);
        assert_eq!(fg, BG_PANE);
        assert_eq!(bg, Some(TEXT_PRIMARY));

        let (fg, bg) = resolve_cell_colors(
            Color::Rgb(200, 100, 50),
            Color::Rgb(10, 20, 30),
            CellFlags::INVERSE,
        );
        assert_eq!(fg, Rgba::from_rgb_u8(10, 20, 30));
        assert_eq!(bg, Some(Rgba::from_rgb_u8(200, 100, 50)));
    }

    #[test]
    fn resolve_dim_scales_foreground_luminance_to_60_percent() {
        let base = Rgba::from_rgb_u8(200, 100, 50);
        let (fg, bg) = resolve_cell_colors(
            Color::Rgb(200, 100, 50),
            Color::Default,
            CellFlags::DIM,
        );
        assert_eq!(bg, None);
        assert_rgba_approx(
            fg,
            Rgba::new(
                base.0[0] * DIM_FACTOR,
                base.0[1] * DIM_FACTOR,
                base.0[2] * DIM_FACTOR,
                1.0,
            ),
        );

        // DIM + INVERSE dims the swapped foreground while keeping background un-dimmed
        let (fg, bg) = resolve_cell_colors(
            Color::Rgb(200, 100, 50),
            Color::Rgb(100, 100, 100),
            CellFlags::INVERSE | CellFlags::DIM,
        );
        let swapped_base = Rgba::from_rgb_u8(100, 100, 100);
        assert_rgba_approx(fg, swapped_base.scale_luminance(DIM_FACTOR));
        assert_eq!(bg, Some(base));
    }

    #[test]
    fn rgba_converts_to_glyphon_color() {
        let c = Rgba::from_rgba_u8(0x58, 0xA6, 0xFF, 0x80).to_glyphon_color();
        assert_eq!(c, glyphon::Color::rgba(0x58, 0xA6, 0xFF, 0x80));
    }
}
