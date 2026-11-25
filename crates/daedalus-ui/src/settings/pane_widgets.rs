use super::{Control, SettingsAction, SettingsDialog};
use Control::*;
use daedalus_proto::PaneRole;

impl SettingsDialog {
    pub fn set_automatic_widgets(&mut self, service: bool, runner: bool) {
        self.automatic_service_widget = service;
        self.automatic_runner_widget = runner;
    }

    fn widget_present(&self, role: PaneRole) -> bool {
        let automatic = match role {
            PaneRole::Service => self.automatic_service_widget,
            PaneRole::Runner => self.automatic_runner_widget,
            PaneRole::Agent => false,
        };
        self.widgets.includes(role, automatic)
    }

    pub(super) fn pane_widget_rows(&self, pages: Vec<Control>) -> Vec<Vec<Control>> {
        let mut rows = vec![pages];
        if self.widget_present(PaneRole::Service) {
            rows.extend([vec![ServicePreset, ServiceWidget], vec![ServiceLabel]]);
        } else {
            rows.push(vec![ServiceWidget]);
        }
        if self.widget_present(PaneRole::Runner) {
            rows.extend([
                vec![RunnerPreset, RunnerWidget],
                vec![RunnerLabel],
                vec![RunnerDuration, RunnerWarnings],
            ]);
        } else {
            rows.push(vec![RunnerWidget]);
        }
        rows.push(vec![Cancel, Save]);
        rows
    }

    pub(super) fn pane_widget_label(&self, c: Control) -> Option<String> {
        Some(match c {
            ServiceWidget | RunnerWidget => {
                let (role, name) = if c == ServiceWidget {
                    (PaneRole::Service, "service")
                } else {
                    (PaneRole::Runner, "runner")
                };
                let action = if self.widget_present(role) {
                    "Remove"
                } else {
                    "+ Add"
                };
                format!("{action} {name} widget")
            }
            ServicePreset => format!(
                "Service preset   {}   ↻",
                preset_label(self.widgets.service.preset.as_deref())
            ),
            RunnerPreset => format!(
                "Runner preset   {}   ↻",
                preset_label(self.widgets.runner.preset.as_deref())
            ),
            ServiceLabel | RunnerLabel => format!(
                "{}   {}",
                c.field_label().unwrap(),
                self.value(c).unwrap_or_default()
            ),
            RunnerDuration => format!(
                "Duration · {}",
                detail_label(self.widgets.runner.show_duration)
            ),
            RunnerWarnings => format!(
                "Warnings · {}",
                detail_label(self.widgets.runner.show_warnings)
            ),
            Cancel if self.is_pane_widgets_page() && self.widgets_saved => "Done".into(),
            Save if self.is_pane_widgets_page() => "Save widgets".into(),
            _ => return None,
        })
    }

    pub(super) fn pane_widget_enabled(&self, c: Control) -> bool {
        match c {
            ServiceWidget | RunnerWidget => true,
            ServicePreset | ServiceLabel => self.widget_present(PaneRole::Service),
            RunnerPreset | RunnerLabel | RunnerDuration | RunnerWarnings => {
                self.widget_present(PaneRole::Runner)
            }
            _ => true,
        }
    }

    pub(super) fn pane_widget_field(&mut self) -> Option<&mut String> {
        self.widgets_saved = false;
        match self.focus {
            ServiceLabel => Some(self.widgets.service.label.get_or_insert_with(String::new)),
            RunnerLabel => Some(self.widgets.runner.label.get_or_insert_with(String::new)),
            _ => None,
        }
    }

    pub(super) fn pane_widget_value(&self, control: Control) -> Option<&str> {
        match control {
            ServiceLabel => Some(self.widgets.service.label.as_deref().unwrap_or_default()),
            RunnerLabel => Some(self.widgets.runner.label.as_deref().unwrap_or_default()),
            _ => None,
        }
    }

    pub(super) fn apply_pane_widget_control(&mut self, c: Control) -> SettingsAction {
        if matches!(
            c,
            ServiceWidget
                | RunnerWidget
                | ServicePreset
                | RunnerPreset
                | RunnerDuration
                | RunnerWarnings
        ) {
            self.widgets_saved = false;
        }
        match c {
            ServiceWidget => {
                let add = !self.widget_present(PaneRole::Service);
                self.widgets.service_enabled = Some(add);
                if !add {
                    self.widgets.service = Default::default();
                }
            }
            RunnerWidget => {
                let add = !self.widget_present(PaneRole::Runner);
                self.widgets.runner_enabled = Some(add);
                if !add {
                    self.widgets.runner = Default::default();
                }
            }
            ServicePreset => {
                cycle_preset(&mut self.widgets.service.preset, &["service", "command"])
            }
            RunnerPreset => cycle_preset(
                &mut self.widgets.runner.preset,
                &["cargo-test", "cargo-check", "command"],
            ),
            RunnerDuration => cycle_detail(&mut self.widgets.runner.show_duration),
            RunnerWarnings => cycle_detail(&mut self.widgets.runner.show_warnings),
            ServiceLabel | RunnerLabel => self.select_all = true,
            Save => {
                let mut widgets = self.widgets.clone();
                for config in [&mut widgets.service, &mut widgets.runner] {
                    config.label = config
                        .label
                        .as_deref()
                        .map(str::trim)
                        .filter(|label| !label.is_empty())
                        .map(str::to_owned);
                }
                return SettingsAction::SaveWidgets(widgets);
            }
            _ => {}
        }
        SettingsAction::None
    }
}

fn preset_label(preset: Option<&str>) -> &'static str {
    match preset {
        Some("service") => "Service",
        Some("command") => "Command",
        Some("cargo-test") => "Cargo Test",
        Some("cargo-check") => "Cargo Check",
        _ => "Default",
    }
}

fn cycle_preset(preset: &mut Option<String>, choices: &[&str]) {
    let next = preset
        .as_deref()
        .and_then(|current| choices.iter().position(|&choice| choice == current))
        .map_or(0, |index| index + 1);
    *preset = choices.get(next).map(|choice| (*choice).to_owned());
}

fn detail_label(value: Option<bool>) -> &'static str {
    match value {
        None => "Default",
        Some(true) => "Show",
        Some(false) => "Hide",
    }
}

fn cycle_detail(value: &mut Option<bool>) {
    *value = match *value {
        None => Some(true),
        Some(true) => Some(false),
        Some(false) => None,
    };
}
