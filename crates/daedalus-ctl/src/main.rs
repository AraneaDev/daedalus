mod client;

use clap::{Parser, Subcommand, ValueEnum};
use client::DaemonClient;
use daedalus_proto::{
    default_socket_path, wrap_bracketed_paste, ClientMessage, PaneId, PaneRole, ResponsePayload,
    StatusLevel, WorkspaceId,
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
    Worktree {
        #[command(subcommand)]
        action: WorktreeAction,
    },
    Status {
        #[command(subcommand)]
        action: StatusAction,
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
        #[arg(long)]
        compose_down: bool,
    },
}

#[derive(Debug, Subcommand)]
enum WorktreeAction {
    List {
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    Create {
        branch: String,
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        existing: bool,
        #[arg(long = "env")]
        env: Vec<String>,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum LevelArg {
    Info,
    Ok,
    Warn,
    Error,
}

impl From<LevelArg> for StatusLevel {
    fn from(level: LevelArg) -> Self {
        match level {
            LevelArg::Info => StatusLevel::Info,
            LevelArg::Ok => StatusLevel::Ok,
            LevelArg::Warn => StatusLevel::Warn,
            LevelArg::Error => StatusLevel::Error,
        }
    }
}

#[derive(Debug, Subcommand)]
enum StatusAction {
    Publish {
        #[arg(long)]
        source: String,
        #[arg(long)]
        label: String,
        #[arg(long, value_enum, default_value_t = LevelArg::Info)]
        level: LevelArg,
        #[arg(long, conflicts_with = "dir")]
        workspace: Option<Uuid>,
        #[arg(long, conflicts_with = "workspace")]
        dir: Option<PathBuf>,
        #[arg(long)]
        detail: Option<String>,
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

fn resolve_dir(dir: Option<PathBuf>) -> Result<PathBuf, std::io::Error> {
    match dir {
        Some(d) if d.is_relative() => {
            let joined = std::env::current_dir()?.join(d);
            Ok(std::fs::canonicalize(&joined).unwrap_or(joined))
        }
        Some(d) => Ok(d),
        None => std::env::current_dir(),
    }
}

fn unexpected_response(payload: ResponsePayload) -> Box<dyn std::error::Error> {
    format!("unexpected response payload: {payload:?}").into()
}

fn expect_ok(payload: ResponsePayload) -> Result<(), Box<dyn std::error::Error>> {
    match payload {
        ResponsePayload::Ok => Ok(()),
        other => Err(unexpected_response(other)),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let socket = cli.socket.unwrap_or_else(default_socket_path);
    let mut client = DaemonClient::connect(&socket).await?;

    match cli.command {
        Commands::Ping => match client.request(ClientMessage::Ping).await? {
            ResponsePayload::Pong {
                version,
                vt_backend,
                isolation_mode,
            } => {
                println!("daedalusd v{version} (vt={vt_backend}, isolation={isolation_mode})");
            }
            other => return Err(unexpected_response(other)),
        },
        Commands::Workspace { action } => match action {
            WorkspaceAction::Create { name, dir, env } => {
                let root_dir = resolve_dir(Some(dir))?;
                let env_map = parse_env_pairs(&env)?;
                match client
                    .request(ClientMessage::CreateWorkspace {
                        name,
                        root_dir,
                        env: env_map,
                    })
                    .await?
                {
                    ResponsePayload::WorkspaceCreated(ws) => println!("{}", ws.id),
                    other => return Err(unexpected_response(other)),
                }
            }
            WorkspaceAction::List => match client.request(ClientMessage::ListWorkspaces).await? {
                ResponsePayload::Workspaces(list) => {
                    for ws in list {
                        let mut line = format!(
                            "{}  {}  ({})  panes={}",
                            ws.id,
                            ws.name,
                            ws.root_dir.display(),
                            ws.panes.len()
                        );
                        if let Some(branch) = &ws.git_branch {
                            line.push_str(&format!("  branch={branch}"));
                        }
                        if ws.port_offset > 0 {
                            line.push_str(&format!("  offset=+{}", ws.port_offset));
                        }
                        if !ws.external_statuses.is_empty() {
                            let badges: Vec<String> = ws
                                .external_statuses
                                .iter()
                                .map(|s| format!("{}:{}", s.source, s.label))
                                .collect();
                            line.push_str(&format!("  status=[{}]", badges.join(", ")));
                        }
                        println!("{line}");
                    }
                }
                other => return Err(unexpected_response(other)),
            },
            WorkspaceAction::Close { id, compose_down } => {
                expect_ok(
                    client
                        .request(ClientMessage::CloseWorkspace {
                            id: WorkspaceId(id),
                            compose_down,
                        })
                        .await?,
                )?;
                println!("closed {id}");
            }
        },
        Commands::Worktree { action } => match action {
            WorktreeAction::List { dir } => {
                let repo_dir = resolve_dir(dir)?;
                match client
                    .request(ClientMessage::DiscoverWorktrees { repo_dir })
                    .await?
                {
                    ResponsePayload::Worktrees(worktrees) => {
                        for wt in worktrees {
                            let branch = wt.branch.as_deref().unwrap_or("<detached>");
                            let short_head: String = wt.head.chars().take(8).collect();
                            let tag = if wt.is_primary { " [primary]" } else { "" };
                            println!("{}  {}  ({}){}", wt.path.display(), branch, short_head, tag);
                        }
                    }
                    other => return Err(unexpected_response(other)),
                }
            }
            WorktreeAction::Create {
                branch,
                dir,
                existing,
                env,
            } => {
                let repo_dir = resolve_dir(dir)?;
                let env_map = parse_env_pairs(&env)?;
                match client
                    .request(ClientMessage::CreateWorktreeWorkspace {
                        repo_dir,
                        branch,
                        create_branch: !existing,
                        env: env_map,
                    })
                    .await?
                {
                    ResponsePayload::WorkspaceCreated(ws) => println!("{}", ws.id),
                    other => return Err(unexpected_response(other)),
                }
            }
        },
        Commands::Status { action } => match action {
            StatusAction::Publish {
                source,
                label,
                level,
                workspace,
                dir,
                detail,
            } => {
                let root_dir = if workspace.is_some() {
                    None
                } else {
                    Some(resolve_dir(dir)?)
                };
                expect_ok(
                    client
                        .request(ClientMessage::PublishExternalStatus {
                            workspace_id: workspace.map(WorkspaceId),
                            root_dir,
                            source,
                            level: level.into(),
                            label,
                            detail,
                        })
                        .await?,
                )?;
                println!("published");
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
                match client
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
                    ResponsePayload::PaneSpawned(pane) => println!("{}", pane.id),
                    other => return Err(unexpected_response(other)),
                }
            }
            PaneAction::Close { pane_id } => {
                expect_ok(
                    client
                        .request(ClientMessage::ClosePane {
                            pane_id: PaneId(pane_id),
                        })
                        .await?,
                )?;
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
                expect_ok(
                    client
                        .request(ClientMessage::SendInput {
                            pane_id: PaneId(pane_id),
                            data: bytes,
                        })
                        .await?,
                )?;
            }
            PaneAction::Resize {
                pane_id,
                cols,
                rows,
            } => {
                expect_ok(
                    client
                        .request(ClientMessage::ResizePane {
                            pane_id: PaneId(pane_id),
                            cols,
                            rows,
                        })
                        .await?,
                )?;
            }
            PaneAction::Snapshot { pane_id } => {
                match client
                    .request(ClientMessage::GetSnapshot {
                        pane_id: PaneId(pane_id),
                    })
                    .await?
                {
                    ResponsePayload::Snapshot(snap) => {
                        for row in snap.rows_data {
                            let line: String = row.cells.iter().map(|c| c.text.as_str()).collect();
                            println!("{}", line.trim_end());
                        }
                    }
                    other => return Err(unexpected_response(other)),
                }
            }
            PaneAction::Trace { pane_id } => {
                match client
                    .request(ClientMessage::GetFailureTrace {
                        pane_id: PaneId(pane_id),
                    })
                    .await?
                {
                    ResponsePayload::FailureTrace(trace) => match trace {
                        Some(t) => {
                            println!(
                                "exit_code={} command={}",
                                t.exit_code,
                                t.command.as_deref().unwrap_or("<unknown>")
                            );
                            println!("{}", t.output);
                        }
                        None => println!("no failure trace"),
                    },
                    other => return Err(unexpected_response(other)),
                }
            }
        },
        Commands::Events { pane, json } => {
            expect_ok(
                client
                    .request(ClientMessage::Subscribe {
                        pane_ids: pane.map(|p| vec![PaneId(p)]),
                    })
                    .await?,
            )?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_phase3_cli_subcommands_and_flags() {
        let wt_list = Cli::try_parse_from(["daedalus-ctl", "worktree", "list", "--dir", "/repo"])
            .expect("parse worktree list");
        assert!(matches!(
            wt_list.command,
            Commands::Worktree {
                action: WorktreeAction::List { dir: Some(_) }
            }
        ));

        let wt_create = Cli::try_parse_from([
            "daedalus-ctl",
            "worktree",
            "create",
            "feat/auth",
            "--dir",
            "/repo",
            "--existing",
            "--env",
            "FOO=bar",
        ])
        .expect("parse worktree create");
        match wt_create.command {
            Commands::Worktree {
                action:
                    WorktreeAction::Create {
                        branch,
                        dir,
                        existing,
                        env,
                    },
            } => {
                assert_eq!(branch, "feat/auth");
                assert_eq!(dir, Some(PathBuf::from("/repo")));
                assert!(existing);
                assert_eq!(env, vec!["FOO=bar"]);
            }
            other => panic!("unexpected command: {other:?}"),
        }

        let id = Uuid::new_v4();
        let id_str = id.to_string();
        let ws_close =
            Cli::try_parse_from(["daedalus-ctl", "workspace", "close", &id_str, "--compose-down"])
                .expect("parse workspace close --compose-down");
        assert!(matches!(
            ws_close.command,
            Commands::Workspace {
                action: WorkspaceAction::Close {
                    compose_down: true,
                    ..
                }
            }
        ));

        let status = Cli::try_parse_from([
            "daedalus-ctl",
            "status",
            "publish",
            "--source",
            "phrourion",
            "--label",
            "2 dirty",
            "--level",
            "warn",
            "--dir",
            "/repo",
            "--detail",
            "2 modified",
        ])
        .expect("parse status publish");
        match status.command {
            Commands::Status {
                action:
                    StatusAction::Publish {
                        source,
                        label,
                        level,
                        workspace,
                        dir,
                        detail,
                    },
            } => {
                assert_eq!(source, "phrourion");
                assert_eq!(label, "2 dirty");
                assert_eq!(StatusLevel::from(level), StatusLevel::Warn);
                assert!(workspace.is_none());
                assert_eq!(dir, Some(PathBuf::from("/repo")));
                assert_eq!(detail.as_deref(), Some("2 modified"));
            }
            other => panic!("unexpected command: {other:?}"),
        }

        let conflict = Cli::try_parse_from([
            "daedalus-ctl",
            "status",
            "publish",
            "--source",
            "phrourion",
            "--label",
            "ok",
            "--workspace",
            &id_str,
            "--dir",
            "/repo",
        ]);
        assert!(conflict.is_err());
    }

    #[test]
    fn resolve_dir_makes_relative_paths_absolute() {
        let resolved = resolve_dir(Some(PathBuf::from("."))).expect("resolve '.'");
        assert!(
            resolved.is_absolute(),
            "expected absolute path, got {}",
            resolved.display()
        );
        let cwd = std::env::current_dir().unwrap();
        let expected = std::fs::canonicalize(&cwd).unwrap_or(cwd);
        assert_eq!(resolved, expected);
    }
}
