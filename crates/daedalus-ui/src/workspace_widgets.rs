use daedalus_proto::{PaneRole, WidgetConfig, WorkspaceId};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fs, io::ErrorKind, path::Path};
use tracing::warn;

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct WorkspaceWidgets {
    pub service: WidgetConfig,
    pub runner: WidgetConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_enabled: Option<bool>,
    pub hide_cargo_test: bool,
    pub hide_cargo_check: bool,
}

impl WorkspaceWidgets {
    pub(crate) fn for_role(&self, role: PaneRole) -> Option<&WidgetConfig> {
        match role {
            PaneRole::Service => Some(&self.service),
            PaneRole::Runner => Some(&self.runner),
            _ => None,
        }
    }

    pub(crate) fn includes(&self, role: PaneRole, automatic: bool) -> bool {
        match role {
            PaneRole::Service => self.service_enabled.unwrap_or(automatic),
            PaneRole::Runner => self.runner_enabled.unwrap_or(automatic),
            PaneRole::Agent => false,
        }
    }
}

impl From<WorkspaceWidgets> for daedalus_proto::workbench::PaneWidgets {
    fn from(widgets: WorkspaceWidgets) -> Self {
        Self {
            service: widgets.service,
            runner: widgets.runner,
            service_enabled: widgets.service_enabled,
            runner_enabled: widgets.runner_enabled,
            hide_cargo_test: widgets.hide_cargo_test,
            hide_cargo_check: widgets.hide_cargo_check,
        }
    }
}

impl From<daedalus_proto::workbench::PaneWidgets> for WorkspaceWidgets {
    fn from(widgets: daedalus_proto::workbench::PaneWidgets) -> Self {
        Self {
            service: widgets.service,
            runner: widgets.runner,
            service_enabled: widgets.service_enabled,
            runner_enabled: widgets.runner_enabled,
            hide_cargo_test: widgets.hide_cargo_test,
            hide_cargo_check: widgets.hide_cargo_check,
        }
    }
}

/// Each workspace has its own file so saves in different workspaces are independent.
pub fn load_all(directory: &Path) -> HashMap<WorkspaceId, WorkspaceWidgets> {
    let files = match fs::read_dir(directory) {
        Ok(files) => files,
        Err(err) if err.kind() == ErrorKind::NotFound => return HashMap::new(),
        Err(err) => {
            warn!(%err, path = %directory.display(), "could not read workspace widgets");
            return HashMap::new();
        }
    };
    files
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            if path.extension()?.to_str()? != "toml" {
                return None;
            }
            let id = WorkspaceId(uuid::Uuid::parse_str(path.file_stem()?.to_str()?).ok()?);
            match load_from(&path) {
                Ok(Some(widgets)) => Some((id, widgets)),
                Ok(None) => None,
                Err(err) => {
                    warn!(%err, path = %path.display(), "could not load workspace widgets");
                    None
                }
            }
        })
        .collect()
}

pub fn load(directory: &Path, id: WorkspaceId) -> Result<Option<WorkspaceWidgets>, String> {
    load_from(&directory.join(format!("{id}.toml")))
}

fn load_from(path: &Path) -> Result<Option<WorkspaceWidgets>, String> {
    match fs::read_to_string(path) {
        Ok(contents) => toml::from_str(&contents)
            .map(Some)
            .map_err(|err| err.to_string()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.to_string()),
    }
}

pub fn save(
    directory: &Path,
    workspace: WorkspaceId,
    widgets: &WorkspaceWidgets,
) -> Result<(), String> {
    update(directory, workspace, |_| Ok(widgets.clone())).map(|_| ())
}

pub fn update(
    directory: &Path,
    workspace: WorkspaceId,
    edit: impl FnOnce(WorkspaceWidgets) -> Result<WorkspaceWidgets, String>,
) -> Result<WorkspaceWidgets, String> {
    let path = directory.join(format!("{workspace}.toml"));
    let _lock = crate::assignments::lock_for_write(&path)?;
    let widgets = edit(load_from(&path)?.unwrap_or_default())?;
    let contents = toml::to_string_pretty(&widgets)
        .map_err(|err| format!("Could not serialize workspace widget settings: {err}"))?;
    crate::assignments::write_atomic(&path, &contents)?;
    Ok(widgets)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widget_overrides_round_trip_and_stay_in_their_workspace() {
        let directory = tempfile::tempdir().unwrap();
        let id = WorkspaceId::new();
        let widgets = WorkspaceWidgets {
            runner: WidgetConfig {
                preset: Some("cargo-check".into()),
                label: Some("Compiler".into()),
                show_warnings: Some(false),
                ..Default::default()
            },
            ..Default::default()
        };
        save(directory.path(), id, &widgets).unwrap();
        assert_eq!(load_all(directory.path()), HashMap::from([(id, widgets)]));
        save(directory.path(), id, &WorkspaceWidgets::default()).unwrap();
        assert_eq!(load_all(directory.path())[&id], WorkspaceWidgets::default());
    }
}
