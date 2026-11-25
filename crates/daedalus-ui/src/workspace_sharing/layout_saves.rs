use super::DaedalusApp;
use daedalus_proto::workbench::{WorkbenchLayout, WorkbenchState};
use daedalus_proto::{ClientMessage, ResponsePayload, WorkspaceId};
use daedalus_ui::bridge::{UiCommand, WorkbenchOperation};
use daedalus_ui::layout::PaneSplits;
use daedalus_ui::state::ConnectionState;
use uuid::Uuid;

pub(crate) struct LayoutSave {
    pub(super) desired: WorkbenchLayout,
    pub(super) base: Option<WorkbenchState>,
    in_flight: Option<Uuid>,
}

impl LayoutSave {
    fn pause(&mut self) {
        self.in_flight = None;
        self.base = None;
    }
}

impl DaedalusApp {
    pub(crate) fn save_workbench_layout(&mut self) {
        if !self.workbench_capable {
            return;
        }
        let Some(id) = self.state.active_workspace_id else {
            return;
        };
        let desired = self.pointer.splits.portable(!self.state.zoomed);
        if let Some(save) = self.layout_saves.get_mut(&id) {
            save.desired = desired;
            if save.base.is_none() && self.state.connection == ConnectionState::Connected {
                self.request_workbench(id, ClientMessage::GetWorkbench { workspace_id: id });
            }
        } else if let Some((_, base)) = self
            .state
            .workbench
            .as_ref()
            .filter(|(owner, base)| *owner == id && base.revision > 0)
        {
            self.layout_saves.insert(
                id,
                LayoutSave {
                    desired,
                    base: (self.state.connection == ConnectionState::Connected)
                        .then(|| base.clone()),
                    in_flight: None,
                },
            );
        }
        self.flush_workbench_layout(id);
    }

    pub(super) fn flush_workbench_layout(&mut self, id: WorkspaceId) {
        if !self.workbench_capable || self.state.connection != ConnectionState::Connected {
            return;
        }
        let Some(save) = self.layout_saves.get_mut(&id) else {
            return;
        };
        let Some(base) = save.base.as_ref().filter(|_| save.in_flight.is_none()) else {
            return;
        };
        let confirmed = &base.document.layout;
        if PaneSplits::from_portable(confirmed).portable(confirmed.split_view) == save.desired {
            self.layout_saves.remove(&id);
            return;
        }

        // Rebase the desktop intent on the newest confirmed document. Retain
        // imported split keys the desktop does not own as well as other settings.
        let mut document = base.document.clone();
        document.layout.split_view = save.desired.split_view;
        document.layout.splits.extend(save.desired.splits.clone());
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
