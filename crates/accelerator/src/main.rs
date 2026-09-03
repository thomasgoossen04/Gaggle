//! Headless accelerator daemon. Runs as either a bandwidth-heavy relay node
//! with a hot-chunk cache or a storage-heavy cache/NAS replica
//! node.
//!
//! ```text
//! # relay + Kademlia bootstrap, caching a share pulled from an origin
//! accelerator --role relay --upstream /ip4/1.2.3.4/udp/4001/quic-v1/p2p/<id> --cache-mib 512
//!
//! # durable full replica of a share, kept on disk and re-served
//! accelerator --role nas --dir ./replica --source /ip4/1.2.3.4/udp/4001/quic-v1/p2p/<id>
//! ```

use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, ValueEnum};
use gaggle_core::ChunkStore;
use net::{Catalog, DiskChunkStore, Invite, Multiaddr, Node, RelayConfig, RelayNode, SharePublicKey};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Role {
    /// High-bandwidth hot-chunk cache + NAT relay / rendezvous point.
    Relay,
    /// Durable full replica of a designated folder, LAN-priority serving.
    Nas,
}

#[derive(Debug, Parser)]
#[command(name = "accelerator", about = "P2P folder-share accelerator daemon")]
struct Cli {
    /// Which accelerator role this node runs as.
    #[arg(long, value_enum)]
    role: Role,

    /// relay: seed(s) to pull cache misses (and the share's metadata) from.
    /// Repeatable. Without any, the relay just relays + bootstraps.
    #[arg(long = "upstream", value_name = "MULTIADDR")]
    upstreams: Vec<String>,

    /// relay: hot-chunk cache budget, in MiB.
    #[arg(long, default_value_t = 256)]
    cache_mib: u64,

    /// nas: directory holding the durable chunk store.
    #[arg(long)]
    dir: Option<PathBuf>,

    /// nas: peer(s) to replicate the share from. Repeatable, at least one.
    #[arg(long = "source", value_name = "MULTIADDR")]
    sources: Vec<String>,

    /// nas: also materialize the real folder tree here once replicated.
    #[arg(long)]
    materialize: Option<PathBuf>,

    /// nas: an invite token (`gaggle1…`) for a private share — presented to
    /// every `--source`, and then required of anyone pulling from this replica.
    #[arg(long)]
    invite: Option<String>,

    /// relay: hex of the share's public key — makes the relay serve only
    /// connections that present a valid invite for that share.
    #[arg(long, value_name = "HEX")]
    restrict: Option<String>,
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

    match cli.role {
        Role::Relay => run_relay(cli).await,
        Role::Nas => run_nas(cli).await,
    }
}

async fn run_relay(cli: Cli) -> anyhow::Result<()> {
    let relay = RelayNode::spawn_with(RelayConfig {
        cache_capacity_bytes: cli.cache_mib * 1024 * 1024,
    })
    .await?;
    tracing::info!(peer_id = %relay.peer_id(), "relay node up");
    for addr in relay.listen_addrs().await? {
        tracing::info!(%addr, "listening");
    }

    if cli.upstreams.is_empty() {
        tracing::info!("no --upstream given; running as a plain relay + bootstrap node");
    } else {
        // Learn the share's metadata from an upstream, then register it for
        // caching with every upstream as a fill source.
        let meta_node = Node::spawn().await?;
        let mut upstream_ids = Vec::new();
        for addr in &cli.upstreams {
            let addr: Multiaddr = addr.parse().with_context(|| format!("bad --upstream {addr}"))?;
            relay.add_upstream(addr.clone()).await?;
            upstream_ids.push(meta_node.connect(addr).await?);
        }

        let mut meta = None;
        for &peer in &upstream_ids {
            match meta_node.fetch_share_meta(peer).await {
                Ok(m) => {
                    meta = Some(m);
                    break;
                }
                Err(e) => tracing::warn!(%peer, error = %e, "could not get share metadata"),
            }
        }
        let (manifest, chunk_lists) =
            meta.context("no upstream returned the share metadata")?;
        tracing::info!(
            share = %manifest.id(),
            files = manifest.files.len(),
            "caching share from {} upstream(s)",
            upstream_ids.len()
        );
        relay
            .cache_share(manifest, chunk_lists.into_values(), upstream_ids)
            .await?;
        meta_node.shutdown().await;

        if let Some(hex) = &cli.restrict {
            let share = SharePublicKey::from_hex(hex.trim())
                .with_context(|| format!("bad --restrict key {hex}"))?;
            relay.restrict_to_invite_holders(share).await?;
            tracing::info!(%share, "relay restricted to invite holders");
        }
    }

    tracing::info!("ready — bootstrap peers against the address above; Ctrl-C to stop");
    relay.run_until_ctrl_c().await
}

async fn run_nas(cli: Cli) -> anyhow::Result<()> {
    let dir = cli.dir.context("--dir is required for the nas role")?;
    anyhow::ensure!(!cli.sources.is_empty(), "the nas role needs at least one --source");

    let mut disk = DiskChunkStore::open(&dir).with_context(|| format!("opening {}", dir.display()))?;
    tracing::info!(dir = %dir.display(), have_chunks = disk.len(), "opened durable chunk store");

    let invite = cli
        .invite
        .as_deref()
        .map(|t| Invite::parse(t.trim()).context("parsing --invite"))
        .transpose()?;

    let node = Node::spawn().await?;
    let mut sources = Vec::new();
    for addr in &cli.sources {
        let addr: Multiaddr = addr.parse().with_context(|| format!("bad --source {addr}"))?;
        sources.push(node.connect(addr).await?);
    }

    if let Some(invite) = &invite {
        node.authenticate_all(&sources, &invite.credential).await?;
        tracing::info!(share = %invite.share, "presented invite to every source");
    }

    tracing::info!("replicating share from {} source(s)…", sources.len());
    let pulled = node.download_share_multi(&sources, &mut disk).await?;
    let manifest = pulled.share.manifest.clone();
    let chunk_lists = pulled.share.chunk_lists.clone();
    tracing::info!(
        share = %manifest.id(),
        files = manifest.files.len(),
        chunks = disk.len(),
        bytes = manifest.total_size(),
        "replica complete"
    );

    if let Some(out) = &cli.materialize {
        gaggle_core::write_share(out, &manifest, &chunk_lists, &disk)
            .with_context(|| format!("materializing into {}", out.display()))?;
        tracing::info!(path = %out.display(), "materialized the folder tree");
    }

    node.serve(Catalog::new(manifest, chunk_lists, disk)).await?;
    if let Some(invite) = &invite {
        node.restrict_to_invite_holders(invite.share).await?;
        tracing::info!("replica is invite-only");
    }
    tracing::info!(peer_id = %node.peer_id(), "serving the replica — Ctrl-C to stop");
    for addr in node.listen_addrs().await? {
        tracing::info!(%addr, "listening");
    }
    tokio::signal::ctrl_c().await?;
    node.shutdown().await;
    Ok(())
}
