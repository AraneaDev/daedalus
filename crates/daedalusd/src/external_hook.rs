use crate::registry::Registry;
use daedalus_proto::{StatusLevel, WorkspaceId};
use serde::Deserialize;
use std::future::Future;
use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;
use tokio::task::JoinSet;
use tracing::{debug, error, info, warn};

const MAX_LINE_BYTES: usize = 65_536;

fn default_info_level() -> StatusLevel {
    StatusLevel::Info
}

#[derive(Debug, Deserialize)]
struct ExternalStatusIngest {
    workspace_id: Option<WorkspaceId>,
    root_dir: Option<PathBuf>,
    source: String,
    #[serde(default = "default_info_level")]
    level: StatusLevel,
    label: String,
    detail: Option<String>,
}

/// NDJSON Unix domain socket listener that ingests external status updates (e.g. from
/// `phrourion` or `cassandra`) into [`Registry`].
pub struct ExternalHookServer {
    socket_path: PathBuf,
    listener: UnixListener,
    registry: Arc<Registry>,
}

impl ExternalHookServer {
    /// Binds an NDJSON Unix socket at `socket_path`, cleaning up any stale socket file first.
    pub async fn bind(socket_path: PathBuf, registry: Arc<Registry>) -> std::io::Result<Self> {
        if let Some(parent) = socket_path.parent().filter(|p| !p.as_os_str().is_empty()) {
            let created = !parent.exists();
            std::fs::create_dir_all(parent)?;
            if created {
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
            }
        }

        if socket_path.exists() {
            match UnixStream::connect(&socket_path).await {
                Ok(_) => {
                    return Err(std::io::Error::new(
                        ErrorKind::AddrInUse,
                        format!(
                            "external events socket is already active at {}",
                            socket_path.display()
                        ),
                    ));
                }
                Err(_) => {
                    let _ = std::fs::remove_file(&socket_path);
                }
            }
        }

        let listener = UnixListener::bind(&socket_path)?;
        info!(socket = %socket_path.display(), "external status hook listening");

        Ok(Self {
            socket_path,
            listener,
            registry,
        })
    }

    /// Returns the filesystem path of the bound Unix socket.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Serves incoming NDJSON status connections until `shutdown` resolves, then removes the socket.
    pub async fn serve(self, shutdown: impl Future<Output = ()>) -> std::io::Result<()> {
        tokio::pin!(shutdown);
        let (stop_tx, stop_rx) = watch::channel(false);
        let mut connections = JoinSet::new();

        loop {
            tokio::select! {
                accept_res = self.listener.accept() => {
                    match accept_res {
                        Ok((stream, _)) => {
                            let reg = Arc::clone(&self.registry);
                            let stop = stop_rx.clone();
                            connections.spawn(handle_hook_client(stream, reg, stop));
                        }
                        Err(e) => {
                            error!(error = %e, "external hook accept failed");
                        }
                    }
                    while connections.try_join_next().is_some() {}
                }
                _ = &mut shutdown => {
                    debug!(socket = %self.socket_path.display(), "shutting down external status hook");
                    break;
                }
            }
        }

        let _ = stop_tx.send(true);
        connections.abort_all();
        while connections.join_next().await.is_some() {}
        let _ = std::fs::remove_file(&self.socket_path);
        Ok(())
    }
}

async fn handle_hook_client(
    stream: UnixStream,
    registry: Arc<Registry>,
    mut stop: watch::Receiver<bool>,
) {
    let mut reader = BufReader::new(stream);
    let mut line_buf = Vec::new();
    let mut oversized = false;

    loop {
        if *stop.borrow() {
            break;
        }

        let available = tokio::select! {
            _ = wait_for_stop(&mut stop) => break,
            res = reader.fill_buf() => match res {
                Ok(buf) => buf,
                Err(e) => {
                    debug!(error = %e, "external hook read failed");
                    break;
                }
            }
        };

        if available.is_empty() {
            if !oversized && !line_buf.is_empty() {
                process_line(&line_buf, &registry).await;
            }
            break;
        }

        if let Some(nl_pos) = available.iter().position(|&b| b == b'\n') {
            if !oversized {
                if line_buf.len() + nl_pos <= MAX_LINE_BYTES {
                    line_buf.extend_from_slice(&available[..nl_pos]);
                    reader.consume(nl_pos + 1);
                    process_line(&line_buf, &registry).await;
                } else {
                    reader.consume(nl_pos + 1);
                    warn!(
                        max_bytes = MAX_LINE_BYTES,
                        "external hook line exceeded maximum length; discarding"
                    );
                }
            } else {
                reader.consume(nl_pos + 1);
                oversized = false;
            }
            line_buf.clear();
        } else {
            let chunk_len = available.len();
            if !oversized {
                if line_buf.len() + chunk_len > MAX_LINE_BYTES {
                    line_buf.clear();
                    oversized = true;
                    warn!(
                        max_bytes = MAX_LINE_BYTES,
                        "external hook line exceeded maximum length; discarding"
                    );
                } else {
                    line_buf.extend_from_slice(available);
                }
            }
            reader.consume(chunk_len);
        }
    }
}

async fn wait_for_stop(stop: &mut watch::Receiver<bool>) {
    while !*stop.borrow() {
        if stop.changed().await.is_err() {
            break;
        }
    }
}

async fn process_line(raw: &[u8], registry: &Registry) {
    let trimmed = std::str::from_utf8(raw)
        .ok()
        .map(|s| s.trim_matches(|c: char| c.is_ascii_whitespace()));
    let Some(line) = trimmed else {
        warn!("ignoring non-UTF-8 external status line");
        return;
    };
    if line.is_empty() {
        return;
    }

    let ingest: ExternalStatusIngest = match serde_json::from_str(line) {
        Ok(parsed) => parsed,
        Err(e) => {
            warn!(error = %e, "ignoring invalid external status JSON line");
            return;
        }
    };

    if let Err(e) = registry
        .publish_external_status(
            ingest.workspace_id,
            ingest.root_dir,
            ingest.source,
            ingest.level,
            ingest.label,
            ingest.detail,
        )
        .await
    {
        warn!(error = %e, "failed to publish external status");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell_integration::ShellIntegration;
    use crate::supervisor::{IsolationMode, Supervisor};
    use daedalus_proto::ServerEvent;
    use std::collections::HashMap;
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio::io::AsyncWriteExt;
    use tokio::sync::{broadcast, oneshot};

    fn test_registry(runtime_dir: &Path) -> Arc<Registry> {
        let supervisor = Supervisor::with_mode(IsolationMode::PgidFallback);
        let shell_integration = ShellIntegration::prepare(runtime_dir).unwrap();
        let (event_tx, _) = broadcast::channel(64);
        Arc::new(Registry::new(supervisor, shell_integration, event_tx))
    }

    #[tokio::test]
    async fn cleans_stale_socket_rejects_active_and_removes_on_shutdown() {
        let dir = TempDir::new().unwrap();
        let reg = test_registry(dir.path());
        let sock = dir.path().join("nested").join("events.sock");

        // Stale file (not listening) should be removed and rebound
        std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
        std::fs::write(&sock, b"stale").unwrap();

        let hook = ExternalHookServer::bind(sock.clone(), Arc::clone(&reg))
            .await
            .unwrap();
        assert_eq!(hook.socket_path(), sock.as_path());

        // Active listener must reject a second bind with AddrInUse
        let err = ExternalHookServer::bind(sock.clone(), Arc::clone(&reg))
            .await
            .err()
            .expect("second bind should fail");
        assert_eq!(err.kind(), ErrorKind::AddrInUse);

        let (tx, rx) = oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            hook.serve(async {
                let _ = rx.await;
            })
            .await
            .unwrap();
        });

        let _ = tx.send(());
        handle.await.unwrap();
        assert!(!sock.exists(), "events socket should be removed on shutdown");
    }

    #[tokio::test]
    async fn ingests_valid_ndjson_and_discards_invalid_and_oversized_lines() {
        let dir = TempDir::new().unwrap();
        let reg = test_registry(dir.path());
        let ws_dir = dir.path().join("app");
        std::fs::create_dir_all(&ws_dir).unwrap();

        let ws = reg
            .create_workspace("app".to_string(), ws_dir.clone(), HashMap::new())
            .await
            .unwrap();
        let mut events = reg.subscribe_events();

        let sock = dir.path().join("events.sock");
        let hook = ExternalHookServer::bind(sock.clone(), Arc::clone(&reg))
            .await
            .unwrap();
        let (tx, rx) = oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            hook.serve(async {
                let _ = rx.await;
            })
            .await
            .unwrap();
        });

        let mut client = UnixStream::connect(&sock).await.unwrap();
        // 1. Invalid JSON line
        client.write_all(b"{not json\n").await.unwrap();
        // 2. Oversized line (> 64 KiB)
        let mut huge = vec![b'a'; MAX_LINE_BYTES + 1024];
        huge.push(b'\n');
        client.write_all(&huge).await.unwrap();
        // 3. Valid line omitting level (defaults to info)
        let valid_1 = format!(
            "{{\"root_dir\":\"{}\",\"source\":\"phrourion\",\"label\":\"clean\"}}\n",
            ws_dir.display()
        );
        client.write_all(valid_1.as_bytes()).await.unwrap();
        // 4. Valid line with explicit level and no trailing newline until EOF
        let valid_2 = format!(
            "{{\"workspace_id\":\"{}\",\"source\":\"cassandra\",\"level\":\"error\",\"label\":\"retry 2/5\",\"detail\":\"timeout\"}}",
            ws.id
        );
        client.write_all(valid_2.as_bytes()).await.unwrap();
        client.shutdown().await.unwrap();
        drop(client);

        let mut received = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), async {
            while received.len() < 2 {
                if let Ok(ServerEvent::ExternalStatusUpdated {
                    workspace_id,
                    status,
                }) = events.recv().await
                {
                    assert_eq!(workspace_id, ws.id);
                    received.push(status);
                }
            }
        })
        .await
        .expect("timed out waiting for external status events");

        assert_eq!(received[0].source, "phrourion");
        assert_eq!(received[0].level, StatusLevel::Info);
        assert_eq!(received[0].label, "clean");
        assert_eq!(received[1].source, "cassandra");
        assert_eq!(received[1].level, StatusLevel::Error);
        assert_eq!(received[1].label, "retry 2/5");
        assert_eq!(received[1].detail.as_deref(), Some("timeout"));

        let _ = tx.send(());
        handle.await.unwrap();
    }
}
