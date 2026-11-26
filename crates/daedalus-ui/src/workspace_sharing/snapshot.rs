use super::DaedalusApp;
use daedalus_proto::workbench::{WorkbenchState, WorkspaceDocument};
use daedalus_proto::{ClientMessage, WorkspaceId};
use daedalus_ui::bridge::WorkbenchOperation;
use daedalus_ui::workspace_widgets::{self, WorkspaceWidgets};

impl DaedalusApp {
    pub(super) fn load_workbench(&mut self, id: WorkspaceId, state: WorkbenchState) {
        if !self
            .state
            .workspaces
            .iter()
            .any(|workspace| workspace.id == id)
        {
            return;
        }
        let settings = self.state.settings.open && self.state.settings.widget_workspace == Some(id);
        if self.workbench_snapshot_is_stale(id, state.revision, settings) {
            self.flush_workbench_layout(id);
            return;
        }
        if let Some(save) = self.layout_saves.get_mut(&id) {
            save.rebase(&state);
        }

        if state.revision == 0 && !self.layout_saves.contains_key(&id) {
            self.legacy_workbench_ready(id, &state.document);
        }

        self.update_settings_snapshot(id, &state, settings);
        if state.revision > 0 {
            self.project_setups
                .workspace_changed(id, state.document.assignments.clone());
            self.refresh_workspace_setup(id);
        }
        if self.state.active_workspace_id == Some(id) {
            if state.revision > 0
                && let Some(workspace) = self.state.active_workspace_mut()
            {
                workspace.name.clone_from(&state.document.name);
            }
            self.reconcile_workbench_layout(id, &state.document.layout);
            if state.revision > 0 {
                self.request_workbench(
                    id,
                    ClientMessage::GetWidgetResults {
                        workspace_id: id,
                        refresh: None,
                    },
                );
            }
            self.state.workbench = Some((id, state));
            self.schedule_resize_flush();
        }
        self.flush_workbench_layout(id);
    }

    fn workbench_snapshot_is_stale(&self, id: WorkspaceId, revision: u64, settings: bool) -> bool {
        let current = if settings {
            self.state.settings.custom.state.as_ref()
        } else {
            self.state
                .workbench
                .as_ref()
                .filter(|(workspace, _)| *workspace == id)
                .map(|(_, state)| state)
        };
        let pending_base = self
            .layout_saves
            .get(&id)
            .and_then(|save| save.base.as_ref());
        current
            .into_iter()
            .chain(pending_base)
            .any(|current| current.revision > revision)
    }

    fn update_settings_snapshot(
        &mut self,
        id: WorkspaceId,
        state: &WorkbenchState,
        settings: bool,
    ) {
        self.state.workspace_widgets.remove(&id);
        let effective = state
            .document
            .pane_widgets
            .clone()
            .map(WorkspaceWidgets::from);
        if let Some(widgets) = &effective {
            self.state.workspace_widgets.insert(id, widgets.clone());
        }
        if settings {
            let dialog = &mut self.state.settings;
            dialog.receive_pane_widget_snapshot(effective.unwrap_or_default());
            dialog.custom.accept_snapshot(state.clone());
        }
    }

    // A layout write at revision zero must wait for the legacy widget seed.
    pub(super) fn legacy_workbench_ready(
        &mut self,
        id: WorkspaceId,
        initial: &WorkspaceDocument,
    ) -> bool {
        if self.seeding_workbench.contains(&id) {
            return false;
        }
        match workspace_widgets::load(&self.widget_directory, id) {
            Ok(None) => true,
            Ok(Some(legacy)) => {
                let mut document = initial.clone();
                document.pane_widgets = Some(legacy.into());
                if self.request_workbench_as(
                    id,
                    WorkbenchOperation::Migration,
                    ClientMessage::SeedLegacyWorkbench {
                        workspace_id: id,
                        document,
                    },
                ) {
                    self.seeding_workbench.insert(id);
                }
                false
            }
            Err(error) => {
                self.show_toast(
                    format!("Could not read legacy widget settings: {error}"),
                    true,
                );
                false
            }
        }
    }
}
