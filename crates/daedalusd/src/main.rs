use clap::Parser;
use daedalus_proto::{default_runtime_dir, default_socket_path};
use daedalusd::server::DaemonServer;
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

    let server = DaemonServer::bind(socket, runtime_dir).await?;
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
