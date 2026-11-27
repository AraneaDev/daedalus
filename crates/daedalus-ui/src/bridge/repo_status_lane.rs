use super::{UiCommand, UiEvent, UiEventSink, WorkbenchOperation, background_rpc};
use daedalus_proto::{ClientMessage, ResponsePayload, WorkspaceId};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinSet;

const CHECK_TIMEOUT: Duration = Duration::from_secs(3);

struct Check {
    workspace_id: WorkspaceId,
    operation: WorkbenchOperation,
}

pub(super) struct RepoStatusLane<'a> {
    endpoint: &'a Path,
    auth_token: Option<&'a str>,
    sink: &'a Arc<dyn UiEventSink>,
    job: JoinSet<(Check, Option<ResponsePayload>)>,
    pending: Option<Check>,
}

impl<'a> RepoStatusLane<'a> {
    pub(super) fn new(
        endpoint: &'a Path,
        auth_token: Option<&'a str>,
        sink: &'a Arc<dyn UiEventSink>,
    ) -> Self {
        Self {
            endpoint,
            auth_token,
            sink,
            job: JoinSet::new(),
            pending: None,
        }
    }

    pub(super) fn has_job(&self) -> bool {
        !self.job.is_empty()
    }

    pub(super) fn dispatch(&mut self, command: &UiCommand) -> bool {
        let UiCommand::Workbench {
            operation, request, ..
        } = command
        else {
            return false;
        };
        let ClientMessage::CheckWorkspaceRepo { workspace_id } = request.as_ref() else {
            return false;
        };

        self.pending = Some(Check {
            workspace_id: *workspace_id,
            operation: operation.clone(),
        });
        self.start_pending();
        true
    }

    pub(super) async fn join_next(&mut self) {
        if let Some(Ok((check, Some(response)))) = self.job.join_next().await
            && self
                .pending
                .as_ref()
                .is_none_or(|pending| pending.workspace_id == check.workspace_id)
        {
            self.sink.emit(UiEvent::Workbench {
                workspace_id: check.workspace_id,
                operation: check.operation,
                response: Box::new(Ok(response)),
            });
        }
        self.start_pending();
    }

    fn start_pending(&mut self) {
        if self.has_job() {
            return;
        }
        let Some(check) = self.pending.take() else {
            return;
        };
        let endpoint = self.endpoint.to_path_buf();
        let auth_token = self.auth_token.map(str::to_owned);
        self.job.spawn(async move {
            // A slow repo read is advisory. Bound the separate connection,
            // authentication, and reply together without interrupting the session.
            let result = tokio::time::timeout(
                CHECK_TIMEOUT,
                background_rpc::request(
                    &endpoint,
                    auth_token.as_deref(),
                    "repo status",
                    ClientMessage::CheckWorkspaceRepo {
                        workspace_id: check.workspace_id,
                    },
                ),
            )
            .await;
            let response = match result {
                Ok(Ok(response @ ResponsePayload::WorkspaceRepoStatus(_))) => Some(response),
                _ => None,
            };
            (check, response)
        });
    }
}
