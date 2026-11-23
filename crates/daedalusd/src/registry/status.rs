use super::Registry;
use daedalus_proto::{ExternalStatus, ProtoError, ServerEvent, StatusLevel, WorkspaceId};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_EXTERNAL_STATUSES: usize = 16;

impl Registry {
    pub async fn publish_external_status(
        &self,
        workspace_id: Option<WorkspaceId>,
        root_dir: Option<PathBuf>,
        source: String,
        level: StatusLevel,
        label: String,
        detail: Option<String>,
    ) -> Result<ExternalStatus, ProtoError> {
        if source.trim().is_empty() {
            return Err(ProtoError::InvalidRequest(
                "external status source cannot be empty".to_string(),
            ));
        }
        if label.trim().is_empty() {
            return Err(ProtoError::InvalidRequest(
                "external status label cannot be empty".to_string(),
            ));
        }
        if workspace_id.is_none() && root_dir.is_none() {
            return Err(ProtoError::InvalidRequest(
                "either workspace_id or root_dir must be provided".to_string(),
            ));
        }

        let canonical_target = Self::canonicalize_target(workspace_id, root_dir.as_deref()).await;

        let updated_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0);

        let status = ExternalStatus {
            source,
            level,
            label,
            detail,
            updated_at_ms,
        };

        let target_id = {
            let mut ws_map = self.workspaces.write().await;
            let resolved_id = Self::resolve_workspace_in_map(
                &ws_map,
                workspace_id,
                root_dir.as_deref(),
                canonical_target.as_deref(),
            )?;

            let ws = ws_map
                .get_mut(&resolved_id)
                .ok_or(ProtoError::WorkspaceNotFound(resolved_id))?;

            let changed = if let Some(pos) = ws
                .external_statuses
                .iter()
                .position(|existing| existing.source == status.source)
            {
                let previous = ws.external_statuses.remove(pos);
                previous.level != status.level
                    || previous.label != status.label
                    || previous.detail != status.detail
            } else {
                if ws.external_statuses.len() >= MAX_EXTERNAL_STATUSES {
                    ws.external_statuses.remove(0);
                }
                true
            };
            if changed {
                ws.journal.record(daedalus_proto::ActivityKind::External {
                    level: status.level,
                });
            }
            ws.external_statuses.push(status.clone());
            resolved_id
        };

        let _ = self.event_tx.send(ServerEvent::ExternalStatusUpdated {
            workspace_id: target_id,
            status: status.clone(),
        });

        Ok(status)
    }

    pub async fn clear_external_status(
        &self,
        workspace_id: Option<WorkspaceId>,
        root_dir: Option<PathBuf>,
        source: String,
    ) -> Result<(), ProtoError> {
        if source.trim().is_empty() {
            return Err(ProtoError::InvalidRequest(
                "external status source cannot be empty".to_string(),
            ));
        }
        if workspace_id.is_none() && root_dir.is_none() {
            return Err(ProtoError::InvalidRequest(
                "either workspace_id or root_dir must be provided".to_string(),
            ));
        }

        let canonical_target = Self::canonicalize_target(workspace_id, root_dir.as_deref()).await;
        let updated_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0);

        let cleared_ws_id = {
            let mut ws_map = self.workspaces.write().await;
            let resolved_id = Self::resolve_workspace_in_map(
                &ws_map,
                workspace_id,
                root_dir.as_deref(),
                canonical_target.as_deref(),
            )?;
            let ws = ws_map
                .get_mut(&resolved_id)
                .ok_or(ProtoError::WorkspaceNotFound(resolved_id))?;

            let before = ws.external_statuses.len();
            ws.external_statuses
                .retain(|existing| existing.source != source);
            (ws.external_statuses.len() != before).then(|| {
                ws.journal
                    .record(daedalus_proto::ActivityKind::ExternalCleared);
                resolved_id
            })
        };

        if let Some(target_id) = cleared_ws_id {
            let _ = self.event_tx.send(ServerEvent::ExternalStatusUpdated {
                workspace_id: target_id,
                status: ExternalStatus {
                    source,
                    level: StatusLevel::Info,
                    label: String::new(),
                    detail: None,
                    updated_at_ms,
                },
            });
        }

        Ok(())
    }
}
