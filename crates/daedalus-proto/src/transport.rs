use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

const WINDOWS_PIPE_PREFIX: &str = r"\\.\pipe\";
const WINDOWS_UNC_PIPE_PREFIX: &str = r"\\?\pipe\";
const MAX_WINDOWS_PIPE_NAME_LEN: usize = 256;

#[cfg(windows)]
const ERROR_PIPE_BUSY: i32 = 231;
#[cfg(windows)]
const PIPE_BUSY_MAX_RETRIES: usize = 50;
#[cfg(windows)]
const PIPE_BUSY_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(10);

/// Parsed IPC transport endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpcEndpoint {
    /// TCP socket address (e.g. `127.0.0.1:7777` parsed from `tcp://127.0.0.1:7777`),
    /// used across all platforms including WSL2 Bridge Mode.
    Tcp(String),
    /// Platform-native local IPC endpoint (Unix domain socket path on Unix,
    /// Windows named pipe or mapped path on Windows).
    Local(PathBuf),
}

/// Parses a CLI/config endpoint path into TCP (`tcp://host:port`) or local IPC.
pub fn parse_endpoint(endpoint: &Path) -> IpcEndpoint {
    if let Some(raw) = endpoint.to_str()
        && let Some(addr) = raw.strip_prefix("tcp://")
    {
        return IpcEndpoint::Tcp(addr.to_string());
    }
    IpcEndpoint::Local(endpoint.to_path_buf())
}

fn fnv1a_64(bytes: &[u8]) -> u64 {
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0100_0000_01b3;
    let mut hash = FNV_OFFSET_BASIS;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Maps any filesystem path to a valid Windows named pipe name.
///
/// Existing `\\.\pipe\...` or `\\?\pipe\...` paths are preserved verbatim.
/// Regular filesystem paths (such as `TempDir` socket paths in tests) are
/// hashed deterministically into `\\.\pipe\daedalus-<stem>-<hash>`.
pub fn windows_pipe_name_from_path(path: &Path) -> String {
    let raw = path.to_string_lossy();
    if raw.get(..WINDOWS_PIPE_PREFIX.len()).is_some_and(|prefix| {
        prefix.eq_ignore_ascii_case(WINDOWS_PIPE_PREFIX)
            || prefix.eq_ignore_ascii_case(WINDOWS_UNC_PIPE_PREFIX)
    }) {
        return raw.into_owned();
    }

    let hash = fnv1a_64(raw.as_bytes());
    let file_name = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let raw_stem = match file_name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => file_name,
    };
    let sanitized_stem: String = raw_stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(24)
        .collect();
    let stem = if sanitized_stem.is_empty() {
        "pipe"
    } else {
        &sanitized_stem
    };
    let pipe = format!(r"\\.\pipe\daedalus-{stem}-{hash:016x}");
    debug_assert!(pipe.len() <= MAX_WINDOWS_PIPE_NAME_LEN);
    pipe
}

pub type IpcReadHalf = tokio::io::ReadHalf<IpcStream>;
pub type IpcWriteHalf = tokio::io::WriteHalf<IpcStream>;

/// Cross-platform bidirectional byte stream for Daedalus IPC.
#[derive(Debug)]
pub enum IpcStream {
    #[cfg(unix)]
    Unix(tokio::net::UnixStream),
    #[cfg(windows)]
    NamedPipeServer(tokio::net::windows::named_pipe::NamedPipeServer),
    #[cfg(windows)]
    NamedPipeClient(tokio::net::windows::named_pipe::NamedPipeClient),
    Tcp(tokio::net::TcpStream),
}

impl IpcStream {
    /// Connects to a Unix socket, Windows named pipe, or `tcp://` endpoint.
    pub async fn connect(endpoint: &Path) -> std::io::Result<Self> {
        match parse_endpoint(endpoint) {
            IpcEndpoint::Tcp(addr) => {
                let stream = tokio::net::TcpStream::connect(&addr).await?;
                let _ = stream.set_nodelay(true);
                Ok(Self::Tcp(stream))
            }
            IpcEndpoint::Local(path) => Self::connect_local(&path).await,
        }
    }

    #[cfg(unix)]
    async fn connect_local(path: &Path) -> std::io::Result<Self> {
        let stream = tokio::net::UnixStream::connect(path).await?;
        Ok(Self::Unix(stream))
    }

    #[cfg(windows)]
    async fn connect_local(path: &Path) -> std::io::Result<Self> {
        let pipe_name = windows_pipe_name_from_path(path);
        let mut retries = 0usize;
        loop {
            match tokio::net::windows::named_pipe::ClientOptions::new().open(&pipe_name) {
                Ok(client) => return Ok(Self::NamedPipeClient(client)),
                Err(err)
                    if err.raw_os_error() == Some(ERROR_PIPE_BUSY)
                        && retries < PIPE_BUSY_MAX_RETRIES =>
                {
                    retries += 1;
                    tokio::time::sleep(PIPE_BUSY_RETRY_DELAY).await;
                }
                Err(err) => return Err(err),
            }
        }
    }

    /// Splits this stream into independent read and write halves.
    pub fn into_split(self) -> (IpcReadHalf, IpcWriteHalf) {
        tokio::io::split(self)
    }

    /// Returns `true` if a listener is currently accepting connections at `endpoint`.
    pub async fn is_endpoint_alive(endpoint: &Path) -> bool {
        Self::connect(endpoint).await.is_ok()
    }
}

/// Returns `true` if a listener is currently accepting connections at `endpoint`.
pub async fn is_endpoint_alive(endpoint: &Path) -> bool {
    IpcStream::is_endpoint_alive(endpoint).await
}

impl AsyncRead for IpcStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match &mut *self {
            #[cfg(unix)]
            Self::Unix(stream) => Pin::new(stream).poll_read(cx, buf),
            #[cfg(windows)]
            Self::NamedPipeServer(stream) => Pin::new(stream).poll_read(cx, buf),
            #[cfg(windows)]
            Self::NamedPipeClient(stream) => Pin::new(stream).poll_read(cx, buf),
            Self::Tcp(stream) => Pin::new(stream).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for IpcStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match &mut *self {
            #[cfg(unix)]
            Self::Unix(stream) => Pin::new(stream).poll_write(cx, buf),
            #[cfg(windows)]
            Self::NamedPipeServer(stream) => Pin::new(stream).poll_write(cx, buf),
            #[cfg(windows)]
            Self::NamedPipeClient(stream) => Pin::new(stream).poll_write(cx, buf),
            Self::Tcp(stream) => Pin::new(stream).poll_write(cx, buf),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match &mut *self {
            #[cfg(unix)]
            Self::Unix(stream) => Pin::new(stream).poll_flush(cx),
            #[cfg(windows)]
            Self::NamedPipeServer(stream) => Pin::new(stream).poll_flush(cx),
            #[cfg(windows)]
            Self::NamedPipeClient(stream) => Pin::new(stream).poll_flush(cx),
            Self::Tcp(stream) => Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match &mut *self {
            #[cfg(unix)]
            Self::Unix(stream) => Pin::new(stream).poll_shutdown(cx),
            #[cfg(windows)]
            Self::NamedPipeServer(stream) => Pin::new(stream).poll_shutdown(cx),
            #[cfg(windows)]
            Self::NamedPipeClient(stream) => Pin::new(stream).poll_shutdown(cx),
            Self::Tcp(stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }
}

#[derive(Debug)]
enum IpcListenerInner {
    #[cfg(unix)]
    Unix {
        path: PathBuf,
        listener: tokio::net::UnixListener,
    },
    #[cfg(windows)]
    NamedPipe {
        pipe_name: String,
        endpoint: PathBuf,
        server: Option<tokio::net::windows::named_pipe::NamedPipeServer>,
    },
    Tcp {
        listener: tokio::net::TcpListener,
        local_addr: std::net::SocketAddr,
    },
}

/// Cross-platform IPC listener accepting Unix sockets, Windows named pipes, or TCP.
#[derive(Debug)]
pub struct IpcListener {
    inner: IpcListenerInner,
}

impl IpcListener {
    /// Binds an IPC listener at `endpoint`.
    pub async fn bind(endpoint: &Path) -> std::io::Result<Self> {
        let inner = match parse_endpoint(endpoint) {
            IpcEndpoint::Tcp(addr) => {
                let listener = tokio::net::TcpListener::bind(&addr).await?;
                let local_addr = listener.local_addr()?;
                IpcListenerInner::Tcp {
                    listener,
                    local_addr,
                }
            }
            IpcEndpoint::Local(path) => Self::bind_local(path).await?,
        };
        Ok(Self { inner })
    }

    #[cfg(unix)]
    async fn bind_local(path: PathBuf) -> std::io::Result<IpcListenerInner> {
        use std::os::unix::fs::PermissionsExt;

        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            let created = !parent.exists();
            std::fs::create_dir_all(parent)?;
            if created {
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
            }
        }
        if path.exists() {
            match tokio::net::UnixStream::connect(&path).await {
                Ok(_) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AddrInUse,
                        format!("daedalusd is already running on {}", path.display()),
                    ));
                }
                Err(_) => {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
        let listener = tokio::net::UnixListener::bind(&path)?;
        Ok(IpcListenerInner::Unix { path, listener })
    }

    #[cfg(windows)]
    async fn bind_local(path: PathBuf) -> std::io::Result<IpcListenerInner> {
        const BIND_MAX_RETRIES: usize = 50;
        const BIND_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(20);

        let pipe_name = windows_pipe_name_from_path(&path);
        let mut retries = 0usize;
        let server = loop {
            match tokio::net::windows::named_pipe::ServerOptions::new()
                .first_pipe_instance(true)
                .create(&pipe_name)
            {
                Ok(server) => break server,
                Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                    if IpcStream::is_endpoint_alive(&path).await {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::AddrInUse,
                            format!("daedalusd is already running on {}", path.display()),
                        ));
                    }
                    if retries >= BIND_MAX_RETRIES {
                        return Err(err);
                    }
                    retries += 1;
                    tokio::time::sleep(BIND_RETRY_DELAY).await;
                }
                Err(err) => return Err(err),
            }
        };
        Ok(IpcListenerInner::NamedPipe {
            pipe_name,
            endpoint: path,
            server: Some(server),
        })
    }

    /// Accepts the next inbound client stream.
    pub async fn accept(&mut self) -> std::io::Result<IpcStream> {
        match &mut self.inner {
            #[cfg(unix)]
            IpcListenerInner::Unix { listener, .. } => {
                let (stream, _) = listener.accept().await?;
                Ok(IpcStream::Unix(stream))
            }
            #[cfg(windows)]
            IpcListenerInner::NamedPipe {
                pipe_name, server, ..
            } => {
                let current = server.as_ref().ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "named pipe listener closed",
                    )
                })?;
                current.connect().await?;
                let next = tokio::net::windows::named_pipe::ServerOptions::new()
                    .first_pipe_instance(false)
                    .create(pipe_name)?;
                let connected = server
                    .replace(next)
                    .ok_or_else(|| std::io::Error::other("named pipe listener state missing"))?;
                Ok(IpcStream::NamedPipeServer(connected))
            }
            IpcListenerInner::Tcp { listener, .. } => {
                let (stream, _) = listener.accept().await?;
                let _ = stream.set_nodelay(true);
                Ok(IpcStream::Tcp(stream))
            }
        }
    }

    /// Returns the bound endpoint path (`tcp://host:port` for TCP listeners).
    pub fn local_endpoint(&self) -> PathBuf {
        match &self.inner {
            #[cfg(unix)]
            IpcListenerInner::Unix { path, .. } => path.clone(),
            #[cfg(windows)]
            IpcListenerInner::NamedPipe { endpoint, .. } => endpoint.clone(),
            IpcListenerInner::Tcp { local_addr, .. } => {
                PathBuf::from(format!("tcp://{local_addr}"))
            }
        }
    }

    /// Removes any local socket artifact created on disk (Unix domain sockets only).
    pub fn cleanup(&self) {
        #[cfg(unix)]
        if let IpcListenerInner::Unix { path, .. } = &self.inner {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClientMessage, Request, read_frame, write_frame};
    use tempfile::TempDir;

    #[test]
    fn parses_tcp_and_local_endpoints() {
        assert_eq!(
            parse_endpoint(Path::new("tcp://127.0.0.1:7777")),
            IpcEndpoint::Tcp("127.0.0.1:7777".to_string())
        );
        assert_eq!(
            parse_endpoint(Path::new("/run/user/1000/daedalus/daedalusd.sock")),
            IpcEndpoint::Local(PathBuf::from("/run/user/1000/daedalus/daedalusd.sock"))
        );
        assert_eq!(
            parse_endpoint(Path::new(r"\\.\pipe\daedalus-alice-daedalusd")),
            IpcEndpoint::Local(PathBuf::from(r"\\.\pipe\daedalus-alice-daedalusd"))
        );
    }

    #[test]
    fn windows_pipe_name_maps_paths_deterministically_and_preserves_pipes() {
        let existing = Path::new(r"\\.\pipe\daedalus-alice-daedalusd");
        assert_eq!(
            windows_pipe_name_from_path(existing),
            r"\\.\pipe\daedalus-alice-daedalusd"
        );
        let existing_upper = Path::new(r"\\.\PIPE\custom");
        assert_eq!(
            windows_pipe_name_from_path(existing_upper),
            r"\\.\PIPE\custom"
        );

        let unc_pipe = Path::new(r"\\?\pipe\daedalus-unc");
        assert_eq!(
            windows_pipe_name_from_path(unc_pipe),
            r"\\?\pipe\daedalus-unc"
        );

        let temp_sock_a = Path::new(r"C:\Users\alice\AppData\Local\Temp\abc\daedalusd.sock");
        let temp_sock_b = Path::new(r"C:\Users\alice\AppData\Local\Temp\def\daedalusd.sock");
        let pipe_a1 = windows_pipe_name_from_path(temp_sock_a);
        let pipe_a2 = windows_pipe_name_from_path(temp_sock_a);
        let pipe_b = windows_pipe_name_from_path(temp_sock_b);

        assert_eq!(pipe_a1, pipe_a2);
        assert_ne!(pipe_a1, pipe_b);
        assert!(pipe_a1.starts_with(r"\\.\pipe\daedalus-daedalusd-"));
        assert!(pipe_a1.len() <= MAX_WINDOWS_PIPE_NAME_LEN);

        // Very long path still produces a pipe name well under 256 chars.
        let long_path = PathBuf::from(format!("/{}/daedalusd.sock", "segment/".repeat(40)));
        let long_pipe = windows_pipe_name_from_path(&long_path);
        assert!(long_pipe.starts_with(r"\\.\pipe\daedalus-daedalusd-"));
        assert!(long_pipe.len() <= MAX_WINDOWS_PIPE_NAME_LEN);
    }

    async fn assert_frame_round_trip(listener_endpoint: &Path) {
        let mut listener = IpcListener::bind(listener_endpoint).await.unwrap();
        let bound_endpoint = listener.local_endpoint();
        assert!(is_endpoint_alive(&bound_endpoint).await);

        let accept_task = tokio::spawn(async move {
            // Drain the liveness probe connection first, then accept the RPC client.
            let probe = listener.accept().await.unwrap();
            drop(probe);
            let stream = listener.accept().await.unwrap();
            let (mut reader, mut writer) = stream.into_split();
            let req: Request = read_frame(&mut reader).await.unwrap().unwrap();
            let reply = Request {
                id: req.id + 1,
                payload: req.payload,
            };
            write_frame(&mut writer, &reply).await.unwrap();
            listener.cleanup();
        });

        let client = IpcStream::connect(&bound_endpoint).await.unwrap();
        let (mut reader, mut writer) = client.into_split();
        let req = Request {
            id: 41,
            payload: ClientMessage::DiscoverWorktrees {
                repo_dir: PathBuf::from("/tmp/repo"),
            },
        };
        write_frame(&mut writer, &req).await.unwrap();
        let reply: Request = read_frame(&mut reader).await.unwrap().unwrap();
        assert_eq!(reply.id, 42);
        assert_eq!(
            reply.payload,
            ClientMessage::DiscoverWorktrees {
                repo_dir: PathBuf::from("/tmp/repo"),
            }
        );

        accept_task.await.unwrap();
    }

    #[tokio::test]
    async fn tcp_bridge_listener_and_stream_round_trip() {
        assert_frame_round_trip(Path::new("tcp://127.0.0.1:0")).await;
    }

    #[tokio::test]
    async fn local_ipc_listener_and_stream_round_trip() {
        let dir = TempDir::new().unwrap();
        let endpoint = dir.path().join("daedalusd.sock");
        assert_frame_round_trip(&endpoint).await;
    }
}
