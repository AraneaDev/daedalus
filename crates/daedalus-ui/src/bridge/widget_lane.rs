use super::{UiCommand, UiEvent, UiEventSink};
use daedalus_proto::transport::IpcStream;
use daedalus_proto::{
    ClientMessage, Request, ResponsePayload, ServerMessage, read_frame, write_frame,
};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
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
            let response = run(&endpoint, auth_token, request).await;
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

async fn run(
    endpoint: &Path,
    auth_token: Option<String>,
    request: ClientMessage,
) -> Result<ResponsePayload, String> {
    let mut stream = tokio::time::timeout(Duration::from_secs(30), async {
        let mut stream = IpcStream::connect(endpoint)
            .await
            .map_err(|error| error.to_string())?;
        if let Some(token) = auth_token {
            let response = call(&mut stream, 1, ClientMessage::Authenticate { token }).await?;
            if response != ResponsePayload::Ok {
                return Err("Unexpected authentication response for the widget connection.".into());
            }
        }
        Ok::<_, String>(stream)
    })
    .await
    .map_err(|_| "The widget connection timed out.".to_string())??;

    // Execution uses the limits configured in the widget. Ending the main session drops
    // this separate connection with its outstanding request.
    call(&mut stream, 2, request).await
}

async fn call(
    stream: &mut IpcStream,
    id: u64,
    payload: ClientMessage,
) -> Result<ResponsePayload, String> {
    write_frame(stream, &Request { id, payload })
        .await
        .map_err(|error| error.to_string())?;
    match read_frame::<_, ServerMessage>(stream)
        .await
        .map_err(|error| error.to_string())?
    {
        Some(ServerMessage::Response {
            id: response_id,
            result,
        }) if response_id == id => result.map_err(|error| error.to_string()),
        Some(_) => Err("Unexpected reply on the widget connection.".into()),
        None => Err("The widget connection closed before reporting a result.".into()),
    }
}

#[cfg(test)]
mod tests;
