pub mod codec;
pub mod rpc;
pub mod transport;
pub mod types;

pub use codec::{CodecError, MAX_FRAME_SIZE, read_frame, write_frame};
pub use rpc::{ClientMessage, ProtoError, Request, ResponsePayload, ServerEvent, ServerMessage};
pub use types::{
    AgentState, Cell, CellFlags, Color, ContainerHealth, CursorState, ExternalStatus, FailureTrace,
    GitWorkspaceStatus, GitWorktreeInfo, GridDelta, GridRow, GridSnapshot, PaneId, PaneRole,
    PaneStatus, PaneSummary, PortMapping, PortSource, SearchMatch, StatusLevel, WorkspaceId,
    WorkspaceInfo, WorkspaceManifest, wrap_bracketed_paste,
};

use std::path::{Path, PathBuf};

const MAX_UNIX_SOCKET_PATH_BYTES: usize = 96;

#[cfg(any(not(unix), test))]
fn sanitize_user_tag(raw: Option<&str>) -> String {
    let Some(raw) = raw else {
        return "user".to_string();
    };
    let sanitized: String = raw
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "user".to_string()
    } else {
        sanitized
    }
}

#[cfg(unix)]
fn current_user_tag() -> String {
    nix::unistd::Uid::current().as_raw().to_string()
}

#[cfg(not(unix))]
fn current_user_tag() -> String {
    sanitize_user_tag(std::env::var("USERNAME").ok().as_deref())
}

fn resolve_runtime_dir_with_limit(
    xdg: Option<&str>,
    temp_dir: &Path,
    user_tag: impl std::fmt::Display,
    max_socket_bytes: usize,
) -> PathBuf {
    if let Some(xdg) = xdg
        && !xdg.is_empty()
    {
        return PathBuf::from(xdg).join("daedalus");
    }
    let dir_name = format!("daedalus-{user_tag}");
    let candidate = temp_dir.join(&dir_name);
    if candidate.join("daedalusd.sock").as_os_str().len() > max_socket_bytes {
        PathBuf::from("/tmp").join(dir_name)
    } else {
        candidate
    }
}

fn resolve_runtime_dir(xdg: Option<&str>) -> PathBuf {
    resolve_runtime_dir_with_limit(
        xdg,
        &std::env::temp_dir(),
        current_user_tag(),
        MAX_UNIX_SOCKET_PATH_BYTES,
    )
}

pub fn default_runtime_dir() -> PathBuf {
    resolve_runtime_dir(std::env::var("XDG_RUNTIME_DIR").ok().as_deref())
}

pub fn default_socket_path() -> PathBuf {
    #[cfg(windows)]
    {
        resolve_windows_socket_path(
            std::env::var("DAEDALUS_SOCKET").ok().as_deref(),
            &current_user_tag(),
            "daedalusd",
        )
    }
    #[cfg(not(windows))]
    {
        if let Ok(sock) = std::env::var("DAEDALUS_SOCKET")
            && !sock.is_empty()
        {
            return PathBuf::from(sock);
        }
        default_runtime_dir().join("daedalusd.sock")
    }
}

#[cfg(any(unix, test))]
fn resolve_events_socket_path(env_override: Option<&str>, xdg: Option<&str>) -> PathBuf {
    if let Some(sock) = env_override
        && !sock.is_empty()
    {
        return PathBuf::from(sock);
    }
    resolve_runtime_dir(xdg).join("events.sock")
}

#[cfg(any(windows, test))]
fn resolve_windows_socket_path(
    env_override: Option<&str>,
    user_tag: &str,
    pipe_suffix: &str,
) -> PathBuf {
    if let Some(sock) = env_override
        && !sock.is_empty()
    {
        return PathBuf::from(sock);
    }
    PathBuf::from(format!(r"\\.\pipe\daedalus-{user_tag}-{pipe_suffix}"))
}

pub fn default_events_socket_path() -> PathBuf {
    #[cfg(windows)]
    {
        resolve_windows_socket_path(
            std::env::var("DAEDALUS_EVENTS_SOCKET").ok().as_deref(),
            &current_user_tag(),
            "events",
        )
    }
    #[cfg(not(windows))]
    {
        resolve_events_socket_path(
            std::env::var("DAEDALUS_EVENTS_SOCKET").ok().as_deref(),
            std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
        )
    }
}

fn resolve_user_file_path(
    env_override: Option<&str>,
    xdg_dir: Option<&str>,
    home: Option<&str>,
    home_rel: &[&str],
    filename: &str,
) -> PathBuf {
    if let Some(path) = env_override
        && !path.is_empty()
    {
        return PathBuf::from(path);
    }
    if let Some(xdg) = xdg_dir
        && !xdg.is_empty()
    {
        return PathBuf::from(xdg).join("daedalus").join(filename);
    }
    if let Some(home_dir) = home
        && !home_dir.is_empty()
    {
        let mut path = PathBuf::from(home_dir);
        for segment in home_rel {
            path.push(segment);
        }
        return path.join("daedalus").join(filename);
    }
    std::env::temp_dir()
        .join(format!("daedalus-{}", current_user_tag()))
        .join(filename)
}

fn resolve_state_file_path(
    env_override: Option<&str>,
    xdg_state_home: Option<&str>,
    home: Option<&str>,
) -> PathBuf {
    resolve_user_file_path(
        env_override,
        xdg_state_home,
        home,
        &[".local", "state"],
        "state.json",
    )
}

#[cfg(any(windows, test))]
fn resolve_windows_state_file_path(
    env_override: Option<&str>,
    local_app_data: Option<&str>,
    user_profile: Option<&str>,
) -> PathBuf {
    resolve_state_file_path(env_override, local_app_data, user_profile)
}

pub fn default_state_file_path() -> PathBuf {
    #[cfg(windows)]
    {
        resolve_windows_state_file_path(
            std::env::var("DAEDALUS_STATE_FILE").ok().as_deref(),
            std::env::var("LOCALAPPDATA").ok().as_deref(),
            std::env::var("USERPROFILE").ok().as_deref(),
        )
    }
    #[cfg(not(windows))]
    {
        resolve_state_file_path(
            std::env::var("DAEDALUS_STATE_FILE").ok().as_deref(),
            std::env::var("XDG_STATE_HOME").ok().as_deref(),
            std::env::var("HOME").ok().as_deref(),
        )
    }
}

fn resolve_config_file_path(
    env_override: Option<&str>,
    xdg_config_home: Option<&str>,
    home: Option<&str>,
) -> PathBuf {
    resolve_user_file_path(
        env_override,
        xdg_config_home,
        home,
        &[".config"],
        "config.toml",
    )
}

#[cfg(any(windows, test))]
fn resolve_windows_config_file_path(
    env_override: Option<&str>,
    app_data: Option<&str>,
    user_profile: Option<&str>,
) -> PathBuf {
    resolve_config_file_path(env_override, app_data, user_profile)
}

pub fn default_config_file_path() -> PathBuf {
    #[cfg(windows)]
    {
        resolve_windows_config_file_path(
            std::env::var("DAEDALUS_CONFIG").ok().as_deref(),
            std::env::var("APPDATA").ok().as_deref(),
            std::env::var("USERPROFILE").ok().as_deref(),
        )
    }
    #[cfg(not(windows))]
    {
        resolve_config_file_path(
            std::env::var("DAEDALUS_CONFIG").ok().as_deref(),
            std::env::var("XDG_CONFIG_HOME").ok().as_deref(),
            std::env::var("HOME").ok().as_deref(),
        )
    }
}

/// Strips Windows verbatim path prefixes (`\\?\UNC\` -> `\\` and `\\?\` -> ``) produced by
/// `std::fs::canonicalize`, leaving non-verbatim and Unix paths unchanged.
pub fn strip_windows_verbatim_prefix(path: PathBuf) -> PathBuf {
    if let Some(raw) = path.to_str() {
        if let Some(unc) = raw.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{unc}"));
        }
        if let Some(stripped) = raw.strip_prefix(r"\\?\") {
            return PathBuf::from(stripped);
        }
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tokio::io::duplex;

    #[tokio::test]
    async fn round_trips_requests_responses_and_events() {
        let (mut client, mut server) = duplex(8192);
        let ws_id = WorkspaceId::new();
        let pane_id = PaneId::new();

        let req = Request {
            id: 7,
            payload: ClientMessage::SpawnPane {
                workspace_id: ws_id,
                role: PaneRole::Agent,
                command: Some(vec!["agy".to_string()]),
                shell: None,
                cols: 120,
                rows: 40,
                env: HashMap::from([("FOO".to_string(), "bar".to_string())]),
            },
        };
        write_frame(&mut client, &req).await.unwrap();
        let decoded: Request = read_frame(&mut server).await.unwrap().unwrap();
        assert_eq!(decoded, req);

        let evt = ServerMessage::Event(ServerEvent::CommandFinished {
            pane_id,
            command: Some("cargo test".to_string()),
            exit_code: 101,
            has_failure_trace: true,
        });
        write_frame(&mut server, &evt).await.unwrap();
        let decoded_evt: ServerMessage = read_frame(&mut client).await.unwrap().unwrap();
        assert_eq!(decoded_evt, evt);
    }

    #[tokio::test]
    async fn rejects_oversized_frame_header() {
        let (mut tx, mut rx) = duplex(64);
        use tokio::io::AsyncWriteExt;
        let bad_len = ((MAX_FRAME_SIZE + 1) as u32).to_be_bytes();
        tx.write_all(&bad_len).await.unwrap();
        tx.flush().await.unwrap();

        let res: Result<Option<Request>, CodecError> = read_frame(&mut rx).await;
        assert!(matches!(res, Err(CodecError::FrameTooLarge(_))));
    }

    #[tokio::test]
    async fn returns_none_on_clean_eof() {
        let (tx, mut rx) = duplex(64);
        drop(tx);
        let res: Result<Option<Request>, CodecError> = read_frame(&mut rx).await;
        assert!(matches!(res, Ok(None)));
    }

    #[test]
    fn wraps_bracketed_paste() {
        let wrapped = wrap_bracketed_paste(b"line1\nline2");
        assert_eq!(wrapped, b"\x1b[200~line1\nline2\x1b[201~");
    }

    #[test]
    fn strips_embedded_and_nested_bracketed_paste_breakout_sequences() {
        // Embedded 7-bit end marker
        assert_eq!(
            wrap_bracketed_paste(b"safe\x1b[201~rm -rf /"),
            b"\x1b[200~saferm -rf /\x1b[201~"
        );
        // Embedded 7-bit start marker
        assert_eq!(
            wrap_bracketed_paste(b"a\x1b[200~b"),
            b"\x1b[200~ab\x1b[201~"
        );
        // Embedded 8-bit CSI markers
        assert_eq!(
            wrap_bracketed_paste(b"a\x9b201~b\x9b200~c"),
            b"\x1b[200~abc\x1b[201~"
        );
        // Nested breakout attempt that would reconstruct \x1b[201~ after a single pass
        assert_eq!(
            wrap_bracketed_paste(b"before\x1b[\x1b[201~201~after"),
            b"\x1b[200~beforeafter\x1b[201~"
        );
        // Mixed 8-bit + 7-bit nested breakout attempt
        assert_eq!(
            wrap_bracketed_paste(b"x\x9b\x1b[200~201~y"),
            b"\x1b[200~xy\x1b[201~"
        );
    }

    #[test]
    fn runtime_dir_uses_xdg_or_uid_fallback() {
        assert_eq!(
            resolve_runtime_dir(Some("/run/user/1234")),
            PathBuf::from("/run/user/1234/daedalus")
        );
        let uid = current_user_tag();
        let candidate = std::env::temp_dir().join(format!("daedalus-{uid}"));
        let expected_fallback =
            if candidate.join("daedalusd.sock").as_os_str().len() > MAX_UNIX_SOCKET_PATH_BYTES {
                PathBuf::from("/tmp").join(format!("daedalus-{uid}"))
            } else {
                candidate
            };
        assert_eq!(resolve_runtime_dir(None), expected_fallback);
        assert_eq!(resolve_runtime_dir(Some("")), expected_fallback);
    }

    #[test]
    fn runtime_dir_falls_back_to_tmp_when_temp_dir_exceeds_socket_limit() {
        let short_tmp = Path::new("/tmp");
        assert_eq!(
            resolve_runtime_dir_with_limit(None, short_tmp, 501, MAX_UNIX_SOCKET_PATH_BYTES),
            PathBuf::from("/tmp/daedalus-501")
        );

        let long_macos_tmp = Path::new(
            "/var/folders/ab/cdefghijklmnopqrstuvwxyz0123456789_very_long_per_user_temp_dir/T",
        );
        assert!(
            long_macos_tmp
                .join("daedalus-501/daedalusd.sock")
                .as_os_str()
                .len()
                > MAX_UNIX_SOCKET_PATH_BYTES
        );
        assert_eq!(
            resolve_runtime_dir_with_limit(None, long_macos_tmp, 501, MAX_UNIX_SOCKET_PATH_BYTES),
            PathBuf::from("/tmp/daedalus-501")
        );

        // XDG_RUNTIME_DIR takes precedence regardless of temp_dir length.
        assert_eq!(
            resolve_runtime_dir_with_limit(
                Some("/run/user/501"),
                long_macos_tmp,
                501,
                MAX_UNIX_SOCKET_PATH_BYTES,
            ),
            PathBuf::from("/run/user/501/daedalus")
        );

        // Exact boundary check: 96 bytes stays, 97 bytes falls back to /tmp.
        let suffix_len = "daedalus-501/daedalusd.sock".len();
        let exact_96_dir = PathBuf::from(format!(
            "/{}",
            "a".repeat(MAX_UNIX_SOCKET_PATH_BYTES - suffix_len - 2)
        ));
        assert_eq!(
            exact_96_dir
                .join("daedalus-501/daedalusd.sock")
                .as_os_str()
                .len(),
            MAX_UNIX_SOCKET_PATH_BYTES
        );
        assert_eq!(
            resolve_runtime_dir_with_limit(None, &exact_96_dir, 501, MAX_UNIX_SOCKET_PATH_BYTES),
            exact_96_dir.join("daedalus-501")
        );

        let over_97_dir = PathBuf::from(format!(
            "/{}",
            "a".repeat(MAX_UNIX_SOCKET_PATH_BYTES - suffix_len - 1)
        ));
        assert_eq!(
            over_97_dir
                .join("daedalus-501/daedalusd.sock")
                .as_os_str()
                .len(),
            MAX_UNIX_SOCKET_PATH_BYTES + 1
        );
        assert_eq!(
            resolve_runtime_dir_with_limit(None, &over_97_dir, 501, MAX_UNIX_SOCKET_PATH_BYTES),
            PathBuf::from("/tmp/daedalus-501")
        );
    }

    #[test]
    fn events_socket_path_uses_override_or_runtime_dir() {
        assert_eq!(
            resolve_events_socket_path(Some("/tmp/custom-events.sock"), Some("/run/user/1000")),
            PathBuf::from("/tmp/custom-events.sock")
        );
        assert_eq!(
            resolve_events_socket_path(Some(""), Some("/run/user/1000")),
            PathBuf::from("/run/user/1000/daedalus/events.sock")
        );
        assert_eq!(
            resolve_events_socket_path(None, Some("/run/user/1000")),
            PathBuf::from("/run/user/1000/daedalus/events.sock")
        );
    }

    #[tokio::test]
    async fn round_trips_phase3_types_rpcs_and_events() {
        let (mut client, mut server) = duplex(16384);
        let ws_id = WorkspaceId::new();

        let requests = vec![
            Request {
                id: 1,
                payload: ClientMessage::DiscoverWorktrees {
                    repo_dir: PathBuf::from("/home/tim/daedalus"),
                },
            },
            Request {
                id: 2,
                payload: ClientMessage::CreateWorktreeWorkspace {
                    repo_dir: PathBuf::from("/home/tim/daedalus"),
                    branch: "feat/auth".to_string(),
                    create_branch: true,
                    env: HashMap::from([("RUST_LOG".to_string(), "debug".to_string())]),
                },
            },
            Request {
                id: 3,
                payload: ClientMessage::CloseWorkspace {
                    id: ws_id,
                    compose_down: true,
                    remove_worktree: false,
                    force_remove_worktree: false,
                },
            },
            Request {
                id: 4,
                payload: ClientMessage::PublishExternalStatus {
                    workspace_id: Some(ws_id),
                    root_dir: Some(PathBuf::from("/home/tim/daedalus")),
                    source: "phrourion".to_string(),
                    level: StatusLevel::Warn,
                    label: "3 dirty".to_string(),
                    detail: Some("2 modified, 1 untracked".to_string()),
                },
            },
        ];

        for req in requests {
            write_frame(&mut client, &req).await.unwrap();
            let decoded: Request = read_frame(&mut server).await.unwrap().unwrap();
            assert_eq!(decoded, req);
        }

        let worktrees = vec![
            GitWorktreeInfo {
                path: PathBuf::from("/home/tim/daedalus"),
                branch: Some("master".to_string()),
                head: "15f8202deadbeef".to_string(),
                is_primary: true,
                is_detached: false,
            },
            GitWorktreeInfo {
                path: PathBuf::from("/home/tim/.local/share/daedalus/worktrees/daedalus/feat-auth"),
                branch: None,
                head: "abcdef123456".to_string(),
                is_primary: false,
                is_detached: true,
            },
        ];

        let port_mapping = PortMapping {
            host_port: 5532,
            container_port: Some(5432),
            service: Some("postgres".to_string()),
            source: PortSource::Docker,
        };
        let local_port = PortMapping {
            host_port: 3100,
            container_port: None,
            service: None,
            source: PortSource::LocalProcess,
        };
        let container = ContainerHealth {
            name: "daedalus-wt1-postgres-1".to_string(),
            service: Some("postgres".to_string()),
            state: "running".to_string(),
            health: Some("healthy".to_string()),
            ports: vec![5532],
        };
        let ext_status = ExternalStatus {
            source: "cassandra".to_string(),
            level: StatusLevel::Error,
            label: "retry 3/5".to_string(),
            detail: Some("rate limited".to_string()),
            updated_at_ms: 1_760_000_000_123,
        };

        let ws_info = WorkspaceInfo {
            id: ws_id,
            name: "feat-auth".to_string(),
            root_dir: PathBuf::from("/tmp/wt1"),
            slot_index: 1,
            injected_env: HashMap::from([("PORT".to_string(), "3100".to_string())]),
            panes: Vec::new(),
            port_offset: 100,
            compose_project_name: Some("daedalus-wt1".to_string()),
            git_branch: Some("feat/auth".to_string()),
            git_status: None,
            bound_ports: vec![port_mapping.clone(), local_port.clone()],
            containers: vec![container.clone()],
            external_statuses: vec![ext_status.clone()],
            manifest: None,
        };

        let server_msgs = vec![
            ServerMessage::Response {
                id: 1,
                result: Ok(ResponsePayload::Worktrees(worktrees)),
            },
            ServerMessage::Response {
                id: 2,
                result: Ok(ResponsePayload::WorkspaceCreated(ws_info)),
            },
            ServerMessage::Event(ServerEvent::WorkspaceRuntimeUpdated {
                workspace_id: ws_id,
                bound_ports: vec![port_mapping, local_port],
                containers: vec![container],
                git_status: None,
                git_branch: None,
            }),
            ServerMessage::Event(ServerEvent::ExternalStatusUpdated {
                workspace_id: ws_id,
                status: ext_status,
            }),
        ];

        for msg in server_msgs {
            write_frame(&mut server, &msg).await.unwrap();
            let decoded: ServerMessage = read_frame(&mut client).await.unwrap().unwrap();
            assert_eq!(decoded, msg);
        }
    }

    #[test]
    fn serde_defaults_preserve_backward_compatibility_for_workspace_info_and_close_workspace() {
        #[derive(serde::Serialize)]
        struct LegacyWorkspaceInfo {
            id: WorkspaceId,
            name: String,
            root_dir: PathBuf,
            slot_index: u16,
            injected_env: HashMap<String, String>,
            panes: Vec<PaneSummary>,
        }

        #[derive(serde::Serialize)]
        enum LegacyClientMessage {
            CloseWorkspace { id: WorkspaceId },
        }

        let ws_id = WorkspaceId::new();
        let legacy_ws = LegacyWorkspaceInfo {
            id: ws_id,
            name: "legacy".to_string(),
            root_dir: PathBuf::from("/tmp/legacy"),
            slot_index: 0,
            injected_env: HashMap::new(),
            panes: Vec::new(),
        };
        let bytes = rmp_serde::to_vec_named(&legacy_ws).unwrap();
        let decoded_ws: WorkspaceInfo = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(decoded_ws.id, ws_id);
        assert_eq!(decoded_ws.port_offset, 0);
        assert_eq!(decoded_ws.compose_project_name, None);
        assert_eq!(decoded_ws.git_branch, None);
        assert!(decoded_ws.bound_ports.is_empty());
        assert!(decoded_ws.containers.is_empty());
        assert!(decoded_ws.external_statuses.is_empty());
        assert_eq!(decoded_ws.manifest, None);

        let legacy_close = LegacyClientMessage::CloseWorkspace { id: ws_id };
        let close_bytes = rmp_serde::to_vec_named(&legacy_close).unwrap();
        let decoded_close: ClientMessage = rmp_serde::from_slice(&close_bytes).unwrap();
        assert_eq!(
            decoded_close,
            ClientMessage::CloseWorkspace {
                id: ws_id,
                compose_down: false,
                remove_worktree: false,
                force_remove_worktree: false,
            }
        );
    }

    #[tokio::test]
    async fn round_trips_manifest_and_search_rpcs() {
        let (mut client, mut server) = duplex(8192);
        let ws_id = WorkspaceId::new();
        let pane_id = PaneId::new();

        let search_req = Request {
            id: 42,
            payload: ClientMessage::SearchPane {
                pane_id,
                query: "needle".to_string(),
                case_sensitive: false,
            },
        };
        write_frame(&mut client, &search_req).await.unwrap();
        let decoded_req: Request = read_frame(&mut server).await.unwrap().unwrap();
        assert_eq!(decoded_req, search_req);

        let search_resp = ServerMessage::Response {
            id: 42,
            result: Ok(ResponsePayload::SearchMatches {
                pane_id,
                total_scrollback: 120,
                matches: vec![
                    SearchMatch {
                        abs_row: 5,
                        start_col: 2,
                        end_col: 8,
                    },
                    SearchMatch {
                        abs_row: 125,
                        start_col: 0,
                        end_col: 6,
                    },
                ],
            }),
        };
        write_frame(&mut server, &search_resp).await.unwrap();
        let decoded_resp: ServerMessage = read_frame(&mut client).await.unwrap().unwrap();
        assert_eq!(decoded_resp, search_resp);

        let manifest = WorkspaceManifest {
            agent_preset: Some("agy".to_string()),
            agent_command: Some(vec!["agy".to_string(), "--fast".to_string()]),
            service_command: Some(vec!["docker".to_string(), "compose".to_string()]),
            runner_command: Some(vec!["cargo".to_string(), "test".to_string()]),
        };
        let ws_info = WorkspaceInfo {
            id: ws_id,
            name: "manifest-ws".to_string(),
            root_dir: PathBuf::from("/tmp/manifest-ws"),
            slot_index: 0,
            injected_env: HashMap::new(),
            panes: Vec::new(),
            port_offset: 0,
            compose_project_name: None,
            git_branch: None,
            git_status: None,
            bound_ports: Vec::new(),
            containers: Vec::new(),
            external_statuses: Vec::new(),
            manifest: Some(manifest),
        };
        let ws_resp = ServerMessage::Response {
            id: 43,
            result: Ok(ResponsePayload::WorkspaceCreated(ws_info)),
        };
        write_frame(&mut server, &ws_resp).await.unwrap();
        let decoded_ws_resp: ServerMessage = read_frame(&mut client).await.unwrap().unwrap();
        assert_eq!(decoded_ws_resp, ws_resp);
    }

    #[test]
    fn resolves_state_and_config_file_paths() {
        assert_eq!(
            resolve_state_file_path(
                Some("/tmp/custom-state.json"),
                Some("/xdg/state"),
                Some("/home/alice"),
            ),
            PathBuf::from("/tmp/custom-state.json")
        );
        assert_eq!(
            resolve_state_file_path(Some(""), Some("/xdg/state"), Some("/home/alice")),
            PathBuf::from("/xdg/state/daedalus/state.json")
        );
        assert_eq!(
            resolve_state_file_path(None, Some(""), Some("/home/alice")),
            PathBuf::from("/home/alice/.local/state/daedalus/state.json")
        );

        assert_eq!(
            resolve_config_file_path(
                Some("/tmp/custom-config.toml"),
                Some("/xdg/config"),
                Some("/home/alice"),
            ),
            PathBuf::from("/tmp/custom-config.toml")
        );
        assert_eq!(
            resolve_config_file_path(Some(""), Some("/xdg/config"), Some("/home/alice")),
            PathBuf::from("/xdg/config/daedalus/config.toml")
        );
        assert_eq!(
            resolve_config_file_path(None, None, Some("/home/alice")),
            PathBuf::from("/home/alice/.config/daedalus/config.toml")
        );

        let fallback_prefix = std::env::temp_dir().join(format!("daedalus-{}", current_user_tag()));
        assert_eq!(
            resolve_state_file_path(None, None, None),
            fallback_prefix.join("state.json")
        );
        assert_eq!(
            resolve_config_file_path(None, None, Some("")),
            fallback_prefix.join("config.toml")
        );
    }

    #[test]
    fn sanitizes_windows_user_tags() {
        assert_eq!(sanitize_user_tag(Some("Alice")), "Alice");
        assert_eq!(sanitize_user_tag(Some("Alice Smith")), "Alice_Smith");
        assert_eq!(sanitize_user_tag(Some("tim-1_2")), "tim-1_2");
        assert_eq!(sanitize_user_tag(Some("  ")), "user");
        assert_eq!(sanitize_user_tag(None), "user");
    }

    #[test]
    fn resolves_windows_socket_and_user_paths() {
        assert_eq!(
            resolve_windows_socket_path(None, "Alice", "daedalusd"),
            PathBuf::from(r"\\.\pipe\daedalus-Alice-daedalusd")
        );
        assert_eq!(
            resolve_windows_socket_path(Some(""), "Alice", "events"),
            PathBuf::from(r"\\.\pipe\daedalus-Alice-events")
        );
        assert_eq!(
            resolve_windows_socket_path(Some("tcp://127.0.0.1:7777"), "Alice", "daedalusd"),
            PathBuf::from("tcp://127.0.0.1:7777")
        );
        assert_eq!(
            resolve_windows_socket_path(Some(r"\\.\pipe\custom"), "Alice", "events"),
            PathBuf::from(r"\\.\pipe\custom")
        );

        let app_data = r"C:\Users\Alice\AppData\Roaming";
        let local_app_data = r"C:\Users\Alice\AppData\Local";
        let user_profile = r"C:\Users\Alice";

        assert_eq!(
            resolve_windows_config_file_path(
                Some(r"D:\custom\config.toml"),
                Some(app_data),
                Some(user_profile),
            ),
            PathBuf::from(r"D:\custom\config.toml")
        );
        assert_eq!(
            resolve_windows_config_file_path(Some(""), Some(app_data), Some(user_profile)),
            PathBuf::from(app_data).join("daedalus").join("config.toml")
        );
        assert_eq!(
            resolve_windows_config_file_path(None, None, Some(user_profile)),
            PathBuf::from(user_profile)
                .join(".config")
                .join("daedalus")
                .join("config.toml")
        );

        assert_eq!(
            resolve_windows_state_file_path(
                Some(r"D:\custom\state.json"),
                Some(local_app_data),
                Some(user_profile),
            ),
            PathBuf::from(r"D:\custom\state.json")
        );
        assert_eq!(
            resolve_windows_state_file_path(Some(""), Some(local_app_data), Some(user_profile)),
            PathBuf::from(local_app_data)
                .join("daedalus")
                .join("state.json")
        );
        assert_eq!(
            resolve_windows_state_file_path(None, Some(""), Some(user_profile)),
            PathBuf::from(user_profile)
                .join(".local")
                .join("state")
                .join("daedalus")
                .join("state.json")
        );
    }

    #[test]
    fn agent_state_from_cli_name_and_display() {
        assert_eq!(AgentState::from_cli_name("idle"), Some(AgentState::Idle));
        assert_eq!(AgentState::from_cli_name("IDLE"), Some(AgentState::Idle));
        assert_eq!(
            AgentState::from_cli_name("Thinking"),
            Some(AgentState::Thinking)
        );
        assert_eq!(
            AgentState::from_cli_name("waiting"),
            Some(AgentState::WaitingForApproval)
        );
        assert_eq!(
            AgentState::from_cli_name("approval"),
            Some(AgentState::WaitingForApproval)
        );
        assert_eq!(
            AgentState::from_cli_name("waiting_for_approval"),
            Some(AgentState::WaitingForApproval)
        );
        assert_eq!(
            AgentState::from_cli_name("waiting-for-approval"),
            Some(AgentState::WaitingForApproval)
        );
        assert_eq!(
            AgentState::from_cli_name("WAITING-FOR-APPROVAL"),
            Some(AgentState::WaitingForApproval)
        );
        assert_eq!(AgentState::from_cli_name("error"), Some(AgentState::Error));
        assert_eq!(AgentState::from_cli_name("ERROR"), Some(AgentState::Error));
        assert_eq!(AgentState::from_cli_name("unknown"), None);
        assert_eq!(AgentState::from_cli_name(""), None);

        assert_eq!(AgentState::Idle.to_string(), "idle");
        assert_eq!(AgentState::Thinking.to_string(), "thinking");
        assert_eq!(
            AgentState::WaitingForApproval.to_string(),
            "waiting_for_approval"
        );
        assert_eq!(AgentState::Error.to_string(), "error");
    }

    #[tokio::test]
    async fn round_trips_v0_0_3_types_rpcs_and_events() {
        let (mut client, mut server) = duplex(16384);
        let ws_id = WorkspaceId::new();
        let pane_id = PaneId::new();

        for state in [
            AgentState::Idle,
            AgentState::Thinking,
            AgentState::WaitingForApproval,
            AgentState::Error,
        ] {
            let encoded = rmp_serde::to_vec_named(&state).unwrap();
            let decoded: AgentState = rmp_serde::from_slice(&encoded).unwrap();
            assert_eq!(decoded, state);
        }

        let git_status = GitWorkspaceStatus {
            is_worktree: true,
            dirty: true,
            ahead: 2,
            behind: 1,
        };
        let encoded_gs = rmp_serde::to_vec_named(&git_status).unwrap();
        let decoded_gs: GitWorkspaceStatus = rmp_serde::from_slice(&encoded_gs).unwrap();
        assert_eq!(decoded_gs, git_status);

        let requests = vec![
            Request {
                id: 101,
                payload: ClientMessage::CloseWorkspace {
                    id: ws_id,
                    compose_down: true,
                    remove_worktree: true,
                    force_remove_worktree: true,
                },
            },
            Request {
                id: 102,
                payload: ClientMessage::SetAgentState {
                    pane_id: Some(pane_id),
                    workspace_id: Some(ws_id),
                    root_dir: Some(PathBuf::from("/tmp/wt1")),
                    state: AgentState::WaitingForApproval,
                },
            },
            Request {
                id: 103,
                payload: ClientMessage::ClearExternalStatus {
                    workspace_id: None,
                    root_dir: Some(PathBuf::from("/tmp/wt1")),
                    source: "phrourion".to_string(),
                },
            },
        ];

        for req in requests {
            write_frame(&mut client, &req).await.unwrap();
            let decoded: Request = read_frame(&mut server).await.unwrap().unwrap();
            assert_eq!(decoded, req);
        }

        let trace = FailureTrace {
            pane_id,
            command: Some("cargo test".to_string()),
            exit_code: 101,
            cwd: Some(PathBuf::from("/tmp/wt1")),
            output: "assertion failed".to_string(),
            captured_at_ms: 1_760_000_000_999,
            git_branch: Some("feat/v0.0.3".to_string()),
            git_diff_stat: Some(" crates/daedalus-proto/src/types.rs | 10 ++++\n".to_string()),
        };

        let pane_summary = PaneSummary {
            id: pane_id,
            workspace_id: ws_id,
            role: PaneRole::Agent,
            status: PaneStatus::Busy,
            cols: 120,
            rows: 40,
            cwd: Some(PathBuf::from("/tmp/wt1")),
            last_exit_code: None,
            has_failure_trace: false,
            agent_state: Some(AgentState::Thinking),
        };

        let ws_info = WorkspaceInfo {
            id: ws_id,
            name: "wt1".to_string(),
            root_dir: PathBuf::from("/tmp/wt1"),
            slot_index: 1,
            injected_env: HashMap::new(),
            panes: vec![pane_summary.clone()],
            port_offset: 100,
            compose_project_name: None,
            git_branch: Some("feat/v0.0.3".to_string()),
            git_status: Some(git_status),
            bound_ports: Vec::new(),
            containers: Vec::new(),
            external_statuses: Vec::new(),
            manifest: None,
        };

        let server_msgs = vec![
            ServerMessage::Response {
                id: 101,
                result: Ok(ResponsePayload::FailureTrace(Some(trace))),
            },
            ServerMessage::Response {
                id: 102,
                result: Ok(ResponsePayload::PaneSpawned(pane_summary)),
            },
            ServerMessage::Response {
                id: 103,
                result: Ok(ResponsePayload::WorkspaceCreated(ws_info)),
            },
            ServerMessage::Event(ServerEvent::AgentStateChanged {
                workspace_id: ws_id,
                pane_id,
                state: AgentState::WaitingForApproval,
            }),
            ServerMessage::Event(ServerEvent::WorkspaceRuntimeUpdated {
                workspace_id: ws_id,
                bound_ports: Vec::new(),
                containers: Vec::new(),
                git_status: Some(git_status),
                git_branch: Some("feat/v0.0.3".to_string()),
            }),
        ];

        for msg in server_msgs {
            write_frame(&mut server, &msg).await.unwrap();
            let decoded: ServerMessage = read_frame(&mut client).await.unwrap().unwrap();
            assert_eq!(decoded, msg);
        }
    }

    #[test]
    fn serde_defaults_preserve_backward_compatibility_for_v0_0_3_fields() {
        #[derive(serde::Serialize)]
        struct LegacyPaneSummary {
            id: PaneId,
            workspace_id: WorkspaceId,
            role: PaneRole,
            status: PaneStatus,
            cols: u16,
            rows: u16,
            cwd: Option<PathBuf>,
            last_exit_code: Option<i32>,
            has_failure_trace: bool,
        }

        #[derive(serde::Serialize)]
        struct LegacyWorkspaceInfoV02 {
            id: WorkspaceId,
            name: String,
            root_dir: PathBuf,
            slot_index: u16,
            injected_env: HashMap<String, String>,
            panes: Vec<LegacyPaneSummary>,
            port_offset: u16,
            compose_project_name: Option<String>,
            git_branch: Option<String>,
            bound_ports: Vec<PortMapping>,
            containers: Vec<ContainerHealth>,
            external_statuses: Vec<ExternalStatus>,
            manifest: Option<WorkspaceManifest>,
        }

        #[derive(serde::Serialize)]
        struct LegacyFailureTrace {
            pane_id: PaneId,
            command: Option<String>,
            exit_code: i32,
            cwd: Option<PathBuf>,
            output: String,
            captured_at_ms: u64,
        }

        #[derive(serde::Serialize)]
        enum LegacyClientMessageV02 {
            CloseWorkspace { id: WorkspaceId, compose_down: bool },
        }

        #[derive(serde::Serialize)]
        enum LegacyServerEventV02 {
            WorkspaceRuntimeUpdated {
                workspace_id: WorkspaceId,
                bound_ports: Vec<PortMapping>,
                containers: Vec<ContainerHealth>,
            },
        }

        let ws_id = WorkspaceId::new();
        let pane_id = PaneId::new();

        let legacy_pane = LegacyPaneSummary {
            id: pane_id,
            workspace_id: ws_id,
            role: PaneRole::Agent,
            status: PaneStatus::Idle,
            cols: 80,
            rows: 24,
            cwd: None,
            last_exit_code: None,
            has_failure_trace: false,
        };
        let pane_bytes = rmp_serde::to_vec_named(&legacy_pane).unwrap();
        let decoded_pane: PaneSummary = rmp_serde::from_slice(&pane_bytes).unwrap();
        assert_eq!(decoded_pane.agent_state, None);

        let legacy_ws = LegacyWorkspaceInfoV02 {
            id: ws_id,
            name: "v0.0.2".to_string(),
            root_dir: PathBuf::from("/tmp/v02"),
            slot_index: 0,
            injected_env: HashMap::new(),
            panes: vec![legacy_pane],
            port_offset: 0,
            compose_project_name: None,
            git_branch: Some("main".to_string()),
            bound_ports: Vec::new(),
            containers: Vec::new(),
            external_statuses: Vec::new(),
            manifest: None,
        };
        let ws_bytes = rmp_serde::to_vec_named(&legacy_ws).unwrap();
        let decoded_ws: WorkspaceInfo = rmp_serde::from_slice(&ws_bytes).unwrap();
        assert_eq!(decoded_ws.git_status, None);
        assert_eq!(decoded_ws.panes[0].agent_state, None);

        let legacy_trace = LegacyFailureTrace {
            pane_id,
            command: Some("make test".to_string()),
            exit_code: 2,
            cwd: None,
            output: "failed".to_string(),
            captured_at_ms: 42,
        };
        let trace_bytes = rmp_serde::to_vec_named(&legacy_trace).unwrap();
        let decoded_trace: FailureTrace = rmp_serde::from_slice(&trace_bytes).unwrap();
        assert_eq!(decoded_trace.git_branch, None);
        assert_eq!(decoded_trace.git_diff_stat, None);

        let legacy_close = LegacyClientMessageV02::CloseWorkspace {
            id: ws_id,
            compose_down: true,
        };
        let close_bytes = rmp_serde::to_vec_named(&legacy_close).unwrap();
        let decoded_close: ClientMessage = rmp_serde::from_slice(&close_bytes).unwrap();
        assert_eq!(
            decoded_close,
            ClientMessage::CloseWorkspace {
                id: ws_id,
                compose_down: true,
                remove_worktree: false,
                force_remove_worktree: false,
            }
        );

        let legacy_runtime = LegacyServerEventV02::WorkspaceRuntimeUpdated {
            workspace_id: ws_id,
            bound_ports: Vec::new(),
            containers: Vec::new(),
        };
        let runtime_bytes = rmp_serde::to_vec_named(&legacy_runtime).unwrap();
        let decoded_runtime: ServerEvent = rmp_serde::from_slice(&runtime_bytes).unwrap();
        assert_eq!(
            decoded_runtime,
            ServerEvent::WorkspaceRuntimeUpdated {
                workspace_id: ws_id,
                bound_ports: Vec::new(),
                containers: Vec::new(),
                git_status: None,
                git_branch: None,
            }
        );
    }

    #[test]
    fn strip_windows_verbatim_prefix_normalizes_drive_and_unc_paths() {
        assert_eq!(
            strip_windows_verbatim_prefix(PathBuf::from(r"\\?\C:\Users\tim\repo")),
            PathBuf::from(r"C:\Users\tim\repo")
        );
        assert_eq!(
            strip_windows_verbatim_prefix(PathBuf::from(r"\\?\UNC\server\share\repo")),
            PathBuf::from(r"\\server\share\repo")
        );
        assert_eq!(
            strip_windows_verbatim_prefix(PathBuf::from(r"C:\Users\tim\repo")),
            PathBuf::from(r"C:\Users\tim\repo")
        );
        assert_eq!(
            strip_windows_verbatim_prefix(PathBuf::from("/home/tim/repo")),
            PathBuf::from("/home/tim/repo")
        );
    }
}
