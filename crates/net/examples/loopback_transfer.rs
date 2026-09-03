//! Milestones 2–7 as real processes on one machine.
//!
//! ```text
//! # one or more terminals — serve a folder, each prints a dialable multiaddr.
//! # add a 32-byte hex seed to make the share private (invite-only).
//! cargo run -p net --example loopback_transfer -- serve ./some/folder [seed-hex]
//!
//! # print a `gaggle1…` invite for a private share (same seed as `serve`)
//! cargo run -p net --example loopback_transfer -- mint-invite ./some/folder <seed-hex>
//!
//! # pull it back over loopback QUIC from a single source and write it to disk.
//! # pass the invite when the share is private.
//! cargo run -p net --example loopback_transfer -- fetch <multiaddr> ./out [invite]
//!
//! # or swarm it from several sources at once, rarest chunk first (public only)
//! cargo run -p net --example loopback_transfer -- fetch-swarm ./out <multiaddr> <multiaddr> ...
//! ```

use std::path::Path;
use std::process::ExitCode;

use gaggle_core::{
    Capability, Invite, MemoryChunkStore, ShareKeypair, SignedCapability, snapshot_dir, write_share,
};
use net::{Catalog, Multiaddr, Node};

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("serve") if args.len() == 2 => serve(Path::new(&args[1]), None).await,
        Some("serve") if args.len() == 3 => serve(Path::new(&args[1]), Some(&args[2])).await,
        Some("mint-invite") if args.len() == 3 => mint_invite(Path::new(&args[1]), &args[2]),
        Some("fetch") if args.len() == 3 => fetch(&args[1], Path::new(&args[2]), None).await,
        Some("fetch") if args.len() == 4 => {
            fetch(&args[1], Path::new(&args[2]), Some(&args[3])).await
        }
        Some("fetch-swarm") if args.len() >= 3 => {
            fetch_swarm(Path::new(&args[1]), &args[2..]).await
        }
        _ => {
            eprintln!(
                "usage:\n  loopback_transfer serve <dir> [seed-hex]\n  loopback_transfer mint-invite <dir> <seed-hex>\n  loopback_transfer fetch <multiaddr> <out-dir> [invite]\n  loopback_transfer fetch-swarm <out-dir> <multiaddr>..."
            );
            return ExitCode::from(2);
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Parse a 64-char hex string into a share keypair seed.
fn keypair_from_hex(hex: &str) -> anyhow::Result<ShareKeypair> {
    anyhow::ensure!(hex.len() == 64, "seed must be 64 hex chars (32 bytes)");
    let mut seed = [0u8; 32];
    for (i, b) in seed.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)?;
    }
    Ok(ShareKeypair::from_seed(seed))
}

fn mint_invite(dir: &Path, seed_hex: &str) -> anyhow::Result<()> {
    let mut store = MemoryChunkStore::new();
    let snapshot = snapshot_dir(dir, dir.display().to_string(), 1, &mut store)?;
    let keypair = keypair_from_hex(seed_hex)?;
    let cap = Capability::new(keypair.public(), snapshot.manifest.id());
    let invite = Invite::new(
        keypair.public(),
        snapshot.manifest.id(),
        &snapshot.manifest.name,
        keypair.issue(cap),
    );
    println!("share key: {}", keypair.public());
    println!("{}", invite.to_url());
    Ok(())
}

async fn serve(dir: &Path, seed_hex: Option<&str>) -> anyhow::Result<()> {
    let mut store = MemoryChunkStore::new();
    let snapshot = snapshot_dir(dir, dir.display().to_string(), 1, &mut store)?;
    let stats = store.stats();
    println!(
        "snapshot: {} files, {} chunks, {} unique bytes ({:.1}% dedup)",
        snapshot.manifest.files.len(),
        stats.unique_chunks,
        stats.unique_bytes,
        stats.dedup_ratio() * 100.0,
    );

    let keypair = seed_hex.map(keypair_from_hex).transpose()?;
    let catalog = Catalog::new(snapshot.manifest, snapshot.chunk_lists, store);
    let node = Node::spawn_serving(catalog).await?;
    if let Some(keypair) = &keypair {
        node.restrict_to_invite_holders(keypair.public()).await?;
        println!("private share — key {}", keypair.public());
    }
    println!("serving on {}", node.listen_addr().await?);
    println!("press Ctrl-C to stop");

    tokio::signal::ctrl_c().await?;
    node.shutdown().await;
    Ok(())
}

async fn fetch(addr: &str, out: &Path, invite_token: Option<&str>) -> anyhow::Result<()> {
    let addr: Multiaddr = addr.parse()?;
    let node = Node::spawn().await?;
    let peer = node.connect(addr).await?;

    if let Some(token) = invite_token {
        let invite = Invite::parse(token)?;
        let credential: SignedCapability = invite.credential;
        node.authenticate(peer, &credential).await?;
        println!("presented invite for share {}", invite.share);
    }

    let mut store = MemoryChunkStore::new();
    let share = node.download_share(peer, &mut store).await?;
    node.shutdown().await;

    write_share(out, &share.manifest, &share.chunk_lists, &store)?;
    println!(
        "downloaded {} files ({} bytes) into {}",
        share.manifest.files.len(),
        share.manifest.total_size(),
        out.display(),
    );
    Ok(())
}

async fn fetch_swarm(out: &Path, addrs: &[String]) -> anyhow::Result<()> {
    let node = Node::spawn().await?;
    let mut sources = Vec::with_capacity(addrs.len());
    for addr in addrs {
        sources.push(node.connect(addr.parse::<Multiaddr>()?).await?);
    }

    let mut store = MemoryChunkStore::new();
    let out_dl = node.download_share_multi(&sources, &mut store).await?;
    node.shutdown().await;

    write_share(out, &out_dl.share.manifest, &out_dl.share.chunk_lists, &store)?;
    let mut breakdown: Vec<_> = out_dl.chunks_per_source.iter().collect();
    breakdown.sort_by_key(|(peer, _)| peer.to_string());
    println!(
        "downloaded {} files ({} bytes) into {}",
        out_dl.share.manifest.files.len(),
        out_dl.share.manifest.total_size(),
        out.display(),
    );
    for (peer, n) in breakdown {
        println!("  {n} chunks from {peer}");
    }
    Ok(())
}
