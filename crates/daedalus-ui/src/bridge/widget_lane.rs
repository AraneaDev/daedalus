use super::{UiCommand, UiEvent, UiEventSink, background_rpc};
use daedalus_proto::ClientMessage;
use std::path::Path;
use std::sync::Arc;
use tokio::task::JoinSet;

pub(super) struct WidgetLane<'a> {
    endpoint: &'a Path,
    auth_token: Option<&'a str>,
    sink: &'a Arc<dyn UiEventSink>,
    jobs: JoinSet<()>,
    polling: bool,
}

impl<'a> WidgetLane<'a> {
    pub(super) fn new(
        endpoint: &'a Path,
        auth_token: Option<&'a str>,
        sink: &'a Arc<dyn UiEventSink>,
    ) -> Self {
        Self {
            endpoint,
            auth_token,
            sink,
            jobs: JoinSet::new(),
            polling: false,
        }
    }

    pub(super) fn has_jobs(&self) -> bool {
        !self.jobs.is_empty()
    }

    pub(super) async fn join_next(&mut self) {
        let _ = self.jobs.join_next().await;
    }

    pub(super) fn dispatch(&mut self, command: &UiCommand) -> bool {
        let UiCommand::Workbench {
            workspace_id,
            operation,
            request,
        } = command
        else {
            return false;
        };
        let status = match request.as_ref() {
            ClientMessage::PreviewWidget { .. } => Some("Running widget preview…"),
            ClientMessage::GetWidgetResults {
                refresh: Some(_), ..
            } => Some("Refreshing widget…"),
            ClientMessage::InvokeWidgetAction { .. } => Some("Running widget action…"),
            ClientMessage::GetWidgetResults { refresh: None, .. } => None,
            _ => return false,
        };

        if self.has_jobs() {
            match status {
                None => return true,
                Some(_) if self.polling => {
                    // A user-requested operation takes precedence over an observation.
                    self.jobs = JoinSet::new();
                }
                Some(_) => {
                    self.sink.emit(UiEvent::Workbench {
                        workspace_id: *workspace_id,
                        operation: operation.clone(),
                        response: Box::new(Err("A widget operation is already running.".into())),
                    });
                    return true;
                }
            }
        }
        if let Some(message) = status {
            self.sink.emit(UiEvent::Toast {
                message: message.into(),
                is_error: false,
            });
        }
        self.polling = status.is_none();
        let workspace_id = *workspace_id;
        let operation = operation.clone();
        let request = (**request).clone();
        let endpoint = self.endpoint.to_path_buf();
        let auth_token = self.auth_token.map(str::to_owned);
        let sink = Arc::clone(self.sink);
        self.jobs.spawn(async move {
            // Widget execution uses its configured limits. Dropping the lane
            // cancels the outstanding request and its separate connection.
            let response =
                background_rpc::request(&endpoint, auth_token.as_deref(), "widget", request).await;
            // Polling is advisory. A failed poll will be retried by the controller.
            if status.is_some() || response.is_ok() {
                sink.emit(UiEvent::Workbench {
                    workspace_id,
                    operation,
                    response: Box::new(response),
                });
            }
        });
        true
    }
}

#[cfg(test)]
mod tests;
