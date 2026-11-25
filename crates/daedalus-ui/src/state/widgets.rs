use super::AppState;
use daedalus_proto::{PaneRole, PaneStatus};

impl AppState {
    pub fn show_cargo_bar(&self) -> bool {
        self.active_workspace()
            .is_some_and(|ws| ws.project.tools.iter().any(|tool| tool == "Cargo"))
    }

    /// Single-line summary shared by terminal headers, the focused view and accessibility.
    pub fn widget_label(&self, role: PaneRole) -> Option<String> {
        let ws = self.active_workspace()?;
        let pane = self.pane_for_role(role)?;
        let manifest = ws.manifest.as_ref();
        let (config, command) = match role {
            PaneRole::Runner => (
                manifest.and_then(|m| m.runner_widget.as_ref()),
                manifest.and_then(|m| m.runner_command.as_ref()),
            ),
            PaneRole::Service => (
                manifest.and_then(|m| m.service_widget.as_ref()),
                manifest.and_then(|m| m.service_command.as_ref()),
            ),
            _ => return None,
        };
        let preset = config.and_then(|c| c.preset.as_deref()).or_else(|| {
            if let Some(preset) = pane.cargo_preset {
                return Some(preset.launch_tag());
            }
            let args = command?;
            daedalus_proto::cargo_launch_preset(args.first()?, args.get(1).map(String::as_str))
        });
        let name = config
            .and_then(|c| c.label.as_deref())
            .filter(|s| !s.trim().is_empty())
            .or_else(|| self.session_names.get(&pane.summary.id).map(String::as_str))
            .or_else(|| {
                pane.cargo_preset
                    .map(crate::assignments::CargoPreset::label)
            })
            .unwrap_or(match (role, preset) {
                (_, Some("cargo-test")) => "Workspace tests",
                (_, Some("cargo-check")) => "Compiler",
                (PaneRole::Service, _) => "Service",
                _ => "Runner",
            });
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
                if config
                    .and_then(|c| c.show_warnings)
                    .unwrap_or(preset != Some("cargo-test"))
                    && info.warnings > 0
                {
                    text.push_str(&format!(" · {} warnings", info.warnings));
                }
            }
            if config.and_then(|c| c.show_duration).unwrap_or(true) && info.elapsed_secs > 0 {
                text.push_str(&format!(" · {}s", info.elapsed_secs));
            }
        } else {
            text.push_str(status);
        }
        Some(text)
    }

    pub fn widget_diagnostic(&self) -> Option<&str> {
        if self
            .active_workspace()?
            .manifest
            .as_ref()
            .and_then(|m| m.runner_widget.as_ref())
            .and_then(|w| w.preset.as_deref())
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
