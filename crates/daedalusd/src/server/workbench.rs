use super::SubscriptionFilter;
use crate::registry::Registry;
use daedalus_proto::{ClientMessage, ProtoError, ResponsePayload};
use tokio::sync::RwLock;

pub(super) async fn dispatch(
    registry: &Registry,
    filter: &RwLock<SubscriptionFilter>,
    request: ClientMessage,
) -> Result<ResponsePayload, ProtoError> {
    match request {
        ClientMessage::GetWorkbench { workspace_id } => Ok(ResponsePayload::Workbench(
            registry.workbench(workspace_id).await?,
        )),
        ClientMessage::WatchWorkbench { workspace_id } => {
            filter.write().await.workbench = Some(workspace_id);
            Ok(ResponsePayload::Workbench(
                registry.workbench(workspace_id).await?,
            ))
        }
        ClientMessage::UpdateWorkbench {
            workspace_id,
            revision,
            document,
        } => {
            let result = registry
                .update_workbench(workspace_id, Some(revision), document)
                .await?;
            Ok(workbench_response(result))
        }
        ClientMessage::SeedLegacyWorkbench {
            workspace_id,
            document,
        } => {
            let result = registry
                .update_workbench(workspace_id, None, document)
                .await?;
            Ok(workbench_response(result))
        }
        ClientMessage::ListWidgetTemplates => Ok(ResponsePayload::WidgetLibrary(
            registry.workbench_store.library().await?,
        )),
        ClientMessage::UpdateWidgetTemplates {
            revision,
            templates,
        } => {
            use crate::workbench::store::Change;
            Ok(
                match registry
                    .workbench_store
                    .update_library(revision, templates)
                    .await?
                {
                    Change::Applied(library) => ResponsePayload::WidgetLibrary(library),
                    Change::Conflict(library) => ResponsePayload::WidgetLibraryConflict(library),
                },
            )
        }
        ClientMessage::PreviewWorkspaceImport {
            workspace_id,
            source,
            linked,
        } => Ok(ResponsePayload::WorkspaceImportPreview(
            registry
                .preview_workspace_import(workspace_id, source, linked)
                .await?,
        )),
        ClientMessage::ApplyWorkspaceImport {
            workspace_id,
            revision,
            incoming,
            linked,
            resolutions,
        } => Ok(workbench_response(
            registry
                .apply_workspace_import(workspace_id, revision, incoming, linked, resolutions)
                .await?,
        )),
        ClientMessage::ResolveWorkspaceImport {
            workspace_id,
            revision,
            incoming,
            linked,
            resolutions,
        } => Ok(ResponsePayload::WorkspaceImportPreview(
            registry
                .resolve_workspace_import(workspace_id, revision, incoming, linked, resolutions)
                .await?,
        )),
        ClientMessage::ExportWorkspace { workspace_id } => Ok(ResponsePayload::WorkbenchToml(
            registry.export_workbench(workspace_id).await?,
        )),
        ClientMessage::PreviewWorkspaceCreation {
            root_dir,
            source,
            linked,
        } => Ok(ResponsePayload::WorkspaceCreationPreview(
            registry
                .preview_workspace_creation(root_dir, source, linked)
                .await?,
        )),
        ClientMessage::ApplyWorkspaceCreation { preview } => Ok(ResponsePayload::WorkbenchCreated(
            registry.apply_workspace_creation(preview).await?,
        )),
        ClientMessage::CheckWorkspaceRepo { workspace_id } => {
            Ok(ResponsePayload::WorkspaceRepoStatus(
                registry.workspace_repo_status(workspace_id).await?,
            ))
        }
        ClientMessage::DecodeWidgetTemplate { text } => {
            let document = crate::workbench::documents::parse_template(&text)
                .map_err(|error| ProtoError::InvalidRequest(error.to_string()))?;
            Ok(ResponsePayload::WidgetTemplate(document.template))
        }
        ClientMessage::EncodeWidgetTemplate { template } => {
            use daedalus_proto::workbench::{TemplateDocument, WORKSPACE_FORMAT_VERSION};
            let text = crate::workbench::documents::serialize_template(&TemplateDocument {
                version: WORKSPACE_FORMAT_VERSION,
                template,
            })
            .map_err(|error| ProtoError::InvalidRequest(error.to_string()))?;
            Ok(ResponsePayload::WorkbenchToml(text))
        }
        ClientMessage::PreviewWidget {
            workspace_id,
            name,
            definition,
        } => Ok(ResponsePayload::WidgetResult(
            registry
                .preview_widget(workspace_id, name, definition)
                .await?,
        )),
        ClientMessage::GetWidgetResults {
            workspace_id,
            refresh,
        } => Ok(ResponsePayload::WidgetResults(
            registry.widget_results(workspace_id, refresh).await?,
        )),
        ClientMessage::InvokeWidgetAction {
            workspace_id,
            widget,
            action,
        } => Ok(ResponsePayload::WidgetAction(
            registry
                .invoke_widget_action(workspace_id, &widget, &action)
                .await?,
        )),
        _ => unreachable!("only workbench requests are routed here"),
    }
}

fn workbench_response(
    result: crate::workbench::store::Change<daedalus_proto::workbench::WorkbenchState>,
) -> ResponsePayload {
    use crate::workbench::store::Change;
    match result {
        Change::Applied(state) => ResponsePayload::Workbench(state),
        Change::Conflict(state) => ResponsePayload::WorkbenchConflict(state),
    }
}
