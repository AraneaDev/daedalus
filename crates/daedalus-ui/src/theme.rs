use std::fmt;

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

/// Built-in workbench color theme presets selectable via `config.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ThemeName {
    #[default]
    DaedalusDark,
    CatppuccinMocha,
    TokyoNight,
    GruvboxDark,
    SolarizedLight,
}

impl ThemeName {
    pub const ALL: [Self; 5] = [
        Self::DaedalusDark,
        Self::CatppuccinMocha,
        Self::TokyoNight,
        Self::GruvboxDark,
        Self::SolarizedLight,
    ];

    /// Parses a preset name (case-insensitive, hyphenated or underscored).
    pub fn from_name(name: &str) -> Option<Self> {
        let normalized = name.trim().to_ascii_lowercase().replace('_', "-");
        match normalized.as_str() {
            "daedalus-dark" => Some(Self::DaedalusDark),
            "catppuccin-mocha" => Some(Self::CatppuccinMocha),
            "tokyo-night" => Some(Self::TokyoNight),
            "gruvbox-dark" => Some(Self::GruvboxDark),
            "solarized-light" => Some(Self::SolarizedLight),
            _ => None,
        }
    }

    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::DaedalusDark => "daedalus-dark",
            Self::CatppuccinMocha => "catppuccin-mocha",
            Self::TokyoNight => "tokyo-night",
            Self::GruvboxDark => "gruvbox-dark",
            Self::SolarizedLight => "solarized-light",
        }
    }
}

impl fmt::Display for ThemeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

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
    pub ansi_16: [Rgba; 16],
}

const DAEDALUS_DARK_THEME: Theme = Theme {
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
    ansi_16: ANSI_16,
};

const CATPPUCCIN_MOCHA_THEME: Theme = Theme {
    bg_window: Rgba::from_rgb_u8(0x11, 0x11, 0x1B),
    bg_sidebar: Rgba::from_rgb_u8(0x18, 0x18, 0x25),
    bg_pane: Rgba::from_rgb_u8(0x1E, 0x1E, 0x2E),
    bg_header: Rgba::from_rgb_u8(0x31, 0x32, 0x44),
    border_unfocused: Rgba::from_rgb_u8(0x45, 0x47, 0x5A),
    border_focused: Rgba::from_rgb_u8(0x89, 0xB4, 0xFA),
    text_primary: Rgba::from_rgb_u8(0xCD, 0xD6, 0xF4),
    text_muted: Rgba::from_rgb_u8(0xA6, 0xAD, 0xC8),
    status_ok: Rgba::from_rgb_u8(0xA6, 0xE3, 0xA1),
    status_busy: Rgba::from_rgb_u8(0xF9, 0xE2, 0xAF),
    status_err: Rgba::from_rgb_u8(0xF3, 0x8B, 0xA8),
    selection_bg: Rgba::from_rgba_u8(0x58, 0x5B, 0x70, 0x66),
    cursor_bg: Rgba::from_rgb_u8(0xF5, 0xE0, 0xDC),
    picker_bg: Rgba::from_rgba_u8(0x18, 0x18, 0x25, 0xF5),
    ansi_16: [
        Rgba::from_rgb_u8(0x45, 0x47, 0x5A), // 0: Black (Surface1)
        Rgba::from_rgb_u8(0xF3, 0x8B, 0xA8), // 1: Red
        Rgba::from_rgb_u8(0xA6, 0xE3, 0xA1), // 2: Green
        Rgba::from_rgb_u8(0xF9, 0xE2, 0xAF), // 3: Yellow
        Rgba::from_rgb_u8(0x89, 0xB4, 0xFA), // 4: Blue
        Rgba::from_rgb_u8(0xF5, 0xC2, 0xE7), // 5: Pink
        Rgba::from_rgb_u8(0x94, 0xE2, 0xD5), // 6: Teal
        Rgba::from_rgb_u8(0xBA, 0xC2, 0xDE), // 7: Subtext1
        Rgba::from_rgb_u8(0x58, 0x5B, 0x70), // 8: Bright Black (Surface2)
        Rgba::from_rgb_u8(0xF3, 0x8B, 0xA8), // 9: Bright Red
        Rgba::from_rgb_u8(0xA6, 0xE3, 0xA1), // 10: Bright Green
        Rgba::from_rgb_u8(0xF9, 0xE2, 0xAF), // 11: Bright Yellow
        Rgba::from_rgb_u8(0x89, 0xB4, 0xFA), // 12: Bright Blue
        Rgba::from_rgb_u8(0xF5, 0xC2, 0xE7), // 13: Bright Magenta
        Rgba::from_rgb_u8(0x94, 0xE2, 0xD5), // 14: Bright Cyan
        Rgba::from_rgb_u8(0xCD, 0xD6, 0xF4), // 15: Bright White (Text)
    ],
};

const TOKYO_NIGHT_THEME: Theme = Theme {
    bg_window: Rgba::from_rgb_u8(0x16, 0x16, 0x1E),
    bg_sidebar: Rgba::from_rgb_u8(0x18, 0x1A, 0x24),
    bg_pane: Rgba::from_rgb_u8(0x1A, 0x1B, 0x26),
    bg_header: Rgba::from_rgb_u8(0x24, 0x28, 0x3B),
    border_unfocused: Rgba::from_rgb_u8(0x29, 0x2E, 0x42),
    border_focused: Rgba::from_rgb_u8(0x7A, 0xA2, 0xF7),
    text_primary: Rgba::from_rgb_u8(0xC0, 0xCA, 0xF5),
    text_muted: Rgba::from_rgb_u8(0x8D, 0x92, 0xAF),
    status_ok: Rgba::from_rgb_u8(0x9E, 0xCE, 0x6A),
    status_busy: Rgba::from_rgb_u8(0xE0, 0xAF, 0x68),
    status_err: Rgba::from_rgb_u8(0xF7, 0x76, 0x8E),
    selection_bg: Rgba::from_rgba_u8(0x33, 0x46, 0x7C, 0x80),
    cursor_bg: Rgba::from_rgb_u8(0x7A, 0xA2, 0xF7),
    picker_bg: Rgba::from_rgba_u8(0x16, 0x16, 0x1E, 0xF5),
    ansi_16: [
        Rgba::from_rgb_u8(0x15, 0x16, 0x1E), // 0: Black
        Rgba::from_rgb_u8(0xF7, 0x76, 0x8E), // 1: Red
        Rgba::from_rgb_u8(0x9E, 0xCE, 0x6A), // 2: Green
        Rgba::from_rgb_u8(0xE0, 0xAF, 0x68), // 3: Yellow
        Rgba::from_rgb_u8(0x7A, 0xA2, 0xF7), // 4: Blue
        Rgba::from_rgb_u8(0xBB, 0x9A, 0xF7), // 5: Magenta
        Rgba::from_rgb_u8(0x7D, 0xCF, 0xFF), // 6: Cyan
        Rgba::from_rgb_u8(0xA9, 0xB1, 0xD6), // 7: White
        Rgba::from_rgb_u8(0x41, 0x48, 0x68), // 8: Bright Black
        Rgba::from_rgb_u8(0xF7, 0x76, 0x8E), // 9: Bright Red
        Rgba::from_rgb_u8(0x9E, 0xCE, 0x6A), // 10: Bright Green
        Rgba::from_rgb_u8(0xE0, 0xAF, 0x68), // 11: Bright Yellow
        Rgba::from_rgb_u8(0x7A, 0xA2, 0xF7), // 12: Bright Blue
        Rgba::from_rgb_u8(0xBB, 0x9A, 0xF7), // 13: Bright Magenta
        Rgba::from_rgb_u8(0x7D, 0xCF, 0xFF), // 14: Bright Cyan
        Rgba::from_rgb_u8(0xC0, 0xCA, 0xF5), // 15: Bright White
    ],
};

const GRUVBOX_DARK_THEME: Theme = Theme {
    bg_window: Rgba::from_rgb_u8(0x1D, 0x20, 0x21),
    bg_sidebar: Rgba::from_rgb_u8(0x24, 0x24, 0x24),
    bg_pane: Rgba::from_rgb_u8(0x28, 0x28, 0x28),
    bg_header: Rgba::from_rgb_u8(0x3C, 0x38, 0x36),
    border_unfocused: Rgba::from_rgb_u8(0x50, 0x49, 0x45),
    border_focused: Rgba::from_rgb_u8(0xFE, 0x80, 0x19),
    text_primary: Rgba::from_rgb_u8(0xEB, 0xDB, 0xB2),
    text_muted: Rgba::from_rgb_u8(0xB4, 0xA5, 0x8F),
    status_ok: Rgba::from_rgb_u8(0xB8, 0xBB, 0x26),
    status_busy: Rgba::from_rgb_u8(0xFA, 0xBD, 0x2F),
    status_err: Rgba::from_rgb_u8(0xFF, 0x80, 0x6B),
    selection_bg: Rgba::from_rgba_u8(0x50, 0x49, 0x45, 0x99),
    cursor_bg: Rgba::from_rgb_u8(0xEB, 0xDB, 0xB2),
    picker_bg: Rgba::from_rgba_u8(0x1D, 0x20, 0x21, 0xF5),
    ansi_16: [
        Rgba::from_rgb_u8(0x28, 0x28, 0x28), // 0: Black
        Rgba::from_rgb_u8(0xCC, 0x24, 0x1D), // 1: Red
        Rgba::from_rgb_u8(0x98, 0x97, 0x1A), // 2: Green
        Rgba::from_rgb_u8(0xD7, 0x99, 0x21), // 3: Yellow
        Rgba::from_rgb_u8(0x45, 0x85, 0x88), // 4: Blue
        Rgba::from_rgb_u8(0xB1, 0x62, 0x86), // 5: Magenta
        Rgba::from_rgb_u8(0x68, 0x9D, 0x6A), // 6: Cyan
        Rgba::from_rgb_u8(0xA8, 0x99, 0x84), // 7: White
        Rgba::from_rgb_u8(0x92, 0x83, 0x74), // 8: Bright Black
        Rgba::from_rgb_u8(0xFB, 0x49, 0x34), // 9: Bright Red
        Rgba::from_rgb_u8(0xB8, 0xBB, 0x26), // 10: Bright Green
        Rgba::from_rgb_u8(0xFA, 0xBD, 0x2F), // 11: Bright Yellow
        Rgba::from_rgb_u8(0x83, 0xA5, 0x98), // 12: Bright Blue
        Rgba::from_rgb_u8(0xD3, 0x86, 0x9B), // 13: Bright Magenta
        Rgba::from_rgb_u8(0x8E, 0xC0, 0x7C), // 14: Bright Cyan
        Rgba::from_rgb_u8(0xEB, 0xDB, 0xB2), // 15: Bright White
    ],
};

const SOLARIZED_LIGHT_THEME: Theme = Theme {
    bg_window: Rgba::from_rgb_u8(0xE6, 0xDF, 0xCB),
    bg_sidebar: Rgba::from_rgb_u8(0xEE, 0xE8, 0xD5),
    bg_pane: Rgba::from_rgb_u8(0xFD, 0xF6, 0xE3),
    bg_header: Rgba::from_rgb_u8(0xE6, 0xDF, 0xCB),
    border_unfocused: Rgba::from_rgb_u8(0xD3, 0xCB, 0xB8),
    border_focused: Rgba::from_rgb_u8(0x16, 0x63, 0x9B),
    text_primary: Rgba::from_rgb_u8(0x40, 0x56, 0x5D),
    text_muted: Rgba::from_rgb_u8(0x53, 0x64, 0x6A),
    status_ok: Rgba::from_rgb_u8(0x59, 0x68, 0x00),
    status_busy: Rgba::from_rgb_u8(0x7F, 0x5C, 0x00),
    status_err: Rgba::from_rgb_u8(0xAD, 0x29, 0x26),
    selection_bg: Rgba::from_rgba_u8(0x26, 0x8B, 0xD2, 0x44),
    cursor_bg: Rgba::from_rgb_u8(0x58, 0x6E, 0x75),
    picker_bg: Rgba::from_rgba_u8(0xEE, 0xE8, 0xD5, 0xF5),
    ansi_16: [
        Rgba::from_rgb_u8(0x07, 0x36, 0x42), // 0: Black (base02)
        Rgba::from_rgb_u8(0xDC, 0x32, 0x2F), // 1: Red
        Rgba::from_rgb_u8(0x85, 0x99, 0x00), // 2: Green
        Rgba::from_rgb_u8(0xB5, 0x89, 0x00), // 3: Yellow
        Rgba::from_rgb_u8(0x26, 0x8B, 0xD2), // 4: Blue
        Rgba::from_rgb_u8(0xD3, 0x36, 0x82), // 5: Magenta
        Rgba::from_rgb_u8(0x2A, 0xA1, 0x98), // 6: Cyan
        Rgba::from_rgb_u8(0xEE, 0xE8, 0xD5), // 7: White (base2)
        Rgba::from_rgb_u8(0x00, 0x2B, 0x36), // 8: Bright Black (base03)
        Rgba::from_rgb_u8(0xCB, 0x4B, 0x16), // 9: Bright Red (orange)
        Rgba::from_rgb_u8(0x58, 0x6E, 0x75), // 10: Bright Green (base01)
        Rgba::from_rgb_u8(0x65, 0x7B, 0x83), // 11: Bright Yellow (base00)
        Rgba::from_rgb_u8(0x83, 0x94, 0x96), // 12: Bright Blue (base0)
        Rgba::from_rgb_u8(0x6C, 0x71, 0xC4), // 13: Bright Magenta (violet)
        Rgba::from_rgb_u8(0x93, 0xA1, 0xA1), // 14: Bright Cyan (base1)
        Rgba::from_rgb_u8(0xFD, 0xF6, 0xE3), // 15: Bright White (base3)
    ],
};

impl Theme {
    pub const fn from_preset(preset: ThemeName) -> Self {
        match preset {
            ThemeName::DaedalusDark => DAEDALUS_DARK_THEME,
            ThemeName::CatppuccinMocha => CATPPUCCIN_MOCHA_THEME,
            ThemeName::TokyoNight => TOKYO_NIGHT_THEME,
            ThemeName::GruvboxDark => GRUVBOX_DARK_THEME,
            ThemeName::SolarizedLight => SOLARIZED_LIGHT_THEME,
        }
    }

    pub const fn dark() -> Self {
        Self::from_preset(ThemeName::DaedalusDark)
    }

    pub const fn role_accent(&self, role: PaneRole) -> Rgba {
        match role {
            PaneRole::Agent => self.border_focused,
            PaneRole::Service => self.status_ok,
            PaneRole::Runner => self.status_busy,
        }
    }

    /// Maps an xterm 256-color palette index (`0..=255`) using this theme's 16 ANSI colors.
    pub fn ansi_indexed_color(&self, idx: u8) -> Rgba {
        ansi_indexed_color_with_palette(idx, &self.ansi_16)
    }

    /// Resolves a terminal cell's `(fg, bg)` colors and `CellFlags` using this theme's palette.
    pub fn resolve_cell_colors(
        &self,
        fg: Color,
        bg: Color,
        flags: CellFlags,
    ) -> (Rgba, Option<Rgba>) {
        resolve_cell_colors_with_theme(self, fg, bg, flags)
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

fn ansi_indexed_color_with_palette(idx: u8, ansi_16: &[Rgba; 16]) -> Rgba {
    match idx {
        0..=15 => ansi_16[idx as usize],
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

/// Maps an xterm 256-color palette index (`0..=255`) to an `Rgba` color using the default dark palette.
///
/// - `0..=15`: standard + bright ANSI colors
/// - `16..=231`: 6×6×6 RGB color cube (`0, 95, 135, 175, 215, 255`)
/// - `232..=255`: 24-step grayscale ramp (`8, 18, ..., 238`)
pub fn ansi_indexed_color(idx: u8) -> Rgba {
    ansi_indexed_color_with_palette(idx, &ANSI_16)
}

fn resolve_color(color: Color, default: Rgba, ansi_16: &[Rgba; 16]) -> Rgba {
    match color {
        Color::Default => default,
        Color::Indexed(idx) => ansi_indexed_color_with_palette(idx, ansi_16),
        Color::Rgb(r, g, b) => Rgba::from_rgb_u8(r, g, b),
    }
}

fn resolve_cell_colors_with_theme(
    theme: &Theme,
    fg: Color,
    bg: Color,
    flags: CellFlags,
) -> (Rgba, Option<Rgba>) {
    let (mut resolved_fg, resolved_bg) = if flags.contains(CellFlags::INVERSE) {
        let swapped_fg = resolve_color(bg, theme.bg_pane, &theme.ansi_16);
        let swapped_bg = resolve_color(fg, theme.text_primary, &theme.ansi_16);
        (swapped_fg, Some(swapped_bg))
    } else {
        let fg_rgba = resolve_color(fg, theme.text_primary, &theme.ansi_16);
        let bg_rgba = match bg {
            Color::Default => None,
            other => Some(resolve_color(other, theme.bg_pane, &theme.ansi_16)),
        };
        (fg_rgba, bg_rgba)
    };

    if flags.contains(CellFlags::DIM) {
        resolved_fg = resolved_fg.scale_luminance(DIM_FACTOR);
    }

    (resolved_fg, resolved_bg)
}

/// Resolves a terminal cell's `(fg, bg)` colors and `CellFlags` into concrete RGBA values
/// using the default dark theme.
///
/// Returns `(foreground, background)` where `background` is `None` when the cell uses the
/// pane's default background (allowing the renderer to skip drawing redundant background quads).
///
/// - `CellFlags::INVERSE` swaps foreground and background (resolving default fg/bg to
///   `TEXT_PRIMARY` and `BG_PANE` respectively).
/// - `CellFlags::DIM` scales foreground luminance to 60% (`DIM_FACTOR`).
pub fn resolve_cell_colors(fg: Color, bg: Color, flags: CellFlags) -> (Rgba, Option<Rgba>) {
    resolve_cell_colors_with_theme(&DAEDALUS_DARK_THEME, fg, bg, flags)
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
        let (fg, bg) =
            resolve_cell_colors(Color::Rgb(200, 100, 50), Color::Default, CellFlags::DIM);
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

    #[test]
    fn theme_name_parses_all_presets_case_insensitively() {
        assert_eq!(
            ThemeName::from_name("daedalus-dark"),
            Some(ThemeName::DaedalusDark)
        );
        assert_eq!(
            ThemeName::from_name("  Catppuccin-Mocha "),
            Some(ThemeName::CatppuccinMocha)
        );
        assert_eq!(
            ThemeName::from_name("TOKYO-NIGHT"),
            Some(ThemeName::TokyoNight)
        );
        assert_eq!(
            ThemeName::from_name("gruvbox_dark"),
            Some(ThemeName::GruvboxDark)
        );
        assert_eq!(
            ThemeName::from_name("solarized-light"),
            Some(ThemeName::SolarizedLight)
        );
        assert_eq!(ThemeName::from_name("unknown-theme"), None);
        assert_eq!(ThemeName::from_name(""), None);
    }

    fn srgb_to_linear(c: f32) -> f32 {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }

    fn relative_luminance(c: Rgba) -> f32 {
        0.2126 * srgb_to_linear(c.0[0])
            + 0.7152 * srgb_to_linear(c.0[1])
            + 0.0722 * srgb_to_linear(c.0[2])
    }

    fn contrast_ratio(a: Rgba, b: Rgba) -> f32 {
        let la = relative_luminance(a);
        let lb = relative_luminance(b);
        let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    fn assert_valid_rgba(c: Rgba) {
        for ch in c.0 {
            assert!(ch.is_finite() && (0.0..=1.0).contains(&ch));
        }
    }

    #[test]
    fn chrome_labels_meet_contrast_on_header_sidebar_and_badges() {
        for preset in ThemeName::ALL {
            let theme = Theme::from_preset(preset);
            for (name, color) in [
                ("primary", theme.text_primary),
                ("muted", theme.text_muted),
                ("focused", theme.border_focused),
                ("ok", theme.status_ok),
                ("busy", theme.status_busy),
                ("error", theme.status_err),
            ] {
                for (surface, background) in [
                    ("header", theme.bg_header),
                    ("sidebar", theme.bg_sidebar),
                    ("badge", theme.bg_pane),
                ] {
                    let ratio = contrast_ratio(color, background);
                    assert!(ratio >= 4.5, "{preset} {name} on {surface}: {ratio:.2}:1");
                }
            }
        }
    }

    #[test]
    fn all_five_theme_presets_are_distinct_valid_and_readable() {
        assert_eq!(Theme::dark(), Theme::from_preset(ThemeName::DaedalusDark));

        let themes: Vec<Theme> = ThemeName::ALL
            .iter()
            .map(|&name| Theme::from_preset(name))
            .collect();

        // All 5 themes have distinct pane backgrounds, valid channels, and WCAG AA contrast
        for (i, theme) in themes.iter().enumerate() {
            assert_valid_rgba(theme.bg_window);
            assert_valid_rgba(theme.bg_sidebar);
            assert_valid_rgba(theme.bg_pane);
            assert_valid_rgba(theme.bg_header);
            assert_valid_rgba(theme.border_unfocused);
            assert_valid_rgba(theme.border_focused);
            assert_valid_rgba(theme.text_primary);
            assert_valid_rgba(theme.text_muted);
            assert_valid_rgba(theme.status_ok);
            assert_valid_rgba(theme.status_busy);
            assert_valid_rgba(theme.status_err);
            assert_valid_rgba(theme.selection_bg);
            assert_valid_rgba(theme.cursor_bg);
            assert_valid_rgba(theme.picker_bg);
            for ansi in theme.ansi_16 {
                assert_valid_rgba(ansi);
            }

            let bg_lum = relative_luminance(theme.bg_pane);
            let fg_lum = relative_luminance(theme.text_primary);
            let ratio = contrast_ratio(theme.text_primary, theme.bg_pane);
            assert!(
                ratio >= 4.5,
                "theme {:?} lacks WCAG AA foreground/background contrast (ratio={ratio:.2})",
                ThemeName::ALL[i]
            );

            if ThemeName::ALL[i] == ThemeName::SolarizedLight {
                assert!(bg_lum > 0.8 && fg_lum < 0.25);
            } else {
                assert!(bg_lum < 0.1 && fg_lum > 0.4);
            }

            for (j, other) in themes.iter().enumerate() {
                if i != j {
                    assert_ne!(theme.bg_pane, other.bg_pane);
                }
            }
        }
    }

    #[test]
    fn theme_resolve_cell_colors_respects_preset_palette() {
        let light = Theme::from_preset(ThemeName::SolarizedLight);
        let (fg, bg) =
            light.resolve_cell_colors(Color::Default, Color::Default, CellFlags::empty());
        assert_eq!(fg, light.text_primary);
        assert_eq!(bg, None);

        let (fg_inv, bg_inv) =
            light.resolve_cell_colors(Color::Default, Color::Default, CellFlags::INVERSE);
        assert_eq!(fg_inv, light.bg_pane);
        assert_eq!(bg_inv, Some(light.text_primary));

        let mocha = Theme::from_preset(ThemeName::CatppuccinMocha);
        let (fg_ansi, _) =
            mocha.resolve_cell_colors(Color::Indexed(1), Color::Default, CellFlags::empty());
        assert_eq!(fg_ansi, mocha.ansi_16[1]);
    }
}
