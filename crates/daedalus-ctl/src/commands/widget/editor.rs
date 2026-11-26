use super::{
    WidgetTarget,
    instances::{Change, find_widget},
    templates::{decode_text, encode},
};
use crate::client::{ClientError, DaemonClient};
use crate::commands::workspace_config::{edit_text, workbench};
use daedalus_proto::workbench::WidgetTemplate;

pub(super) async fn edit(
    client: &mut DaemonClient,
    id: String,
    selection: WidgetTarget,
) -> Result<(), ClientError> {
    let workspace = selection.target.resolve(client).await?;
    let state = workbench(client, workspace).await?;
    let widget = find_widget(&state.document, &id)?;
    let template = WidgetTemplate {
        id: widget.id.clone(),
        name: widget.name.clone(),
        definition: state
            .document
            .widget_definition(widget)
            .cloned()
            .unwrap_or_default(),
    };
    let text = encode(client, template).await?;
    let draft = decode_text(client, edit_text(&text)?).await?;
    Change::Edit {
        id,
        draft: Some(draft),
        name: None,
    }
    .save_at(client, workspace, state, selection.json)
    .await
}
