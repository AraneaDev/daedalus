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
    in_flight: Option<LayoutWrite>,
    edits: LayoutEdits,
}

struct LayoutWrite {
    token: Uuid,
    layout: WorkbenchLayout,
    edits: LayoutEdits,
}

#[derive(Clone, Default)]
struct LayoutEdits {
    view: bool,
    splits: BTreeSet<String>,
}

impl LayoutSave {
    fn record(&mut self, desired: WorkbenchLayout) {
        self.edits.splits.extend(
            desired
                .splits
                .iter()
                .filter(|(key, value)| self.desired.splits.get(*key) != Some(*value))
                .map(|(key, _)| key.clone()),
        );
        self.edits.view |=
            self.desired.split_view != desired.split_view || !self.edits.splits.is_empty();
        self.desired = desired;
    }

    fn project(&self, confirmed: &WorkbenchLayout) -> WorkbenchLayout {
        let mut layout = confirmed.clone();
        if self.edits.view {
            layout.split_view = self.desired.split_view;
        }
        layout.splits.extend(
            self.edits
                .splits
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

    fn confirm(&mut self, write: &LayoutWrite) {
        if write.edits.view && self.desired.split_view == write.layout.split_view {
            self.edits.view = false;
        }
        self.edits.splits.retain(|key| {
            !write.edits.splits.contains(key)
                || self.desired.splits.get(key) != write.layout.splits.get(key)
        });
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
        if !self.state.workbench_capable || self.layout_workspace == self.state.active_workspace_id
        {
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
        if !self.state.workbench_capable {
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
                edits: LayoutEdits::default(),
            }
        });
        save.record(desired);
        if connected && save.base.is_none() {
            self.request_workbench(id, ClientMessage::GetWorkbench { workspace_id: id });
        }
        self.flush_workbench_layout(id);
    }

    pub(super) fn flush_workbench_layout(&mut self, id: WorkspaceId) {
        if !self.state.workbench_capable || self.state.connection != ConnectionState::Connected {
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
        document.layout = layout.clone();
        let revision = base.revision;
        let token = Uuid::new_v4();
        save.in_flight = Some(LayoutWrite {
            token,
            layout,
            edits: save.edits.clone(),
        });
        if !self.send_command(UiCommand::Workbench {
            workspace_id: id,
            operation: WorkbenchOperation::Layout(token),
            request: Box::new(ClientMessage::UpdateWorkbench {
                workspace_id: id,
                revision,
                document,
                layout_only: true,
                edit: None,
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
        let Some(save) = self.layout_saves.get_mut(&id).filter(|save| {
            save.in_flight
                .as_ref()
                .is_some_and(|write| write.token == token)
        }) else {
            return;
        };
        let write = save.in_flight.take().expect("matching layout write");
        match response {
            Ok(ResponsePayload::Workbench(state)) => {
                save.confirm(&write);
                // A watched update can arrive before its matching save response.
                // Reapply the remaining intent to the newest confirmed snapshot.
                let latest = save
                    .base
                    .as_ref()
                    .filter(|base| base.revision > state.revision)
                    .cloned()
                    .unwrap_or(state);
                self.load_workbench(id, latest);
            }
            Ok(ResponsePayload::WorkbenchConflict(state)) => {
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

    pub(super) fn reconcile_workbench_layout(&mut self, id: WorkspaceId, layout: &WorkbenchLayout) {
        self.layout_workspace = Some(id);
        let previous = self
            .state
            .workbench
            .as_ref()
            .filter(|(workspace, _)| *workspace == id)
            .map(|(_, state)| state);
        if !self.pointer.dragging_divider() {
            if let Some(layout) = self.layout_saves.get(&id).map(|save| save.desired.clone()) {
                self.show_workbench_layout(&layout);
            } else if previous.is_none_or(|previous| &previous.document.layout != layout) {
                self.show_workbench_layout(layout);
            }
        }
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
        if !self.state.workbench_capable || self.state.connection != ConnectionState::Connected {
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
