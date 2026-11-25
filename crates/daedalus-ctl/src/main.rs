mod client;
mod commands;

use clap::{Parser, Subcommand, ValueEnum};
use client::DaemonClient;
use daedalus_proto::{
    AgentState, ClientMessage, PaneRole, ResponsePayload, StatusLevel, default_socket_path,
};
use std::collections::HashMap;
use std::path::PathBuf;
use uuid::Uuid;

const DEFAULT_LAUNCHD_LABEL: &str = "dev.aranea.daedalusd";
const DEFAULT_DAEDALUSD_BIN: &str = "/usr/local/bin/daedalusd";

#[derive(Debug, Parser)]
#[command(
    name = "daedalus-ctl",
    about = "CLI control and smoke client for daedalusd"
)]
struct Cli {
    #[arg(long, global = true, env = "DAEDALUS_SOCKET")]
    socket: Option<PathBuf>,
    #[arg(
        long,
        global = true,
        env = "DAEDALUS_AUTH_TOKEN",
        hide_env_values = true
    )]
    auth_token: Option<String>,
    #[command(subcommand)]
    command: Commands,
}

#[cfg(test)]
#[test]
fn accepts_workspace_update_and_widget_creation() {
    Cli::try_parse_from([
        "daedalus-ctl",
        "workspace",
        "update",
        "--dir",
        ".",
        "--dry-run",
        "--json",
    ])
    .unwrap();
    Cli::try_parse_from(["daedalus-ctl", "widget", "create", "branch", "--file", "-"]).unwrap();
}

#[derive(Debug, Subcommand)]
enum Commands {
    Ping,
    Workspace {
        #[command(subcommand)]
        action: WorkspaceAction,
    },
    Widget {
        #[command(subcommand)]
        action: commands::widget::Action,
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
    Action {
        #[command(subcommand)]
        action: CustomActionCommand,
    },
    AgentState {
        #[command(subcommand)]
        action: AgentStateAction,
    },
    FailureTrace {
        pane_id: Uuid,
        #[arg(long)]
        json: bool,
    },
    Events {
        #[arg(long)]
        pane: Option<Uuid>,
        #[arg(long)]
        json: bool,
    },
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },
}

#[derive(Debug, Subcommand)]
enum CustomActionCommand {
    List {
        #[arg(long, conflicts_with = "dir")]
        workspace: Option<Uuid>,
        #[arg(long, conflicts_with = "workspace")]
        dir: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    Run {
        action_id: String,
        #[arg(long, conflicts_with = "dir")]
        workspace: Option<Uuid>,
        #[arg(long, conflicts_with = "workspace")]
        dir: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
enum ServiceAction {
    /// Print a macOS launchd user agent plist (~/Library/LaunchAgents/dev.aranea.daedalusd.plist)
    PrintLaunchd {
        #[arg(long)]
        binary: Option<PathBuf>,
        #[arg(long)]
        label: Option<String>,
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
    /// List recently used projects, newest first
    Recent,
    /// Inspect the current workbench configuration.
    Configure(commands::workspace_config::Configure),
    /// Preview or apply a portable workspace document.
    Import(commands::workspace_config::Import),
    /// Export the workspace and its embedded widget templates.
    Export(commands::workspace_config::Export),
    /// Preview or apply the shared configuration from the checkout.
    Update(commands::workspace_config::Update),
    Close {
        id: Uuid,
        #[arg(long)]
        compose_down: bool,
        #[arg(long)]
        remove_worktree: bool,
        #[arg(long)]
        force: bool,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum StatusPreset {
    Phrourion,
    Cassandra,
}

impl StatusPreset {
    fn canonical_source(self) -> &'static str {
        match self {
            Self::Phrourion => "phrourion",
            Self::Cassandra => "cassandra",
        }
    }
}

fn resolve_status_source(
    preset: Option<StatusPreset>,
    source: Option<String>,
) -> Result<String, String> {
    match (source, preset) {
        (Some(s), _) if !s.trim().is_empty() => Ok(s),
        (Some(_), Some(p)) => Ok(p.canonical_source().to_string()),
        (Some(_), None) => Err("status source cannot be empty".to_string()),
        (None, Some(p)) => Ok(p.canonical_source().to_string()),
        (None, None) => Err("either --preset or source must be provided".to_string()),
    }
}

fn parse_agent_state(s: &str) -> Result<AgentState, String> {
    AgentState::from_cli_name(s).ok_or_else(|| {
        format!(
            "invalid agent state '{s}'; expected one of: idle, thinking, waiting, approval, waiting_for_approval, error"
        )
    })
}

#[derive(Debug, Subcommand)]
enum AgentStateAction {
    Set {
        #[arg(value_parser = parse_agent_state)]
        state: AgentState,
        #[arg(long, conflicts_with_all = ["workspace", "dir"])]
        pane: Option<Uuid>,
        #[arg(long, conflicts_with_all = ["pane", "dir"])]
        workspace: Option<Uuid>,
        #[arg(long, conflicts_with_all = ["pane", "workspace"])]
        dir: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
enum StatusAction {
    Publish {
        #[arg(long, value_enum)]
        preset: Option<StatusPreset>,
        #[arg(long, required_unless_present = "preset")]
        source: Option<String>,
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
    Clear {
        #[arg(required_unless_present = "preset")]
        source: Option<String>,
        #[arg(long, value_enum)]
        preset: Option<StatusPreset>,
        #[arg(long, conflicts_with = "dir")]
        workspace: Option<Uuid>,
        #[arg(long, conflicts_with = "workspace")]
        dir: Option<PathBuf>,
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

/// Anchors a relative directory at the client's working directory. A path with a root is sent as
/// given, because on Windows `/home/dev/project` has no drive and may name a directory on a
/// daemon running in WSL or on another host.
fn resolve_dir(dir: Option<PathBuf>) -> Result<PathBuf, std::io::Error> {
    match dir {
        Some(d) if !d.has_root() => {
            let joined = std::env::current_dir()?.join(d);
            Ok(daedalus_proto::strip_windows_verbatim_prefix(
                std::fs::canonicalize(&joined).unwrap_or(joined),
            ))
        }
        Some(d) => Ok(d),
        None => std::env::current_dir(),
    }
}

fn resolve_target_root_dir(
    has_direct_id: bool,
    dir: Option<PathBuf>,
) -> Result<Option<PathBuf>, std::io::Error> {
    if has_direct_id {
        Ok(None)
    } else {
        Ok(Some(resolve_dir(dir)?))
    }
}

fn unexpected_response(payload: ResponsePayload) -> Box<dyn std::error::Error> {
    format!("unexpected response payload: {payload:?}").into()
}

/// Format unix seconds as `YYYY-MM-DD HH:MM` in UTC.
fn format_utc_minute(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Civil-from-days (Howard Hinnant), valid for the proleptic Gregorian calendar.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}",
        rem / 3_600,
        rem % 3_600 / 60
    )
}

fn format_recent_project(project: &daedalus_proto::RecentProject) -> String {
    format!(
        "{}\t{}\t{}",
        project.name,
        project.root_dir.display(),
        format_utc_minute(project.last_used)
    )
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

    if let Commands::Service { action } = cli.command {
        commands::service::execute(action, &socket);
        return Ok(());
    }

    let mut client = DaemonClient::connect_with_auth(&socket, cli.auth_token.as_deref()).await?;

    match cli.command {
        Commands::Ping => match client.request(ClientMessage::Ping).await? {
            ResponsePayload::Pong {
                version,
                vt_backend,
                isolation_mode,
                ..
            } => {
                println!("daedalusd v{version} (vt={vt_backend}, isolation={isolation_mode})");
            }
            other => return Err(unexpected_response(other)),
        },
        Commands::Workspace { action } => commands::workspace::execute(&mut client, action).await?,
        Commands::Widget { action } => action.run(&mut client).await?,
        Commands::Worktree { action } => commands::worktree::execute(&mut client, action).await?,
        Commands::Status { action } => commands::status::execute(&mut client, action).await?,
        Commands::Pane { action } => commands::pane::execute(&mut client, action).await?,
        Commands::Action { action } => {
            commands::action::execute(
                &mut client,
                action,
                socket.to_string_lossy().starts_with("ssh://"),
            )
            .await?
        }
        Commands::AgentState { action } => {
            commands::status::set_agent_state(&mut client, action).await?
        }
        Commands::FailureTrace { pane_id, json } => {
            commands::diagnostics::failure_trace(&mut client, pane_id, json).await?
        }
        Commands::Events { pane, json } => {
            commands::diagnostics::events(&mut client, pane, json).await?
        }
        Commands::Service { .. } => unreachable!("service commands exit before daemon connect"),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::service::{render_launchd_plist, resolve_daedalusd_binary};
    use std::path::Path;

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
        let ws_close = Cli::try_parse_from([
            "daedalus-ctl",
            "workspace",
            "close",
            &id_str,
            "--compose-down",
        ])
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

        let recent = Cli::try_parse_from(["daedalus-ctl", "workspace", "recent"])
            .expect("parse workspace recent");
        assert!(matches!(
            recent.command,
            Commands::Workspace {
                action: WorkspaceAction::Recent
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
                        preset,
                        source,
                        label,
                        level,
                        workspace,
                        dir,
                        detail,
                    },
            } => {
                assert!(preset.is_none());
                assert_eq!(source.as_deref(), Some("phrourion"));
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
    fn parses_v0_0_3_cli_subcommands_and_presets() {
        let id = Uuid::new_v4();
        let id_str = id.to_string();

        let ws_close = Cli::try_parse_from([
            "daedalus-ctl",
            "workspace",
            "close",
            &id_str,
            "--compose-down",
            "--remove-worktree",
            "--force",
        ])
        .expect("parse workspace close with worktree flags");
        assert!(matches!(
            ws_close.command,
            Commands::Workspace {
                action: WorkspaceAction::Close {
                    id: parsed_id,
                    compose_down: true,
                    remove_worktree: true,
                    force: true,
                }
            } if parsed_id == id
        ));

        for (raw, expected) in [
            ("idle", AgentState::Idle),
            ("thinking", AgentState::Thinking),
            ("waiting", AgentState::WaitingForApproval),
            ("approval", AgentState::WaitingForApproval),
            ("waiting_for_approval", AgentState::WaitingForApproval),
            ("error", AgentState::Error),
        ] {
            let parsed =
                Cli::try_parse_from(["daedalus-ctl", "agent-state", "set", raw, "--pane", &id_str])
                    .unwrap_or_else(|e| panic!("parse agent-state set {raw}: {e}"));
            match parsed.command {
                Commands::AgentState {
                    action:
                        AgentStateAction::Set {
                            state,
                            pane,
                            workspace,
                            dir,
                        },
                } => {
                    assert_eq!(state, expected);
                    assert_eq!(pane, Some(id));
                    assert!(workspace.is_none());
                    assert!(dir.is_none());
                }
                other => panic!("unexpected command: {other:?}"),
            }
        }

        assert!(
            Cli::try_parse_from(["daedalus-ctl", "agent-state", "set", "bogus"]).is_err(),
            "invalid agent state should fail CLI parsing"
        );
        assert!(
            Cli::try_parse_from([
                "daedalus-ctl",
                "agent-state",
                "set",
                "idle",
                "--pane",
                &id_str,
                "--workspace",
                &id_str,
            ])
            .is_err(),
            "--pane and --workspace should conflict"
        );

        let default_target = resolve_target_root_dir(false, None).expect("resolve default cwd");
        assert_eq!(default_target, Some(std::env::current_dir().unwrap()));
        assert_eq!(
            resolve_target_root_dir(true, None).expect("direct id skips root_dir"),
            None
        );

        let ft_json = Cli::try_parse_from(["daedalus-ctl", "failure-trace", &id_str, "--json"])
            .expect("parse failure-trace --json");
        assert!(matches!(
            ft_json.command,
            Commands::FailureTrace {
                pane_id,
                json: true
            } if pane_id == id
        ));

        let ft_plain = Cli::try_parse_from(["daedalus-ctl", "failure-trace", &id_str])
            .expect("parse failure-trace without --json");
        assert!(matches!(
            ft_plain.command,
            Commands::FailureTrace {
                pane_id,
                json: false
            } if pane_id == id
        ));

        let pub_preset = Cli::try_parse_from([
            "daedalus-ctl",
            "status",
            "publish",
            "--preset",
            "cassandra",
            "--label",
            "retrying",
        ])
        .expect("parse status publish --preset");
        match pub_preset.command {
            Commands::Status {
                action: StatusAction::Publish { preset, source, .. },
            } => {
                assert_eq!(preset, Some(StatusPreset::Cassandra));
                assert!(source.is_none());
                assert_eq!(
                    resolve_status_source(preset, source).expect("preset resolves source"),
                    "cassandra"
                );
            }
            other => panic!("unexpected command: {other:?}"),
        }

        assert!(
            Cli::try_parse_from(["daedalus-ctl", "status", "publish", "--label", "missing"])
                .is_err(),
            "status publish without --source or --preset should fail"
        );

        let clear_pos = Cli::try_parse_from([
            "daedalus-ctl",
            "status",
            "clear",
            "phrourion",
            "--dir",
            "/repo",
        ])
        .expect("parse status clear positional");
        match clear_pos.command {
            Commands::Status {
                action:
                    StatusAction::Clear {
                        source,
                        preset,
                        workspace,
                        dir,
                    },
            } => {
                assert_eq!(
                    resolve_status_source(preset, source).expect("positional source"),
                    "phrourion"
                );
                assert!(workspace.is_none());
                assert_eq!(dir, Some(PathBuf::from("/repo")));
            }
            other => panic!("unexpected command: {other:?}"),
        }

        let clear_preset = Cli::try_parse_from([
            "daedalus-ctl",
            "status",
            "clear",
            "--preset",
            "phrourion",
            "--workspace",
            &id_str,
        ])
        .expect("parse status clear --preset");
        match clear_preset.command {
            Commands::Status {
                action:
                    StatusAction::Clear {
                        source,
                        preset,
                        workspace,
                        dir,
                    },
            } => {
                assert_eq!(
                    resolve_status_source(preset, source).expect("preset resolves clear source"),
                    "phrourion"
                );
                assert_eq!(workspace, Some(id));
                assert!(dir.is_none());
            }
            other => panic!("unexpected command: {other:?}"),
        }

        assert!(
            Cli::try_parse_from(["daedalus-ctl", "status", "clear"]).is_err(),
            "status clear without source or --preset should fail"
        );

        assert!(resolve_status_source(None, None).is_err());
        assert!(resolve_status_source(None, Some("   ".to_string())).is_err());
        assert_eq!(
            resolve_status_source(Some(StatusPreset::Phrourion), Some("custom".to_string()))
                .unwrap(),
            "custom"
        );
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
        let expected = daedalus_proto::strip_windows_verbatim_prefix(
            std::fs::canonicalize(&cwd).unwrap_or(cwd),
        );
        assert_eq!(resolved, expected);
    }

    #[test]
    fn resolve_dir_sends_rooted_daemon_paths_unchanged() {
        for raw in ["/home/dev/project", "/tmp/does-not-exist-here"] {
            assert_eq!(
                resolve_dir(Some(PathBuf::from(raw))).unwrap(),
                PathBuf::from(raw)
            );
        }
    }

    #[test]
    fn parses_service_print_launchd_cli_flags() {
        let default_cmd = Cli::try_parse_from(["daedalus-ctl", "service", "print-launchd"])
            .expect("parse service print-launchd");
        assert!(default_cmd.socket.is_none());
        assert!(matches!(
            default_cmd.command,
            Commands::Service {
                action: ServiceAction::PrintLaunchd {
                    binary: None,
                    label: None
                }
            }
        ));

        let custom_cmd = Cli::try_parse_from([
            "daedalus-ctl",
            "service",
            "print-launchd",
            "--socket",
            "/tmp/custom.sock",
            "--binary",
            "/opt/homebrew/bin/daedalusd",
            "--label",
            "com.example.daedalus",
        ])
        .expect("parse service print-launchd with flags");
        assert_eq!(custom_cmd.socket, Some(PathBuf::from("/tmp/custom.sock")));
        match custom_cmd.command {
            Commands::Service {
                action: ServiceAction::PrintLaunchd { binary, label },
            } => {
                assert_eq!(binary, Some(PathBuf::from("/opt/homebrew/bin/daedalusd")));
                assert_eq!(label.as_deref(), Some("com.example.daedalus"));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn resolve_daedalusd_binary_prefers_explicit_then_sibling_then_default() {
        let dir = tempfile::tempdir().unwrap();
        let ctl_exe = dir.path().join("daedalus-ctl");
        let sibling_daemon = dir.path().join("daedalusd");

        assert_eq!(
            resolve_daedalusd_binary(None, Some(&ctl_exe)),
            PathBuf::from(DEFAULT_DAEDALUSD_BIN)
        );

        std::fs::write(&sibling_daemon, b"").unwrap();
        assert_eq!(
            resolve_daedalusd_binary(None, Some(&ctl_exe)),
            sibling_daemon
        );

        let explicit = PathBuf::from("/custom/bin/daedalusd");
        assert_eq!(
            resolve_daedalusd_binary(Some(explicit.clone()), Some(&ctl_exe)),
            explicit
        );
    }

    #[test]
    fn render_launchd_plist_outputs_valid_structure_and_escapes_xml() {
        let plist = render_launchd_plist(
            DEFAULT_LAUNCHD_LABEL,
            Path::new("/opt/homebrew/bin/daedalusd"),
            Path::new("/tmp/daedalus-501/daedalusd.sock"),
            Path::new("/Users/tim/Library/Logs"),
        );

        assert!(plist.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
        assert!(plist.contains("<plist version=\"1.0\">"));
        assert!(plist.contains("<key>Label</key>\n  <string>dev.aranea.daedalusd</string>"));
        assert!(plist.contains("<string>/opt/homebrew/bin/daedalusd</string>"));
        assert!(plist.contains("<string>--socket</string>"));
        assert!(plist.contains("<string>/tmp/daedalus-501/daedalusd.sock</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>\n  <true/>"));
        assert!(plist.contains("<key>KeepAlive</key>\n  <true/>"));
        assert!(
            plist.contains(
                "<key>StandardOutPath</key>\n  <string>/Users/tim/Library/Logs/daedalusd.out.log</string>"
            )
        );
        assert!(
            plist.contains(
                "<key>StandardErrorPath</key>\n  <string>/Users/tim/Library/Logs/daedalusd.err.log</string>"
            )
        );
        assert!(plist.trim_end().ends_with("</plist>"));

        let escaped = render_launchd_plist(
            "dev.test&<\"'>",
            Path::new("/tmp/bin&<\"'>/daedalusd"),
            Path::new("/tmp/sock&<\"'>.sock"),
            Path::new("/tmp/log&<\"'>"),
        );
        assert!(escaped.contains("<string>dev.test&amp;&lt;&quot;&apos;&gt;</string>"));
        assert!(escaped.contains("<string>/tmp/bin&amp;&lt;&quot;&apos;&gt;/daedalusd</string>"));
        assert!(escaped.contains("<string>/tmp/sock&amp;&lt;&quot;&apos;&gt;.sock</string>"));
        assert!(
            escaped
                .contains("<string>/tmp/log&amp;&lt;&quot;&apos;&gt;/daedalusd.out.log</string>")
        );
        assert!(
            escaped
                .contains("<string>/tmp/log&amp;&lt;&quot;&apos;&gt;/daedalusd.err.log</string>")
        );
    }

    #[test]
    fn formats_recent_project_line_in_utc() {
        let project = daedalus_proto::RecentProject {
            name: "demo".into(),
            root_dir: PathBuf::from("/work/demo"),
            last_used: 0,
            has_setup: false,
        };
        assert_eq!(
            format_recent_project(&project),
            "demo\t/work/demo\t1970-01-01 00:00"
        );
        assert_eq!(format_utc_minute(951_782_400 + 3_661), "2000-02-29 01:01");
    }

    #[test]
    fn parses_v0_0_6_auth_token_and_action_subcommands() {
        let id = Uuid::new_v4();
        let id_str = id.to_string();

        let ping = Cli::try_parse_from([
            "daedalus-ctl",
            "--auth-token",
            "secret-token",
            "--socket",
            "ssh://devbox:2222/tmp/daedalusd.sock",
            "ping",
        ])
        .expect("parse global --auth-token and --socket");
        assert_eq!(ping.auth_token.as_deref(), Some("secret-token"));
        assert_eq!(
            ping.socket,
            Some(PathBuf::from("ssh://devbox:2222/tmp/daedalusd.sock"))
        );
        assert!(matches!(ping.command, Commands::Ping));

        let list_json = Cli::try_parse_from([
            "daedalus-ctl",
            "action",
            "list",
            "--workspace",
            &id_str,
            "--json",
            "--auth-token",
            "trailing-global-token",
        ])
        .expect("parse action list --workspace --json");
        assert_eq!(
            list_json.auth_token.as_deref(),
            Some("trailing-global-token")
        );
        assert!(matches!(
            list_json.command,
            Commands::Action {
                action: CustomActionCommand::List {
                    workspace: Some(parsed_id),
                    dir: None,
                    json: true,
                }
            } if parsed_id == id
        ));

        let run_dir = Cli::try_parse_from([
            "daedalus-ctl",
            "action",
            "run",
            "test-all",
            "--dir",
            "/repo",
        ])
        .expect("parse action run --dir");
        match run_dir.command {
            Commands::Action {
                action:
                    CustomActionCommand::Run {
                        action_id,
                        workspace,
                        dir,
                    },
            } => {
                assert_eq!(action_id, "test-all");
                assert!(workspace.is_none());
                assert_eq!(dir, Some(PathBuf::from("/repo")));
            }
            other => panic!("unexpected command: {other:?}"),
        }

        assert!(
            Cli::try_parse_from([
                "daedalus-ctl",
                "action",
                "list",
                "--workspace",
                &id_str,
                "--dir",
                "/repo",
            ])
            .is_err(),
            "--workspace and --dir should conflict on action list"
        );
        assert!(
            Cli::try_parse_from([
                "daedalus-ctl",
                "action",
                "run",
                "test-all",
                "--workspace",
                &id_str,
                "--dir",
                "/repo",
            ])
            .is_err(),
            "--workspace and --dir should conflict on action run"
        );
    }
}
