mod configuration;
mod editor;
mod instances;
mod templates;

use super::workspace_config::{Target, workbench};
use crate::client::{ClientError, DaemonClient};
use crate::unexpected_response;
use clap::{Args, Subcommand, ValueEnum};
use daedalus_proto::workbench::{WidgetDisplay, WidgetElement, WidgetStyle};
use daedalus_proto::{ClientMessage, ResponsePayload};
use instances::{Change, find_widget};
use std::path::PathBuf;
use templates::{TemplateAction, decode, load_template};

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum Style {
    Badge,
    Button,
    Led,
}

impl From<Style> for WidgetStyle {
    fn from(style: Style) -> Self {
        match style {
            Style::Badge => Self::Badge,
            Style::Button => Self::Button,
            Style::Led => Self::Led,
        }
    }
}

#[derive(Debug, Default, Args)]
pub(crate) struct DisplayOverrides {
    /// Set the widget's appearance in the bottom bar.
    #[arg(long, value_enum)]
    style: Option<Style>,
    /// Set ordered widget elements. Use [] to clear them.
    #[arg(long, value_name = "JSON", value_parser = parse_elements)]
    elements: Option<Elements>,
}

impl DisplayOverrides {
    fn is_empty(&self) -> bool {
        self.style.is_none() && self.elements.is_none()
    }

    fn matches(&self, display: &WidgetDisplay) -> bool {
        self.style
            .is_none_or(|style| WidgetStyle::from(style) == display.style)
            && self
                .elements
                .as_ref()
                .is_none_or(|elements| elements.0 == display.elements)
    }

    fn apply(self, display: &mut WidgetDisplay) {
        if let Some(style) = self.style {
            display.style = style.into();
        }
        if let Some(Elements(elements)) = self.elements {
            display.elements = elements;
        }
    }
}

#[derive(Clone, Debug)]
struct Elements(Vec<WidgetElement>);

fn parse_elements(value: &str) -> Result<Elements, String> {
    serde_json::from_str(value)
        .map(Elements)
        .map_err(|error| format!("Provide a JSON array of widget elements. {error}"))
}

#[derive(Debug, Subcommand)]
pub(crate) enum Action {
    List(WidgetTarget),
    Show {
        id: String,
        /// Emit the resolved definition and saved revision as JSON.
        #[arg(long)]
        effective: bool,
        #[command(flatten)]
        selection: WidgetTarget,
    },
    /// Apply a JSON merge patch to a widget.
    Configure(configuration::Configure),
    Create {
        id: String,
        #[arg(long, conflicts_with = "template")]
        file: Option<PathBuf>,
        #[arg(long)]
        template: Option<String>,
        #[arg(long)]
        name: Option<String>,
        #[command(flatten)]
        display: DisplayOverrides,
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
        display: DisplayOverrides,
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
            Self::Show {
                id,
                effective,
                selection,
            } => {
                let workspace = selection.target.resolve(client).await?;
                let state = workbench(client, workspace).await?;
                if effective {
                    return print_json(configuration::effective(&state, &id)?);
                }
                let widget = find_widget(&state.document, &id)?;
                let display = state
                    .document
                    .widget_definition(widget)
                    .map(|definition| &definition.display);
                let mut result = serde_json::to_value(widget)?;
                result["style"] =
                    serde_json::to_value(display.map(|display| display.style).unwrap_or_default())?;
                result["elements"] = serde_json::to_value(
                    display
                        .map(|display| display.elements.as_slice())
                        .unwrap_or_default(),
                )?;
                return print_json(result);
            }
            Self::Configure(configure) => return configure.run(client).await,
            Self::Create {
                id,
                file,
                template,
                name,
                display,
                selection,
            } => {
                let (template, inline) = match (template, file) {
                    (Some(template), _) => (Some(load_template(client, &template).await?), None),
                    (None, Some(path)) => (None, Some(decode(client, &path).await?)),
                    (None, None) => (None, None),
                };
                (
                    selection,
                    Change::create(id, name, template, inline, display),
                )
            }
            Self::Edit {
                id,
                file,
                name,
                display,
                selection,
            } => {
                if file.is_none() && name.is_none() && display.is_empty() {
                    return editor::edit(client, id, selection).await;
                }
                let draft = match file {
                    Some(path) => Some(decode(client, &path).await?),
                    None => None,
                };
                (
                    selection,
                    Change::Edit {
                        id,
                        draft,
                        name,
                        display,
                    },
                )
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
        change.save(client, selection).await
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

fn print_json(value: serde_json::Value) -> Result<(), ClientError> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
