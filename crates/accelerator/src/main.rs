//! Headless accelerator daemon. Runs as either a bandwidth-heavy relay node or a
//! storage-heavy cache/NAS replica node.

use clap::{Parser, ValueEnum};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Role {
    /// High-bandwidth hot-chunk cache + NAT relay / rendezvous point.
    Relay,
    /// Durable full replica of designated folders, LAN-priority serving.
    Nas,
}

#[derive(Debug, Parser)]
#[command(name = "accelerator", about = "P2P folder-share accelerator daemon")]
struct Cli {
    /// Which accelerator role this node runs as.
    #[arg(long, value_enum)]
    role: Role,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    tracing::info!(?cli.role, "accelerator starting");
    tracing::info!("{}", net::describe());
    tracing::info!("{}", control_plane::describe());

    // Milestones 5 & 6 wire the actual relay / replica loops here.
    Ok(())
}
