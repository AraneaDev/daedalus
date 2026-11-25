use super::workspace_config::{Target, edit_text, read_input, update, workbench, write_output};
use crate::client::{ClientError, DaemonClient};
use crate::unexpected_response;
use clap::{Args, Subcommand};
use daedalus_proto::workbench::{
    WidgetDefinition, WidgetInstance, WidgetLibrary, WidgetTemplate, WorkspaceDocument,
};
use daedalus_proto::{ClientMessage, ResponsePayload};
use std::path::{Path, PathBuf};

#[derive(Debug, Subcommand)]
pub(crate) enum Action {
    List(WidgetTarget),
    Show {
        id: String,
        #[command(flatten)]
        selection: WidgetTarget,
    },
    Create {
        id: String,
        #[arg(long, conflicts_with = "template")]
        file: Option<PathBuf>,
        #[arg(long)]
        template: Option<String>,
        #[arg(long)]
        name: Option<String>,
        #[command(flatten)]
        selection: WidgetTarget,
    },
    Edit {
        id: String,
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long)]
        name: Option<String>,
        #[command(flatten)]
        selection: WidgetTarget,
    },
    Duplicate {
        id: String,
        new_id: String,
        #[command(flatten)]
        selection: WidgetTarget,
    },
    Remove {
        id: String,
        #[command(flatten)]
        selection: WidgetTarget,
    },
    /// Move a widget to a one-based position.
    Move {
        id: String,
        position: usize,
        #[command(flatten)]
        selection: WidgetTarget,
    },
    Pin {
        id: String,
        #[arg(long)]
        unpin: bool,
        #[command(flatten)]
        selection: WidgetTarget,
    },
    /// Test a template file against the daemon workspace.
    Preview {
        #[arg(long)]
        file: PathBuf,
        #[command(flatten)]
        selection: WidgetTarget,
    },
    /// Run a saved widget's command sources and print the result.
    Refresh {
        id: String,
        #[command(flatten)]
        selection: WidgetTarget,
    },
    /// Show the current results without executing widget commands.
    Results(WidgetTarget),
    #[command(name = "action")]
    Invoke {
        id: String,
        action: String,
        #[command(flatten)]
        selection: WidgetTarget,
    },
    Template {
        #[command(subcommand)]
        action: TemplateAction,
    },
}

#[derive(Debug, Args)]
pub(crate) struct WidgetTarget {
    #[command(flatten)]
    target: Target,
    #[arg(long)]
    json: bool,
}

impl Action {
    pub async fn run(self, client: &mut DaemonClient) -> Result<(), ClientError> {
        let (selection, change) = match self {
            Self::List(selection) => {
                let id = selection.target.resolve(client).await?;
                let widgets = workbench(client, id).await?.document.widgets;
                if selection.json {
                    print_json(serde_json::to_value(&widgets)?)?;
                } else {
                    for widget in widgets {
                        println!(
                            "{}  {}{}",
                            widget.id,
                            widget.name,
                            if widget.pinned { "  [pinned]" } else { "" }
                        );
                    }
                }
                return Ok(());
            }
            Self::Show { id, selection } => {
                let workspace = selection.target.resolve(client).await?;
                let state = workbench(client, workspace).await?;
                return print_json(serde_json::to_value(find_widget(&state.document, &id)?)?);
            }
            Self::Create {
                id,
                file,
                template,
                name,
                selection,
            } => {
                let (template, inline) = match (template, file) {
                    (Some(template), _) => (
                        Some(find_template(library(client).await?, &template)?),
                        None,
                    ),
                    (None, Some(path)) => (None, Some(decode(client, &path).await?)),
                    (None, None) => (None, None),
                };
                let name = name
                    .or_else(|| {
                        template
                            .as_ref()
                            .or(inline.as_ref())
                            .map(|template| template.name.clone())
                    })
                    .unwrap_or_else(|| id.clone());
                (
                    selection,
                    Change::Create {
                        id,
                        name,
                        template,
                        definition: inline.map(|file| file.definition),
                    },
                )
            }
            Self::Edit {
                id,
                file,
                name,
                selection,
            } => {
                if file.is_none() && name.is_none() {
                    let workspace = selection.target.resolve(client).await?;
                    let mut state = workbench(client, workspace).await?;
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
                    let text = match client
                        .request(ClientMessage::EncodeWidgetTemplate { template })
                        .await?
                    {
                        ResponsePayload::WorkbenchToml(text) => text,
                        response => return Err(unexpected_response(response)),
                    };
                    let draft = decode_text(client, edit_text(&text)?).await?;
                    Change::Edit {
                        id,
                        draft: Some(draft),
                        name: None,
                    }
                    .apply(&mut state.document)?;
                    let saved = update(client, workspace, state.revision, state.document).await?;
                    return super::workspace_config::print_state(&saved, selection.json);
                }
                let draft = match file {
                    Some(path) => Some(decode(client, &path).await?),
                    None => None,
                };
                (selection, Change::Edit { id, draft, name })
            }
            Self::Duplicate {
                id,
                new_id,
                selection,
            } => (selection, Change::Duplicate(id, new_id)),
            Self::Remove { id, selection } => (selection, Change::Remove(id)),
            Self::Move {
                id,
                position,
                selection,
            } => (selection, Change::Move(id, position)),
            Self::Pin {
                id,
                unpin,
                selection,
            } => (selection, Change::Pin(id, !unpin)),
            Self::Preview { file, selection } => {
                let template = decode(client, &file).await?;
                let workspace_id = selection.target.resolve(client).await?;
                return match client
                    .request(ClientMessage::PreviewWidget {
                        workspace_id,
                        name: template.name,
                        definition: template.definition,
                    })
                    .await?
                {
                    ResponsePayload::WidgetResult(result) => print_result(result, selection.json),
                    response => Err(unexpected_response(response)),
                };
            }
            Self::Refresh { id, selection } => return results(client, selection, Some(id)).await,
            Self::Results(selection) => return results(client, selection, None).await,
            Self::Invoke {
                id,
                action,
                selection,
            } => {
                let workspace_id = selection.target.resolve(client).await?;
                return match client
                    .request(ClientMessage::InvokeWidgetAction {
                        workspace_id,
                        widget: id,
                        action,
                    })
                    .await?
                {
                    ResponsePayload::WidgetAction(result) => {
                        print_json(serde_json::to_value(result)?)
                    }
                    response => Err(unexpected_response(response)),
                };
            }
            Self::Template { action } => return action.run(client).await,
        };
        let id = selection.target.resolve(client).await?;
        let mut state = workbench(client, id).await?;
        change.apply(&mut state.document)?;
        let saved = update(client, id, state.revision, state.document).await?;
        super::workspace_config::print_state(&saved, selection.json)
    }
}

async fn results(
    client: &mut DaemonClient,
    selection: WidgetTarget,
    refresh: Option<String>,
) -> Result<(), ClientError> {
    let workspace_id = selection.target.resolve(client).await?;
    match client
        .request(ClientMessage::GetWidgetResults {
            workspace_id,
            refresh: refresh.clone(),
        })
        .await?
    {
        ResponsePayload::WidgetResults(results) => {
            let results = results
                .into_iter()
                .filter(|result| refresh.as_ref().is_none_or(|id| &result.id == id))
                .collect::<Vec<_>>();
            if selection.json {
                return print_json(serde_json::to_value(results)?);
            }
            for result in results {
                println!("{}  {}", result.id, result.result.label);
            }
            Ok(())
        }
        response => Err(unexpected_response(response)),
    }
}

fn print_result(
    result: daedalus_proto::workbench::WidgetResult,
    json: bool,
) -> Result<(), ClientError> {
    if json {
        return print_json(serde_json::to_value(result)?);
    }
    println!("{}", result.label);
    for (name, value) in result.details {
        println!("{name}  {value}");
    }
    Ok(())
}

enum Change {
    Create {
        id: String,
        name: String,
        template: Option<WidgetTemplate>,
        definition: Option<WidgetDefinition>,
    },
    Edit {
        id: String,
        draft: Option<WidgetTemplate>,
        name: Option<String>,
    },
    Duplicate(String, String),
    Remove(String),
    Move(String, usize),
    Pin(String, bool),
}

impl Change {
    fn apply(self, document: &mut WorkspaceDocument) -> Result<(), ClientError> {
        match self {
            Self::Create {
                id,
                name,
                template,
                definition,
            } => {
                let mut instance = WidgetInstance {
                    id,
                    name,
                    pinned: true,
                    template: None,
                    definition,
                };
                if let Some(template) = template {
                    attach_template(document, &mut instance, template);
                } else if instance.definition.is_none() {
                    instance.definition = Some(WidgetDefinition::default());
                }
                document.widgets.push(instance);
            }
            Self::Edit { id, draft, name } => {
                if draft.is_none() && name.is_none() {
                    return Err(
                        "Provide --file with an edited widget definition or --name to rename it."
                            .into(),
                    );
                }
                let index = widget_index(document, &id)?;
                let widget = &mut document.widgets[index];
                if let Some(draft) = draft {
                    widget.name = draft.name;
                    widget.definition = Some(draft.definition);
                }
                if let Some(name) = name {
                    widget.name = name;
                }
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

fn find_widget<'a>(
    document: &'a WorkspaceDocument,
    id: &str,
) -> Result<&'a WidgetInstance, ClientError> {
    Ok(&document.widgets[widget_index(document, id)?])
}

fn widget_index(document: &WorkspaceDocument, id: &str) -> Result<usize, ClientError> {
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

#[derive(Debug, Subcommand)]
pub(crate) enum TemplateAction {
    List {
        #[arg(long)]
        json: bool,
    },
    Create {
        #[arg(long)]
        file: PathBuf,
    },
    Edit {
        #[arg(long)]
        file: PathBuf,
    },
    Validate {
        #[arg(long)]
        file: PathBuf,
    },
    Import {
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        replace: bool,
    },
    Export {
        id: String,
        #[arg(long)]
        file: Option<PathBuf>,
    },
    Apply {
        template: String,
        id: String,
        #[command(flatten)]
        selection: WidgetTarget,
    },
}

impl TemplateAction {
    async fn run(self, client: &mut DaemonClient) -> Result<(), ClientError> {
        match self {
            Self::List { json } => {
                let library = library(client).await?;
                if json {
                    return print_json(serde_json::to_value(&library)?);
                }
                for template in library.templates {
                    println!("{}  {}", template.id, template.name);
                }
            }
            Self::Create { file }
            | Self::Import {
                file,
                replace: false,
            } => save_template(client, &file, false).await?,
            Self::Edit { file }
            | Self::Import {
                file,
                replace: true,
            } => save_template(client, &file, true).await?,
            Self::Validate { file } => {
                println!("{} is valid", decode(client, &file).await?.name);
            }
            Self::Export { id, file } => {
                let template = find_template(library(client).await?, &id)?;
                match client
                    .request(ClientMessage::EncodeWidgetTemplate { template })
                    .await?
                {
                    ResponsePayload::WorkbenchToml(text) => write_output(file.as_deref(), &text)?,
                    response => return Err(unexpected_response(response)),
                }
            }
            Self::Apply {
                template,
                id,
                selection,
            } => {
                return Box::pin(
                    Action::Create {
                        id,
                        file: None,
                        template: Some(template),
                        name: None,
                        selection,
                    }
                    .run(client),
                )
                .await;
            }
        }
        Ok(())
    }
}

async fn save_template(
    client: &mut DaemonClient,
    file: &Path,
    replace: bool,
) -> Result<(), ClientError> {
    let template = decode(client, file).await?;
    let mut library = library(client).await?;
    if let Some(existing) = library
        .templates
        .iter_mut()
        .find(|existing| existing.id == template.id)
    {
        if !replace {
            return Err(format!("Template '{}' already exists", template.id).into());
        }
        *existing = template;
    } else {
        library.templates.push(template);
    }
    match client
        .request(ClientMessage::UpdateWidgetTemplates {
            revision: library.revision,
            templates: library.templates,
        })
        .await?
    {
        ResponsePayload::WidgetLibrary(saved) => {
            println!("Templates saved at revision {}", saved.revision);
            Ok(())
        }
        ResponsePayload::WidgetLibraryConflict(_) => {
            Err("The template library changed. Review it and retry.".into())
        }
        response => Err(unexpected_response(response)),
    }
}

async fn library(client: &mut DaemonClient) -> Result<WidgetLibrary, ClientError> {
    match client.request(ClientMessage::ListWidgetTemplates).await? {
        ResponsePayload::WidgetLibrary(library) => Ok(library),
        response => Err(unexpected_response(response)),
    }
}

fn find_template(library: WidgetLibrary, id: &str) -> Result<WidgetTemplate, ClientError> {
    library
        .templates
        .into_iter()
        .find(|template| template.id == id)
        .ok_or_else(|| format!("No template with ID '{id}'").into())
}

async fn decode(client: &mut DaemonClient, path: &Path) -> Result<WidgetTemplate, ClientError> {
    decode_text(client, read_input(path)?).await
}

async fn decode_text(
    client: &mut DaemonClient,
    text: String,
) -> Result<WidgetTemplate, ClientError> {
    match client
        .request(ClientMessage::DecodeWidgetTemplate { text })
        .await?
    {
        ResponsePayload::WidgetTemplate(template) => Ok(template),
        response => Err(unexpected_response(response)),
    }
}

fn print_json(value: serde_json::Value) -> Result<(), ClientError> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
