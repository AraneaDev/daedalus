mod schema;

use super::instances::widget_index;
use super::{WidgetTarget, print_json, templates};
use crate::client::{ClientError, DaemonClient};
use crate::commands::workspace_config::{read_input, update_widgets, workbench};
use crate::unexpected_response;
use clap::Args;
use daedalus_proto::workbench::{
    WidgetDefinition, WidgetInstance, WidgetTemplate, WorkbenchState, WorkspaceDocument,
};
use daedalus_proto::{ClientMessage, ResponsePayload};
use serde_json::{Map, Value, json};
use std::path::PathBuf;

#[derive(Debug, Args)]
pub(crate) struct Configure {
    id: String,
    /// Read a JSON merge object from a file or from stdin (-).
    #[arg(long, value_name = "FILE")]
    patch: PathBuf,
    /// Create the widget if the ID is missing.
    #[arg(long)]
    create: bool,
    /// Validate the proposed configuration without saving or running sources.
    #[arg(long)]
    dry_run: bool,
    /// Apply only if the workspace has this saved revision.
    #[arg(long, value_name = "REVISION")]
    expect_revision: Option<u64>,
    #[command(flatten)]
    selection: WidgetTarget,
}

impl Configure {
    pub(super) async fn run(self, client: &mut DaemonClient) -> Result<(), ClientError> {
        let patch = Patch::parse(read_input(&self.patch)?)?;
        let workspace = self.selection.target.resolve(client).await?;
        let mut state = workbench(client, workspace).await?;
        if let Some(expected) = self.expect_revision
            && expected != state.revision
        {
            return Err(format!(
                "Expected workspace revision {expected}. Current revision {}.",
                state.revision
            )
            .into());
        }
        let changed = patch
            .apply(client, &mut state.document, &self.id, self.create)
            .await?;
        if self.dry_run {
            if changed && state.revision == u64::MAX {
                return Err("The workspace revision cannot be incremented.".into());
            }
            match client
                .request(ClientMessage::ResolveWorkspaceImport {
                    workspace_id: workspace,
                    revision: state.revision,
                    incoming: state.document.clone(),
                    linked: false,
                    resolutions: Default::default(),
                })
                .await?
            {
                ResponsePayload::WorkspaceImportPreview(_) => {}
                response => return Err(unexpected_response(response)),
            }
        } else if changed {
            state = update_widgets(client, workspace, state.revision, state.document).await?;
        }

        if self.selection.json {
            let mut result = effective(&state, &self.id)?;
            result["changed"] = json!(changed);
            result["dry_run"] = json!(self.dry_run);
            return print_json(result);
        }
        match (self.dry_run, changed) {
            (true, true) => println!("Widget '{}' would change", self.id),
            (false, true) => println!("Widget '{}' saved at revision {}", self.id, state.revision),
            (_, false) => println!(
                "Widget '{}' is already at revision {}",
                self.id, state.revision
            ),
        }
        Ok(())
    }
}

pub(super) fn effective(state: &WorkbenchState, id: &str) -> Result<Value, ClientError> {
    let index = widget_index(&state.document, id)?;
    let widget = &state.document.widgets[index];
    let definition = resolved(&state.document, widget)?;
    let display = &definition.display;
    let appearance = &display.appearance;
    let mut definition_json = serde_json::to_value(definition)?;
    definition_json["display"]["style"] = json!(display.style);
    definition_json["display"]["elements"] = json!(display.elements);
    definition_json["display"]["primary_action"] = json!(display.primary_action);
    definition_json["display"]["appearance"] = json!({
        "accent": appearance.accent,
        "width": appearance.width,
        "density": appearance.density,
        "surface": appearance.surface,
        "status": appearance.status,
    });
    Ok(json!({
        "id": widget.id,
        "revision": state.revision,
        "name": widget.name,
        "pinned": widget.pinned,
        "order": index + 1,
        "template": widget.template,
        "definition": definition_json,
    }))
}

struct Patch {
    instance: Map<String, Value>,
    order: Option<usize>,
    definition: Option<Value>,
}

impl Patch {
    fn parse(input: String) -> Result<Self, ClientError> {
        let Value::Object(mut instance) = serde_json::from_str(&input)? else {
            return Err("Widget patch must be a JSON object.".into());
        };
        let definition = instance.remove("definition");
        if let Some(definition) = &definition {
            schema::validate_definition(definition)?;
        }
        let order = instance
            .remove("order")
            .map(serde_json::from_value::<usize>)
            .transpose()
            .map_err(|error| format!("Invalid widget order: {error}"))?;
        if let Some(field) = instance
            .keys()
            .find(|field| !matches!(field.as_str(), "name" | "pinned" | "template"))
        {
            return Err(format!("Unknown widget patch field '{field}'.").into());
        }
        Ok(Self {
            instance,
            order,
            definition,
        })
    }

    async fn apply(
        self,
        client: &mut DaemonClient,
        document: &mut WorkspaceDocument,
        id: &str,
        create: bool,
    ) -> Result<bool, ClientError> {
        let previous = match widget_index(document, id) {
            Ok(index) => Some(index),
            Err(_) if create => None,
            Err(error) => return Err(error),
        };
        let count = document.widgets.len() + usize::from(previous.is_none());
        let order = self
            .order
            .unwrap_or_else(|| previous.map_or(count, |index| index + 1));
        if !(1..=count).contains(&order) {
            return Err("Position is outside the widget list.".into());
        }

        let default = WidgetInstance {
            id: id.to_owned(),
            name: id.to_owned(),
            pinned: true,
            template: None,
            definition: None,
        };
        let original = previous.map_or(&default, |index| &document.widgets[index]);
        let name_supplied = self.instance.contains_key("name");
        let mut merged = serde_json::to_value(original)?;
        merge_object(&mut merged, self.instance);
        let mut widget: WidgetInstance = serde_json::from_value(merged)
            .map_err(|error| format!("Invalid widget instance patch: {error}"))?;

        let saved = widget
            .template
            .as_deref()
            .and_then(|id| document.templates.iter().find(|template| template.id == id));
        let imported = match (widget.template.as_deref(), saved) {
            (Some(id), None) => Some(templates::load_template(client, id).await?),
            _ => None,
        };
        let selected = saved.or(imported.as_ref());
        if let Some(template) = selected
            && previous.is_none()
            && !name_supplied
        {
            widget.name.clone_from(&template.name);
        }
        patch_definition(document, original, &mut widget, selected, self.definition)?;

        let index = order - 1;
        if previous == Some(index) && widget == *original && imported.is_none() {
            return Ok(false);
        }
        if let Some(template) = imported {
            document.templates.push(template);
        }
        if let Some(previous) = previous {
            document.widgets.remove(previous);
        }
        document.widgets.insert(index, widget);
        Ok(true)
    }
}

fn patch_definition(
    document: &WorkspaceDocument,
    original: &WidgetInstance,
    widget: &mut WidgetInstance,
    selected: Option<&WidgetTemplate>,
    patch: Option<Value>,
) -> Result<(), ClientError> {
    if patch == Some(Value::Null) {
        if selected.is_none() {
            return Err("Clearing the widget definition requires a template.".into());
        }
        widget.definition = None;
        return Ok(());
    }

    let default = WidgetDefinition::default();
    let base = original
        .definition
        .as_ref()
        .or_else(|| selected.map(|template| &template.definition))
        .or_else(|| document.widget_definition(original))
        .unwrap_or(&default);
    if let Some(patch) = patch {
        let Value::Object(patch) = patch else {
            return Err("Widget definition patch must be an object or null.".into());
        };
        let mut merged = serde_json::to_value(base)?;
        merge_object(&mut merged, patch);
        let definition = serde_json::from_value(merged)
            .map_err(|error| format!("Invalid widget definition patch: {error}"))?;
        if definition != *base || selected.is_none() {
            widget.definition = Some(definition);
        }
    } else if widget.definition.is_none() && selected.is_none() {
        widget.definition = Some(base.clone());
    }
    Ok(())
}

fn merge_object(target: &mut Value, patch: Map<String, Value>) {
    if !target.is_object() {
        *target = json!({});
    }
    let target = target.as_object_mut().expect("merge target is an object");
    for (field, value) in patch {
        match value {
            Value::Null => {
                target.remove(&field);
            }
            Value::Object(object) => {
                merge_object(target.entry(field).or_insert(Value::Null), object)
            }
            replacement => {
                target.insert(field, replacement);
            }
        }
    }
}

fn resolved<'a>(
    document: &'a WorkspaceDocument,
    widget: &'a WidgetInstance,
) -> Result<&'a WidgetDefinition, ClientError> {
    document
        .widget_definition(widget)
        .ok_or_else(|| format!("Widget '{}' has no resolved definition.", widget.id).into())
}
