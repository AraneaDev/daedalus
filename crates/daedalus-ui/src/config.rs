use std::io::ErrorKind;
use std::path::Path;

use daedalus_proto::default_config_file_path;
use serde::Deserialize;
use tracing::warn;

use crate::state::AgentPreset;
use crate::theme::ThemeName;

pub const DEFAULT_FONT_SIZE: f32 = 14.0;
pub const MIN_FONT_SIZE: f32 = 8.0;
pub const MAX_FONT_SIZE: f32 = 32.0;

/// Clamps `size` to `[MIN_FONT_SIZE, MAX_FONT_SIZE]`, falling back to `DEFAULT_FONT_SIZE` if non-finite.
pub fn clamp_font_size(size: f32) -> f32 {
    if size.is_finite() {
        size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE)
    } else {
        DEFAULT_FONT_SIZE
    }
}

/// User-level UI configuration loaded from `~/.config/daedalus/config.toml`.
#[derive(Debug, Clone, PartialEq)]
pub struct UiConfig {
    pub font_family: Option<String>,
    pub font_size: f32,
    pub default_agent: AgentPreset,
    pub theme: ThemeName,
    pub notification_ping: bool,
    pub reduced_motion: bool,
    pub editor: Option<String>,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            font_family: None,
            font_size: DEFAULT_FONT_SIZE,
            default_agent: AgentPreset::Shell,
            theme: ThemeName::DaedalusDark,
            notification_ping: false,
            reduced_motion: false,
            editor: None,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct RawUiConfig {
    #[serde(default)]
    font_family: Option<String>,
    #[serde(default)]
    font_size: Option<f32>,
    #[serde(default)]
    default_agent: Option<String>,
    #[serde(default)]
    theme: Option<String>,
    #[serde(default)]
    notification_ping: bool,
    #[serde(default)]
    reduced_motion: bool,
    #[serde(default)]
    editor: Option<String>,
}

fn normalize_non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

impl UiConfig {
    /// Parses `config.toml` contents into a [`UiConfig`], clamping out-of-range values
    /// and falling back cleanly for unknown enum strings.
    pub fn parse(contents: &str) -> Result<Self, toml::de::Error> {
        let raw: RawUiConfig = toml::from_str(contents)?;
        let defaults = Self::default();

        let font_size = raw
            .font_size
            .map(clamp_font_size)
            .unwrap_or(defaults.font_size);

        let default_agent = match normalize_non_empty(raw.default_agent) {
            Some(name) => match AgentPreset::from_cli_name(&name) {
                Some(preset) => preset,
                None => {
                    warn!(
                        default_agent = %name,
                        "unknown default_agent in config.toml; falling back to default"
                    );
                    defaults.default_agent
                }
            },
            None => defaults.default_agent,
        };

        let theme = match normalize_non_empty(raw.theme) {
            Some(name) => match ThemeName::from_name(&name) {
                Some(preset) => preset,
                None => {
                    warn!(
                        theme = %name,
                        "unknown theme in config.toml; falling back to daedalus-dark"
                    );
                    defaults.theme
                }
            },
            None => defaults.theme,
        };

        Ok(Self {
            font_family: normalize_non_empty(raw.font_family),
            font_size,
            default_agent,
            theme,
            notification_ping: raw.notification_ping,
            reduced_motion: raw.reduced_motion,
            editor: normalize_non_empty(raw.editor),
        })
    }

    /// Loads UI configuration from `path`, or `daedalus_proto::default_config_file_path()`
    /// when `path` is `None`.
    ///
    /// Missing files return [`UiConfig::default()`] silently. Unreadable or malformed TOML
    /// files log a warning via `tracing::warn!` and return [`UiConfig::default()`].
    pub fn load(path: Option<&Path>) -> Self {
        let owned_default;
        let resolved = match path {
            Some(p) => p,
            None => {
                owned_default = default_config_file_path();
                &owned_default
            }
        };

        let contents = match std::fs::read_to_string(resolved) {
            Ok(c) => c,
            Err(err) if err.kind() == ErrorKind::NotFound => return Self::default(),
            Err(err) => {
                warn!(
                    path = %resolved.display(),
                    error = %err,
                    "failed to read config.toml; using defaults"
                );
                return Self::default();
            }
        };

        match Self::parse(&contents) {
            Ok(cfg) => cfg,
            Err(err) => {
                warn!(
                    path = %resolved.display(),
                    error = %err,
                    "malformed config.toml; using defaults"
                );
                Self::default()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn loads_valid_toml_config() {
        let dir = tempdir().unwrap();
        let cfg_path = dir.path().join("config.toml");
        std::fs::write(
            &cfg_path,
            r#"
notification_ping = true
reduced_motion = true
font_family = "JetBrains Mono"
font_size = 16.5
default_agent = "claude"
theme = "catppuccin-mocha"
editor = "code --goto {path}:{line}:{col}"
"#,
        )
        .unwrap();

        let cfg = UiConfig::load(Some(&cfg_path));
        assert!(cfg.notification_ping && cfg.reduced_motion);
        assert_eq!(cfg.font_family.as_deref(), Some("JetBrains Mono"));
        assert_eq!(cfg.font_size, 16.5);
        assert_eq!(cfg.default_agent, AgentPreset::Claude);
        assert_eq!(cfg.theme, ThemeName::CatppuccinMocha);
        assert_eq!(
            cfg.editor.as_deref(),
            Some("code --goto {path}:{line}:{col}")
        );
    }

    #[test]
    fn missing_file_returns_defaults() {
        let dir = tempdir().unwrap();
        let cfg_path = dir.path().join("does-not-exist.toml");
        assert_eq!(UiConfig::load(Some(&cfg_path)), UiConfig::default());
    }

    #[test]
    fn malformed_toml_returns_defaults() {
        let dir = tempdir().unwrap();
        let cfg_path = dir.path().join("config.toml");
        std::fs::write(&cfg_path, "font_size = \"not-a-number\"\n[broken").unwrap();
        assert_eq!(UiConfig::load(Some(&cfg_path)), UiConfig::default());
    }

    #[test]
    fn clamps_font_size_and_normalizes_empty_strings() {
        let small = UiConfig::parse(
            r#"
font_family = "   "
font_size = 3.0
editor = ""
"#,
        )
        .unwrap();
        assert_eq!(small.font_family, None);
        assert_eq!(small.font_size, MIN_FONT_SIZE);
        assert_eq!(small.editor, None);

        let large = UiConfig::parse("font_size = 96.0").unwrap();
        assert_eq!(large.font_size, MAX_FONT_SIZE);

        let nan_cfg = UiConfig::parse("font_size = nan").unwrap();
        assert_eq!(nan_cfg.font_size, DEFAULT_FONT_SIZE);
    }

    #[test]
    fn unknown_theme_and_agent_fall_back_gracefully() {
        let cfg = UiConfig::parse(
            r#"
font_size = 15.0
theme = "nonexistent-theme"
default_agent = "unknown-agent"
"#,
        )
        .unwrap();

        assert_eq!(cfg.font_size, 15.0);
        assert_eq!(cfg.theme, ThemeName::DaedalusDark);
        assert_eq!(cfg.default_agent, AgentPreset::Shell);
    }
}
