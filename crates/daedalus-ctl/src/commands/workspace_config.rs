use crate::client::{ClientError, DaemonClient};
use crate::{resolve_dir, unexpected_response};
use clap::Args;
use daedalus_proto::workbench::{
    ConflictChoice, WorkbenchState, WorkspaceDocument, WorkspaceImportSource,
};
use daedalus_proto::{ClientMessage, ResponsePayload, WorkspaceId};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Debug, Args)]
pub(super) struct Target {
    #[arg(long, conflicts_with = "dir")]
    workspace: Option<uuid::Uuid>,
    #[arg(long, conflicts_with = "workspace")]
    dir: Option<PathBuf>,
}

impl Target {
    pub async fn resolve(self, client: &mut DaemonClient) -> Result<WorkspaceId, ClientError> {
        if let Some(id) = self.workspace {
            return Ok(WorkspaceId(id));
        }
        match client
            .request(ClientMessage::ResolveWorkspaceDirectory {
                root_dir: resolve_dir(self.dir)?,
            })
            .await?
        {
            ResponsePayload::WorkspaceResolved(workspace) => Ok(workspace.id),
            response => Err(unexpected_response(response)),
        }
    }
}

#[derive(Debug, Args)]
pub(crate) struct Configure {
    #[command(flatten)]
    target: Target,
    #[arg(long)]
    json: bool,
    /// Edit a copy of the settings using VISUAL or EDITOR.
    #[arg(long)]
    edit: bool,
}

impl Configure {
    pub async fn run(self, client: &mut DaemonClient) -> Result<(), ClientError> {
        let id = self.target.resolve(client).await?;
        if self.edit {
            let current = workbench(client, id).await?;
            let edited = edit_text(&export(client, id).await?)?;
            let preview = match client
                .request(ClientMessage::PreviewWorkspaceImport {
                    workspace_id: id,
                    source: WorkspaceImportSource::Text(edited),
                    linked: false,
                })
                .await?
            {
                ResponsePayload::WorkspaceImportPreview(preview) => preview,
                response => return Err(unexpected_response(response)),
            };
            return print_state(
                &update(client, id, current.revision, preview.incoming).await?,
                self.json,
            );
        }
        if self.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&workbench(client, id).await?)?
            );
        } else {
            print!("{}", export(client, id).await?);
        }
        Ok(())
    }
}

#[derive(Debug, Args)]
pub(crate) struct Import {
    #[command(flatten)]
    target: Target,
    /// Create a workspace in this daemon-host directory instead of updating one.
    #[arg(long, conflicts_with_all = ["workspace", "dir"])]
    new: Option<PathBuf>,
    #[arg(long, conflicts_with = "repo", required_unless_present = "repo")]
    file: Option<PathBuf>,
    #[arg(long)]
    repo: bool,
    #[arg(long, requires = "repo")]
    copy: bool,
    #[command(flatten)]
    options: ApplyOptions,
}

impl Import {
    pub async fn run(self, client: &mut DaemonClient) -> Result<(), ClientError> {
        let source = match self.file {
            Some(path) => WorkspaceImportSource::Text(read_input(&path)?),
            None => WorkspaceImportSource::Repo,
        };
        if let Some(root_dir) = self.new {
            let preview = match client
                .request(ClientMessage::PreviewWorkspaceCreation {
                    root_dir: resolve_dir(Some(root_dir))?,
                    source,
                    linked: self.repo && !self.copy,
                })
                .await?
            {
                ResponsePayload::WorkspaceCreationPreview(preview) => preview,
                response => return Err(unexpected_response(response)),
            };
            if self.options.dry_run {
                if self.options.json {
                    println!("{}", serde_json::to_string_pretty(&preview)?);
                } else {
                    println!(
                        "Create {} in {}",
                        preview.incoming.name,
                        preview.root_dir.display()
                    );
                }
                return Ok(());
            }
            return match client
                .request(ClientMessage::ApplyWorkspaceCreation { preview })
                .await?
            {
                ResponsePayload::WorkbenchCreated(workspace) => {
                    if self.options.json {
                        println!("{}", serde_json::to_string_pretty(&workspace.id)?);
                    } else {
                        println!("{}", workspace.id);
                    }
                    Ok(())
                }
                response => Err(unexpected_response(response)),
            };
        }
        let id = self.target.resolve(client).await?;
        import(client, id, source, self.repo && !self.copy, self.options).await
    }
}

#[derive(Debug, Args)]
pub(crate) struct Export {
    #[command(flatten)]
    target: Target,
    #[arg(long)]
    file: Option<PathBuf>,
}

impl Export {
    pub async fn run(self, client: &mut DaemonClient) -> Result<(), ClientError> {
        let id = self.target.resolve(client).await?;
        write_output(self.file.as_deref(), &export(client, id).await?)
    }
}

#[derive(Debug, Args)]
pub(crate) struct Update {
    #[command(flatten)]
    target: Target,
    #[command(flatten)]
    options: ApplyOptions,
}

impl Update {
    pub async fn run(self, client: &mut DaemonClient) -> Result<(), ClientError> {
        let id = self.target.resolve(client).await?;
        if workbench(client, id).await?.repo_baseline.is_none() {
            return Err("This workspace has no repo link. Use workspace import --repo to import the shared file first.".into());
        }
        import(client, id, WorkspaceImportSource::Repo, true, self.options).await
    }
}

#[derive(Debug, Args)]
struct ApplyOptions {
    #[arg(long)]
    dry_run: bool,
    #[arg(long)]
    json: bool,
    /// Resolve a conflict with /field/path=local or /field/path=incoming.
    #[arg(long, value_parser = parse_resolution)]
    resolve: Vec<(String, ConflictChoice)>,
}

fn parse_resolution(value: &str) -> Result<(String, ConflictChoice), String> {
    let (path, side) = value
        .rsplit_once('=')
        .ok_or("Use /field/path=local or /field/path=incoming")?;
    let choice = match side {
        "local" => ConflictChoice::Local,
        "incoming" => ConflictChoice::Incoming,
        _ => return Err("Choose local or incoming".into()),
    };
    Ok((path.into(), choice))
}

async fn import(
    client: &mut DaemonClient,
    id: WorkspaceId,
    source: WorkspaceImportSource,
    linked: bool,
    options: ApplyOptions,
) -> Result<(), ClientError> {
    let mut preview = match client
        .request(ClientMessage::PreviewWorkspaceImport {
            workspace_id: id,
            source,
            linked,
        })
        .await?
    {
        ResponsePayload::WorkspaceImportPreview(preview) => preview,
        response => return Err(unexpected_response(response)),
    };
    let resolutions = options.resolve.into_iter().collect::<BTreeMap<_, _>>();
    let unresolved = preview
        .conflicts
        .iter()
        .any(|conflict| !resolutions.contains_key(&conflict.path));
    if options.dry_run || unresolved {
        if !resolutions.is_empty() && !preview.conflicts.is_empty() {
            preview = match client
                .request(ClientMessage::ResolveWorkspaceImport {
                    workspace_id: id,
                    revision: preview.revision,
                    incoming: preview.incoming,
                    linked,
                    resolutions,
                })
                .await?
            {
                ResponsePayload::WorkspaceImportPreview(preview) => preview,
                response => return Err(unexpected_response(response)),
            };
        }
        if options.json {
            println!("{}", serde_json::to_string_pretty(&preview)?);
        } else {
            println!(
                "{} → {} (revision {})",
                preview.current.name, preview.proposed.name, preview.revision
            );
            println!(
                "{} assignments, {} widgets, {} templates",
                preview.proposed.assignments.len(),
                preview.proposed.widgets.len(),
                preview.proposed.templates.len()
            );
            for conflict in &preview.conflicts {
                println!("Conflict at {}", conflict.path);
            }
        }
        if !options.dry_run {
            return Err("Resolve the listed conflicts and apply again.".into());
        }
        return Ok(());
    }
    let state = applied(
        client
            .request(ClientMessage::ApplyWorkspaceImport {
                workspace_id: id,
                revision: preview.revision,
                incoming: preview.incoming,
                linked,
                resolutions,
            })
            .await?,
    )?;
    print_state(&state, options.json)
}

pub(super) async fn workbench(
    client: &mut DaemonClient,
    id: WorkspaceId,
) -> Result<WorkbenchState, ClientError> {
    match client
        .request(ClientMessage::GetWorkbench { workspace_id: id })
        .await?
    {
        ResponsePayload::Workbench(state) => Ok(state),
        response => Err(unexpected_response(response)),
    }
}

pub(super) async fn update(
    client: &mut DaemonClient,
    id: WorkspaceId,
    revision: u64,
    document: WorkspaceDocument,
) -> Result<WorkbenchState, ClientError> {
    update_workbench(client, id, revision, document, None).await
}

pub(super) async fn update_widgets(
    client: &mut DaemonClient,
    id: WorkspaceId,
    revision: u64,
    document: WorkspaceDocument,
) -> Result<WorkbenchState, ClientError> {
    update_workbench(
        client,
        id,
        revision,
        document,
        Some(daedalus_proto::workbench::WorkbenchEdit::Widgets),
    )
    .await
}

async fn update_workbench(
    client: &mut DaemonClient,
    id: WorkspaceId,
    revision: u64,
    document: WorkspaceDocument,
    edit: Option<daedalus_proto::workbench::WorkbenchEdit>,
) -> Result<WorkbenchState, ClientError> {
    applied(
        client
            .request(ClientMessage::UpdateWorkbench {
                workspace_id: id,
                revision,
                document,
                layout_only: false,
                edit,
            })
            .await?,
    )
}

fn applied(response: ResponsePayload) -> Result<WorkbenchState, ClientError> {
    match response {
        ResponsePayload::Workbench(state) => Ok(state),
        ResponsePayload::WorkbenchConflict(state) => Err(format!(
            "Workspace changed at revision {}. Review it and retry.",
            state.revision
        )
        .into()),
        response => Err(unexpected_response(response)),
    }
}

async fn export(client: &mut DaemonClient, id: WorkspaceId) -> Result<String, ClientError> {
    match client
        .request(ClientMessage::ExportWorkspace { workspace_id: id })
        .await?
    {
        ResponsePayload::WorkbenchToml(text) => Ok(text),
        response => Err(unexpected_response(response)),
    }
}

pub(super) fn read_input(path: &Path) -> Result<String, ClientError> {
    if path == Path::new("-") {
        let mut text = String::new();
        std::io::stdin().lock().read_to_string(&mut text)?;
        Ok(text)
    } else {
        Ok(std::fs::read_to_string(path)?)
    }
}

pub(super) fn edit_text(text: &str) -> Result<String, ClientError> {
    let folder = tempfile::tempdir()?;
    let path = folder.path().join("definition.toml");
    std::fs::write(&path, text)?;
    let editor = std::env::var_os("VISUAL")
        .or_else(|| std::env::var_os("EDITOR"))
        .unwrap_or_else(|| {
            if cfg!(windows) {
                "notepad.exe".into()
            } else {
                "vi".into()
            }
        });
    if !std::process::Command::new(editor)
        .arg(&path)
        .status()?
        .success()
    {
        return Err("The editor exited before saving a definition.".into());
    }
    Ok(std::fs::read_to_string(path)?)
}

pub(super) fn write_output(path: Option<&Path>, text: &str) -> Result<(), ClientError> {
    match path.filter(|path| *path != Path::new("-")) {
        Some(path) => std::fs::write(path, text)?,
        None => print!("{text}"),
    }
    Ok(())
}

pub(super) fn print_state(state: &WorkbenchState, json: bool) -> Result<(), ClientError> {
    if json {
        println!("{}", serde_json::to_string_pretty(state)?);
    } else {
        println!(
            "{} saved at revision {}",
            state.document.name, state.revision
        );
    }
    Ok(())
}
