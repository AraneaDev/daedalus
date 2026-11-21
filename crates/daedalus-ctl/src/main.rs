mod client;

use clap::{Parser, Subcommand, ValueEnum};
use client::DaemonClient;
use daedalus_proto::{
    default_socket_path, wrap_bracketed_paste, ClientMessage, PaneId, PaneRole, ResponsePayload,
    WorkspaceId,
};
use std::collections::HashMap;
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Parser)]
#[command(name = "daedalus-ctl", about = "CLI control and smoke client for daedalusd")]
struct Cli {
    #[arg(long, global = true, env = "DAEDALUS_SOCKET")]
    socket: Option<PathBuf>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Ping,
    Workspace {
        #[command(subcommand)]
        action: WorkspaceAction,
    },
    Pane {
        #[command(subcommand)]
        action: PaneAction,
    },
    Events {
        #[arg(long)]
        pane: Option<Uuid>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum WorkspaceAction {
    Create {
        name: String,
        #[arg(long)]
        dir: PathBuf,
        #[arg(long = "env")]
        env: Vec<String>,
    },
    List,
    Close {
        id: Uuid,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum RoleArg {
    Agent,
    Service,
    Runner,
}

impl From<RoleArg> for PaneRole {
    fn from(r: RoleArg) -> Self {
        match r {
            RoleArg::Agent => PaneRole::Agent,
            RoleArg::Service => PaneRole::Service,
            RoleArg::Runner => PaneRole::Runner,
        }
    }
}

#[derive(Debug, Subcommand)]
enum PaneAction {
    Spawn {
        workspace_id: Uuid,
        #[arg(long, value_enum, default_value_t = RoleArg::Agent)]
        role: RoleArg,
        #[arg(long)]
        shell: Option<String>,
        #[arg(long, default_value_t = 120)]
        cols: u16,
        #[arg(long, default_value_t = 40)]
        rows: u16,
        #[arg(long = "env")]
        env: Vec<String>,
        #[arg(last = true)]
        command: Vec<String>,
    },
    Close {
        pane_id: Uuid,
    },
    Send {
        pane_id: Uuid,
        text: String,
        #[arg(long)]
        newline: bool,
        #[arg(long)]
        bracketed_paste: bool,
    },
    Resize {
        pane_id: Uuid,
        cols: u16,
        rows: u16,
    },
    Snapshot {
        pane_id: Uuid,
    },
    Trace {
        pane_id: Uuid,
    },
}

fn parse_env_pairs(pairs: &[String]) -> Result<HashMap<String, String>, String> {
    let mut map = HashMap::new();
    for pair in pairs {
        let (k, v) = pair
            .split_once('=')
            .ok_or_else(|| format!("invalid --env '{pair}', expected KEY=VALUE"))?;
        map.insert(k.to_string(), v.to_string());
    }
    Ok(map)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let socket = cli.socket.unwrap_or_else(default_socket_path);
    let mut client = DaemonClient::connect(&socket).await?;

    match cli.command {
        Commands::Ping => {
            if let ResponsePayload::Pong {
                version,
                vt_backend,
                isolation_mode,
            } = client.request(ClientMessage::Ping).await?
            {
                println!("daedalusd v{version} (vt={vt_backend}, isolation={isolation_mode})");
            }
        }
        Commands::Workspace { action } => match action {
            WorkspaceAction::Create { name, dir, env } => {
                let env_map = parse_env_pairs(&env)?;
                if let ResponsePayload::WorkspaceCreated(ws) = client
                    .request(ClientMessage::CreateWorkspace {
                        name,
                        root_dir: dir,
                        env: env_map,
                    })
                    .await?
                {
                    println!("{}", ws.id);
                }
            }
            WorkspaceAction::List => {
                if let ResponsePayload::Workspaces(list) =
                    client.request(ClientMessage::ListWorkspaces).await?
                {
                    for ws in list {
                        println!(
                            "{}  {}  ({})  panes={}",
                            ws.id,
                            ws.name,
                            ws.root_dir.display(),
                            ws.panes.len()
                        );
                    }
                }
            }
            WorkspaceAction::Close { id } => {
                client
                    .request(ClientMessage::CloseWorkspace {
                        id: WorkspaceId(id),
                    })
                    .await?;
                println!("closed {id}");
            }
        },
        Commands::Pane { action } => match action {
            PaneAction::Spawn {
                workspace_id,
                role,
                shell,
                cols,
                rows,
                env,
                command,
            } => {
                let env_map = parse_env_pairs(&env)?;
                let cmd = if command.is_empty() {
                    None
                } else {
                    Some(command)
                };
                if let ResponsePayload::PaneSpawned(pane) = client
                    .request(ClientMessage::SpawnPane {
                        workspace_id: WorkspaceId(workspace_id),
                        role: role.into(),
                        command: cmd,
                        shell,
                        cols,
                        rows,
                        env: env_map,
                    })
                    .await?
                {
                    println!("{}", pane.id);
                }
            }
            PaneAction::Close { pane_id } => {
                client
                    .request(ClientMessage::ClosePane {
                        pane_id: PaneId(pane_id),
                    })
                    .await?;
                println!("closed {pane_id}");
            }
            PaneAction::Send {
                pane_id,
                text,
                newline,
                bracketed_paste,
            } => {
                let mut bytes = if bracketed_paste {
                    wrap_bracketed_paste(text.as_bytes())
                } else {
                    text.into_bytes()
                };
                if newline {
                    bytes.push(b'\n');
                }
                client
                    .request(ClientMessage::SendInput {
                        pane_id: PaneId(pane_id),
                        data: bytes,
                    })
                    .await?;
            }
            PaneAction::Resize {
                pane_id,
                cols,
                rows,
            } => {
                client
                    .request(ClientMessage::ResizePane {
                        pane_id: PaneId(pane_id),
                        cols,
                        rows,
                    })
                    .await?;
            }
            PaneAction::Snapshot { pane_id } => {
                if let ResponsePayload::Snapshot(snap) = client
                    .request(ClientMessage::GetSnapshot {
                        pane_id: PaneId(pane_id),
                    })
                    .await?
                {
                    for row in snap.rows_data {
                        let line: String = row.cells.iter().map(|c| c.text.as_str()).collect();
                        println!("{}", line.trim_end());
                    }
                }
            }
            PaneAction::Trace { pane_id } => {
                if let ResponsePayload::FailureTrace(trace) = client
                    .request(ClientMessage::GetFailureTrace {
                        pane_id: PaneId(pane_id),
                    })
                    .await?
                {
                    match trace {
                        Some(t) => {
                            println!(
                                "exit_code={} command={}",
                                t.exit_code,
                                t.command.as_deref().unwrap_or("<unknown>")
                            );
                            println!("{}", t.output);
                        }
                        None => println!("no failure trace"),
                    }
                }
            }
        },
        Commands::Events { pane, json } => {
            client
                .request(ClientMessage::Subscribe {
                    pane_ids: pane.map(|p| vec![PaneId(p)]),
                })
                .await?;
            while let Some(evt) = client.next_event().await? {
                if json {
                    println!("{}", serde_json::to_string(&evt)?);
                } else {
                    println!("{evt:?}");
                }
            }
        }
    }

    Ok(())
}
