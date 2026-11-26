use super::{WidgetTarget, instances::Change, print_json};
use crate::client::{ClientError, DaemonClient};
use crate::commands::workspace_config::{read_input, write_output};
use crate::unexpected_response;
use clap::Subcommand;
use daedalus_proto::workbench::{WidgetLibrary, WidgetTemplate};
use daedalus_proto::{ClientMessage, ResponsePayload};
use std::path::{Path, PathBuf};

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
    pub(super) async fn run(self, client: &mut DaemonClient) -> Result<(), ClientError> {
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
                let template = load_template(client, &id).await?;
                write_output(file.as_deref(), &encode(client, template).await?)?;
            }
            Self::Apply {
                template,
                id,
                selection,
            } => {
                let template = load_template(client, &template).await?;
                Change::create(id, None, Some(template), None)
                    .save(client, selection)
                    .await?;
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

pub(super) async fn load_template(
    client: &mut DaemonClient,
    id: &str,
) -> Result<WidgetTemplate, ClientError> {
    library(client)
        .await?
        .templates
        .into_iter()
        .find(|template| template.id == id)
        .ok_or_else(|| format!("No template with ID '{id}'").into())
}

pub(super) async fn decode(
    client: &mut DaemonClient,
    path: &Path,
) -> Result<WidgetTemplate, ClientError> {
    decode_text(client, read_input(path)?).await
}

pub(super) async fn decode_text(
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

pub(super) async fn encode(
    client: &mut DaemonClient,
    template: WidgetTemplate,
) -> Result<String, ClientError> {
    match client
        .request(ClientMessage::EncodeWidgetTemplate { template })
        .await?
    {
        ResponsePayload::WorkbenchToml(text) => Ok(text),
        response => Err(unexpected_response(response)),
    }
}
