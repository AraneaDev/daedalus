use super::{UiEvent, background_rpc};
use daedalus_proto::{ClientMessage, PaneId, ResponsePayload, WorkspaceId, rpc::ImagePreview};
use std::path::Path;
use std::time::Duration;
use tokio::task::JoinSet;
use uuid::Uuid;

pub(super) struct ImageRequest {
    pub workspace_id: WorkspaceId,
    pub pane_id: PaneId,
    pub token: Uuid,
    pub path: String,
}

impl ImageRequest {
    pub(super) fn start(
        self,
        jobs: &mut JoinSet<UiEvent>,
        endpoint: &Path,
        auth_token: Option<&str>,
    ) {
        let endpoint = endpoint.to_path_buf();
        let auth_token = auth_token.map(str::to_owned);
        jobs.spawn(async move {
            let result = fetch(
                &endpoint,
                auth_token.as_deref(),
                self.workspace_id,
                self.path,
            )
            .await;
            UiEvent::ImagePreview {
                pane_id: self.pane_id,
                token: self.token,
                result,
            }
        });
    }
}

async fn fetch(
    endpoint: &Path,
    auth_token: Option<&str>,
    workspace_id: WorkspaceId,
    path: String,
) -> Result<ImagePreview, String> {
    let response = tokio::time::timeout(
        Duration::from_secs(30),
        background_rpc::request(
            endpoint,
            auth_token,
            "image preview",
            ClientMessage::PreviewImage { workspace_id, path },
        ),
    )
    .await
    .map_err(|_| "Image preview timed out.".to_string())??;
    match response {
        ResponsePayload::ImagePreview(image) if image.valid() => Ok(image),
        _ => Err("Invalid preview received.".into()),
    }
}
