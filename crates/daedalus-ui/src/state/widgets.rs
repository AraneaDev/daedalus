use super::AppState;
use crate::assignments::CargoPreset;
use daedalus_proto::{PaneRole, PaneStatus, WidgetConfig};

impl AppState {
    pub fn has_widget_source(&self, role: PaneRole) -> bool {
        matches!(role, PaneRole::Service | PaneRole::Runner)
            && (self.pane_for_role(role).is_some()
                || self
                    .active_workspace_id
                    .and_then(|id| self.workspace_widgets.get(&id))
                    .and_then(|widgets| widgets.for_role(role))
                    .is_some_and(|config| config != &WidgetConfig::default()))
    }

    pub fn widget_present(&self, role: PaneRole) -> bool {
        let automatic = self.has_widget_source(role);
        self.active_workspace_id
            .and_then(|id| self.workspace_widgets.get(&id))
            .map_or(automatic, |widgets| widgets.includes(role, automatic))
    }

    fn widget_configs(&self, role: PaneRole) -> impl Iterator<Item = &WidgetConfig> + Clone {
        let workspace = self.active_workspace();
        let overrides = workspace
            .and_then(|ws| self.workspace_widgets.get(&ws.id))
            .and_then(|widgets| widgets.for_role(role));
        let defaults =
            workspace
                .and_then(|ws| ws.manifest.as_ref())
                .and_then(|manifest| match role {
                    PaneRole::Runner => manifest.runner_widget.as_ref(),
                    PaneRole::Service => manifest.service_widget.as_ref(),
                    _ => None,
                });
        [overrides, defaults].into_iter().flatten()
    }

    pub fn legacy_cargo_shortcuts_available(&self) -> bool {
        !self.workbench_capable
            && self
                .active_workspace()
                .is_some_and(|ws| ws.project.tools.iter().any(|tool| tool == "Cargo"))
    }

    pub fn cargo_preset_visible(&self, preset: CargoPreset) -> bool {
        self.legacy_cargo_shortcuts_available()
            && !self
                .active_workspace_id
                .and_then(|id| self.workspace_widgets.get(&id))
                .is_some_and(|widgets| match preset {
                    CargoPreset::Test => widgets.hide_cargo_test,
                    CargoPreset::Check => widgets.hide_cargo_check,
                })
    }

    /// Single-line summary shared by terminal headers, the focused view and accessibility.
    pub fn widget_label(&self, role: PaneRole) -> Option<String> {
        if !self.widget_present(role) {
            return None;
        }
        let ws = self.active_workspace()?;
        let pane = self.pane_for_role(role);
        let manifest = ws.manifest.as_ref();
        let command = match role {
            PaneRole::Runner => manifest.and_then(|m| m.runner_command.as_ref()),
            PaneRole::Service => manifest.and_then(|m| m.service_command.as_ref()),
            _ => return None,
        };
        let configs = self.widget_configs(role);
        let preset = configs
            .clone()
            .find_map(|c| c.preset.as_deref())
            .or_else(|| {
                if let Some(preset) = pane.and_then(|pane| pane.cargo_preset) {
                    return Some(preset.launch_tag());
                }
                let args = command?;
                daedalus_proto::cargo_launch_preset(args.first()?, args.get(1).map(String::as_str))
            });
        let name = configs
            .clone()
            .find_map(|c| c.label.as_deref().filter(|s| !s.trim().is_empty()))
            .or_else(|| {
                pane.and_then(|pane| self.session_names.get(&pane.summary.id))
                    .map(String::as_str)
            })
            .or_else(|| {
                pane.and_then(|pane| pane.cargo_preset)
                    .map(crate::assignments::CargoPreset::label)
            })
            .unwrap_or(match (role, preset) {
                (_, Some("cargo-test")) => "Workspace tests",
                (_, Some("cargo-check")) => "Compiler",
                (PaneRole::Service, _) => "Service",
                _ => "Runner",
            });
        let Some(pane) = pane else {
            return Some(format!("{name} · No terminal"));
        };
        let status = match pane.summary.status {
            PaneStatus::Exited { code: Some(0) } => "Completed",
            PaneStatus::Exited { code: Some(_) } => "Failed",
            PaneStatus::Exited { code: None } => "Stopped",
            PaneStatus::Starting => "Starting",
            PaneStatus::Busy => "Running",
            PaneStatus::Idle => "Idle",
        };
        let mut text = format!("{name} · ");
        if role == PaneRole::Service {
            text.push_str(status);
            if preset != Some("command")
                && !matches!(pane.summary.status, PaneStatus::Exited { .. })
                && let Some(port) = ws.bound_ports.first()
            {
                text.push_str(&format!(" · workspace :{}", port.host_port));
            }
        } else if let Some(info) = &pane.insight {
            let phase = if matches!(pane.summary.status, PaneStatus::Exited { .. })
                || info.phase.is_empty()
                || (preset == Some("command")
                    && !matches!(info.phase.as_str(), "Failed" | "Passed" | "Stopped"))
            {
                status
            } else {
                &info.phase
            };
            text.push_str(phase);
            if preset != Some("command") {
                if info.passed > 0 {
                    text.push_str(&format!(" · {} passed", info.passed));
                }
                if info.failed > 0 {
                    text.push_str(&format!(" · {} failed", info.failed));
                }
                if configs
                    .clone()
                    .find_map(|c| c.show_warnings)
                    .unwrap_or(preset != Some("cargo-test"))
                    && info.warnings > 0
                {
                    text.push_str(&format!(" · {} warnings", info.warnings));
                }
            }
            if configs
                .clone()
                .find_map(|c| c.show_duration)
                .unwrap_or(true)
                && info.elapsed_secs > 0
            {
                text.push_str(&format!(" · {}s", info.elapsed_secs));
            }
        } else {
            text.push_str(status);
        }
        Some(text)
    }

    pub fn widget_diagnostic(&self) -> Option<&str> {
        if !self.widget_present(PaneRole::Runner)
            || self
                .widget_configs(PaneRole::Runner)
                .find_map(|w| w.preset.as_deref())
                == Some("command")
        {
            return None;
        }
        let runner = self.pane_for_role(PaneRole::Runner)?;
        let info = runner.insight.as_ref()?;
        (info.phase == "Failed")
            .then_some(info.diagnostic.as_deref())
            .flatten()
    }

    pub fn focused_widget_label(&self, role: PaneRole) -> Option<String> {
        let mut text = self.widget_label(role)?;
        if role == PaneRole::Runner
            && let Some(detail) = self.widget_diagnostic()
        {
            if self.pane_for_role(role)?.show_diagnostic {
                return Some(format!("‹  {detail}"));
            }
            text.push_str(" · View failure ›");
        }
        Some(text)
    }
}
