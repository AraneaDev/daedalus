use super::background_rpc;
use daedalus_proto::{ClientMessage, RecentProject, ResponsePayload};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::task::JoinSet;

pub(super) fn request(
    jobs: &mut JoinSet<Result<Vec<RecentProject>, String>>,
    endpoint: &Path,
    auth_token: Option<&str>,
) {
    let endpoint: PathBuf = endpoint.into();
    let auth_token = auth_token.map(str::to_owned);
    jobs.spawn(async move {
        // The daemon handles requests in order on each connection. Keep folder
        // probes on a separate stream so they cannot delay terminal commands.
        let response = tokio::time::timeout(
            Duration::from_secs(30),
            background_rpc::request(
                &endpoint,
                auth_token.as_deref(),
                "recent-projects",
                ClientMessage::ListRecentProjects,
            ),
        )
        .await
        .map_err(|_| "Recent projects request timed out.".to_string())??;

        match response {
            ResponsePayload::RecentProjects(projects) => Ok(projects),
            other => Err(format!(
                "Unexpected response to ListRecentProjects: {other:?}"
            )),
        }
    });
}
