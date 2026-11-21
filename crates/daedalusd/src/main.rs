use clap::Parser;
use daedalus_proto::{
    default_events_socket_path, default_runtime_dir, default_socket_path, default_state_file_path,
};
use daedalusd::server::DaemonServer;
use daedalusd::supervisor::Supervisor;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "daedalusd",
    about = "Daedalus persistent headless runtime daemon"
)]
struct Args {
    #[arg(long, env = "DAEDALUS_SOCKET")]
    socket: Option<PathBuf>,
    #[arg(long, env = "DAEDALUS_RUNTIME_DIR")]
    runtime_dir: Option<PathBuf>,
    #[arg(long, env = "DAEDALUS_EVENTS_SOCKET")]
    events_socket: Option<PathBuf>,
    #[arg(long, env = "DAEDALUS_STATE_FILE")]
    state_file: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    let socket = args.socket.unwrap_or_else(default_socket_path);
    let runtime_dir = args.runtime_dir.unwrap_or_else(default_runtime_dir);
    let events_socket = args
        .events_socket
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(default_events_socket_path);
    let state_file = args
        .state_file
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(default_state_file_path);

    let server = DaemonServer::bind_with_options(
        socket,
        runtime_dir,
        Some(events_socket),
        Some(state_file),
        Supervisor::detect(),
    )
    .await?;
    server
        .serve(async {
            let ctrl_c = tokio::signal::ctrl_c();
            #[cfg(unix)]
            {
                use tokio::signal::unix::{SignalKind, signal};
                let mut sigterm =
                    signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");
                tokio::select! {
                    _ = ctrl_c => {}
                    _ = sigterm.recv() => {}
                }
            }
            #[cfg(not(unix))]
            {
                let _ = ctrl_c.await;
            }
        })
        .await?;

    Ok(())
}
