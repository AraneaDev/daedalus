use super::{DisplayOverrides, WidgetTarget};
use crate::client::{ClientError, DaemonClient};
use crate::commands::workspace_config::{print_state, update_widgets, workbench};
use daedalus_proto::WorkspaceId;
use daedalus_proto::workbench::{
    WidgetDefinition, WidgetInstance, WidgetTemplate, WorkbenchState, WorkspaceDocument,
};

pub(super) enum Change {
    Create {
        id: String,
        name: String,
        template: Option<WidgetTemplate>,
        definition: Option<Box<WidgetDefinition>>,
        display: DisplayOverrides,
    },
    Edit {
        id: String,
        draft: Option<WidgetTemplate>,
        name: Option<String>,
        display: DisplayOverrides,
    },
    Duplicate(String, String),
    Remove(String),
    Move(String, usize),
    Pin(String, bool),
}

impl Change {
    pub(super) fn create(
        id: String,
        name: Option<String>,
        template: Option<WidgetTemplate>,
        inline: Option<WidgetTemplate>,
        display: DisplayOverrides,
    ) -> Self {
        let name = name
            .or_else(|| {
                template
                    .as_ref()
                    .or(inline.as_ref())
                    .map(|template| template.name.clone())
            })
            .unwrap_or_else(|| id.clone());
        Self::Create {
            id,
            name,
            template,
            definition: inline.map(|file| Box::new(file.definition)),
            display,
        }
    }

    pub(super) async fn save(
        self,
        client: &mut DaemonClient,
        selection: WidgetTarget,
    ) -> Result<(), ClientError> {
        let workspace = selection.target.resolve(client).await?;
        let state = workbench(client, workspace).await?;
        self.save_at(client, workspace, state, selection.json).await
    }

    pub(super) async fn save_at(
        self,
        client: &mut DaemonClient,
        workspace: WorkspaceId,
        mut state: WorkbenchState,
        json: bool,
    ) -> Result<(), ClientError> {
        self.apply(&mut state.document)?;
        let saved = update_widgets(client, workspace, state.revision, state.document).await?;
        print_state(&saved, json)
    }

    fn apply(self, document: &mut WorkspaceDocument) -> Result<(), ClientError> {
        match self {
            Self::Create {
                id,
                name,
                template,
                definition,
                display,
            } => {
                let mut instance = WidgetInstance {
                    id,
                    name,
                    pinned: true,
                    template: None,
                    definition: definition.map(|definition| *definition),
                };
                if let Some(template) = template {
                    attach_template(document, &mut instance, template);
                } else if instance.definition.is_none() {
                    instance.definition = Some(WidgetDefinition::default());
                }
                update_display(document, &mut instance, display);
                document.widgets.push(instance);
            }
            Self::Edit {
                id,
                draft,
                name,
                display,
            } => {
                let index = widget_index(document, &id)?;
                let mut widget = document.widgets[index].clone();
                if let Some(draft) = draft {
                    widget.name = draft.name;
                    widget.definition = Some(draft.definition);
                }
                if let Some(name) = name {
                    widget.name = name;
                }
                update_display(document, &mut widget, display);
                document.widgets[index] = widget;
            }
            Self::Duplicate(id, new_id) => {
                let index = widget_index(document, &id)?;
                let mut copy = document.widgets[index].clone();
                copy.id = new_id;
                copy.name.push_str(" copy");
                document.widgets.insert(index + 1, copy);
            }
            Self::Remove(id) => {
                let index = widget_index(document, &id)?;
                document.widgets.remove(index);
            }
            Self::Move(id, position) => {
                if !(1..=document.widgets.len()).contains(&position) {
                    return Err("Position is outside the widget list.".into());
                }
                let index = widget_index(document, &id)?;
                let widget = document.widgets.remove(index);
                document.widgets.insert(position - 1, widget);
            }
            Self::Pin(id, pinned) => {
                let index = widget_index(document, &id)?;
                document.widgets[index].pinned = pinned;
            }
        }
        Ok(())
    }
}

fn update_display(
    document: &WorkspaceDocument,
    widget: &mut WidgetInstance,
    display: DisplayOverrides,
) {
    if widget.definition.is_none()
        && let Some(base) = document.widget_definition(widget)
        && !display.matches(&base.display)
    {
        widget.definition = Some(base.clone());
    }
    if let Some(definition) = &mut widget.definition {
        display.apply(&mut definition.display);
    }
}

pub(super) fn find_widget<'a>(
    document: &'a WorkspaceDocument,
    id: &str,
) -> Result<&'a WidgetInstance, ClientError> {
    Ok(&document.widgets[widget_index(document, id)?])
}

pub(super) fn widget_index(document: &WorkspaceDocument, id: &str) -> Result<usize, ClientError> {
    document
        .widgets
        .iter()
        .position(|widget| widget.id == id)
        .ok_or_else(|| format!("No widget with ID '{id}'").into())
}

fn attach_template(
    document: &mut WorkspaceDocument,
    widget: &mut WidgetInstance,
    template: WidgetTemplate,
) {
    widget.template = Some(template.id.clone());
    if let Some(existing) = document
        .templates
        .iter()
        .find(|existing| existing.id == template.id)
    {
        if existing != &template {
            widget.definition = Some(template.definition);
        }
    } else {
        document.templates.push(template);
    }
}
