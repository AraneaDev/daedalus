use super::DaedalusApp;
use daedalus_proto::workbench::{WorkbenchLayout, WorkbenchState};
use daedalus_proto::{ClientMessage, ResponsePayload, WorkspaceId};
use daedalus_ui::bridge::{UiCommand, WorkbenchOperation};
use daedalus_ui::layout::PaneSplits;
use daedalus_ui::state::ConnectionState;
use std::collections::BTreeSet;
use uuid::Uuid;

pub(crate) struct LayoutSave {
    pub(super) desired: WorkbenchLayout,
    pub(super) base: Option<WorkbenchState>,
    in_flight: Option<Uuid>,
    edited_view: bool,
    edited_splits: BTreeSet<String>,
}

impl LayoutSave {
    fn record(&mut self, desired: WorkbenchLayout) {
        self.edited_splits.extend(
            desired
                .splits
                .iter()
                .filter(|(key, value)| self.desired.splits.get(*key) != Some(*value))
                .map(|(key, _)| key.clone()),
        );
        self.edited_view |=
            self.desired.split_view != desired.split_view || !self.edited_splits.is_empty();
        self.desired = desired;
    }

    fn project(&self, confirmed: &WorkbenchLayout) -> WorkbenchLayout {
        let mut layout = confirmed.clone();
        if self.edited_view {
            layout.split_view = self.desired.split_view;
        }
        layout.splits.extend(
            self.edited_splits
                .iter()
                .map(|key| (key.clone(), self.desired.splits[key])),
        );
        layout
    }

    pub(super) fn rebase(&mut self, state: &WorkbenchState) {
        let layout = self.project(&state.document.layout);
        self.desired = normalize_layout(&layout);
        self.base = Some(state.clone());
    }

    fn pause(&mut self) {
        self.in_flight = None;
        self.base = None;
    }
}

fn normalize_layout(layout: &WorkbenchLayout) -> WorkbenchLayout {
    PaneSplits::from_portable(layout).portable(layout.split_view)
}

impl DaedalusApp {
    pub(crate) fn prepare_active_workbench_layout(&mut self) {
        if !self.workbench_capable || self.layout_workspace == self.state.active_workspace_id {
            return;
        }
        self.layout_workspace = self.state.active_workspace_id;
        self.pointer.end_drag();
        let layout = self
            .layout_workspace
            .and_then(|id| {
                self.layout_saves
                    .get(&id)
                    .map(|save| &save.desired)
                    .or_else(|| {
                        self.state
                            .workbench
                            .as_ref()
                            .filter(|(owner, _)| *owner == id)
                            .map(|(_, state)| &state.document.layout)
                    })
            })
            .cloned()
            .unwrap_or_default();
        self.show_workbench_layout(&layout);
        self.schedule_resize_flush();
    }

    pub(crate) fn save_workbench_layout(&mut self) {
        if !self.workbench_capable {
            return;
        }
        let Some(id) = self.state.active_workspace_id else {
            return;
        };
        let desired = self.pointer.splits.portable(!self.state.zoomed);
        let connected = self.state.connection == ConnectionState::Connected;
        let save = self.layout_saves.entry(id).or_insert_with(|| {
            let base = self
                .state
                .workbench
                .as_ref()
                .filter(|(owner, _)| *owner == id)
                .map(|(_, base)| base);
            LayoutSave {
                desired: base.map_or_else(
                    || PaneSplits::default().portable(false),
                    |base| normalize_layout(&base.document.layout),
                ),
                base: base.filter(|_| connected).cloned(),
                in_flight: None,
                edited_view: false,
                edited_splits: BTreeSet::new(),
            }
        });
        save.record(desired);
        if connected && save.base.is_none() {
            self.request_workbench(id, ClientMessage::GetWorkbench { workspace_id: id });
        }
        self.flush_workbench_layout(id);
    }

    pub(super) fn flush_workbench_layout(&mut self, id: WorkspaceId) {
        if !self.workbench_capable || self.state.connection != ConnectionState::Connected {
            return;
        }
        let initial = self
            .layout_saves
            .get(&id)
            .filter(|save| save.in_flight.is_none())
            .and_then(|save| save.base.as_ref())
            .filter(|base| base.revision == 0)
            .map(|base| base.document.clone());
        if let Some(initial) = initial
            && !self.legacy_workbench_ready(id, &initial)
        {
            return;
        }
        let Some(save) = self.layout_saves.get_mut(&id) else {
            return;
        };
        let Some(base) = save.base.as_ref().filter(|_| save.in_flight.is_none()) else {
            return;
        };
        let confirmed = &base.document.layout;
        let layout = save.project(confirmed);
        if normalize_layout(confirmed) == normalize_layout(&layout) {
            self.layout_saves.remove(&id);
            return;
        }

        // Rebase the desktop intent on the newest confirmed document. Retain
        // imported split keys the desktop does not own as well as other settings.
        let mut document = base.document.clone();
        document.layout = layout;
        let revision = base.revision;
        let token = Uuid::new_v4();
        save.in_flight = Some(token);
        if !self.send_command(UiCommand::Workbench {
            workspace_id: id,
            operation: WorkbenchOperation::Layout(token),
            request: Box::new(ClientMessage::UpdateWorkbench {
                workspace_id: id,
                revision,
                document,
                layout_only: true,
            }),
        }) {
            self.fail_layout_save(id, "Reconnect to save the workspace layout.".into());
        }
    }

    pub(super) fn layout_save_response(
        &mut self,
        id: WorkspaceId,
        token: Uuid,
        response: Result<ResponsePayload, String>,
    ) {
        let Some(save) = self
            .layout_saves
            .get_mut(&id)
            .filter(|save| save.in_flight == Some(token))
        else {
            return;
        };
        save.in_flight = None;
        match response {
            Ok(ResponsePayload::Workbench(state) | ResponsePayload::WorkbenchConflict(state)) => {
                self.load_workbench(id, state);
            }
            Err(error) => self.fail_layout_save(id, error),
            Ok(_) => self.fail_layout_save(id, "Unexpected workspace layout reply.".into()),
        }
    }

    fn fail_layout_save(&mut self, id: WorkspaceId, message: String) {
        if let Some(save) = self.layout_saves.get_mut(&id) {
            save.pause();
        }
        self.show_toast(message, true);
    }

    pub(super) fn show_workbench_layout(&mut self, layout: &WorkbenchLayout) {
        self.state.zoomed = !layout.split_view;
        self.pointer.splits = PaneSplits::from_portable(layout);
    }

    pub(crate) fn pause_layout_saves(&mut self) {
        for save in self.layout_saves.values_mut() {
            save.pause();
        }
    }

    pub(crate) fn prune_layout_saves(&mut self) {
        self.layout_saves.retain(|id, _| {
            self.state
                .workspaces
                .iter()
                .any(|workspace| workspace.id == *id)
        });
    }

    pub(crate) fn refresh_inactive_layout_saves(&mut self) {
        if !self.workbench_capable || self.state.connection != ConnectionState::Connected {
            return;
        }
        let reload: Vec<_> = self
            .layout_saves
            .iter()
            .filter(|(id, save)| {
                save.base.is_none() && Some(**id) != self.state.active_workspace_id
            })
            .map(|(id, _)| *id)
            .collect();
        for id in reload {
            self.request_workbench(id, ClientMessage::GetWorkbench { workspace_id: id });
        }
    }
}
