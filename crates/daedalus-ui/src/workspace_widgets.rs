use daedalus_proto::{PaneRole, WidgetConfig, WorkspaceId};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fs, io::ErrorKind, path::Path};
use tracing::warn;

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct WorkspaceWidgets {
    pub service: WidgetConfig,
    pub runner: WidgetConfig,
}

impl WorkspaceWidgets {
    pub(crate) fn for_role(&self, role: PaneRole) -> Option<&WidgetConfig> {
        match role {
            PaneRole::Service => Some(&self.service),
            PaneRole::Runner => Some(&self.runner),
            _ => None,
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
            let result = fs::read_to_string(&path)
                .map_err(|err| err.to_string())
                .and_then(|contents| toml::from_str(&contents).map_err(|err| err.to_string()));
            match result {
                Ok(widgets) => Some((id, widgets)),
                Err(err) => {
                    warn!(%err, path = %path.display(), "could not load workspace widgets");
                    None
                }
            }
        })
        .collect()
}

pub fn save(
    directory: &Path,
    workspace: WorkspaceId,
    widgets: &WorkspaceWidgets,
) -> Result<(), String> {
    let path = directory.join(format!("{workspace}.toml"));
    let contents = toml::to_string_pretty(widgets)
        .map_err(|err| format!("Could not serialize workspace widget settings: {err}"))?;
    crate::assignments::write_atomic(&path, &contents)
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
