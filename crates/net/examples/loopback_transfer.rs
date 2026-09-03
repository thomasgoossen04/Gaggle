//! Milestone 2 as two real processes on one machine.
//!
//! ```text
//! # terminal 1 — serve a folder, prints a dialable multiaddr
//! cargo run -p net --example loopback_transfer -- serve ./some/folder
//!
//! # terminal 2 — pull it back over loopback QUIC and write it to disk
//! cargo run -p net --example loopback_transfer -- fetch <multiaddr> ./out
//! ```
//!
//! `serve` runs until Ctrl-C. `fetch` reconstructs every file from the
//! downloaded chunks, verifying each against the manifest root.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use gaggle_core::{ChunkStore, MemoryChunkStore, snapshot_dir};
use net::{Catalog, Client, ServerHandle, download_share};

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("serve") if args.len() == 2 => serve(Path::new(&args[1])).await,
        Some("fetch") if args.len() == 3 => {
            fetch(&args[1], Path::new(&args[2])).await
        }
        _ => {
            eprintln!(
                "usage:\n  loopback_transfer serve <dir>\n  loopback_transfer fetch <multiaddr> <out-dir>"
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

async fn serve(dir: &Path) -> anyhow::Result<()> {
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

    let catalog = Catalog::new(snapshot.manifest, snapshot.chunk_lists, store);
    let server = ServerHandle::spawn(catalog).await?;
    println!("serving on {}", server.listen_addr);
    println!("press Ctrl-C to stop");

    tokio::signal::ctrl_c().await?;
    server.shutdown().await;
    Ok(())
}

async fn fetch(addr: &str, out: &Path) -> anyhow::Result<()> {
    let client = Client::connect(addr.parse()?).await?;
    let mut store = MemoryChunkStore::new();
    let share = download_share(&client, &mut store).await?;
    client.shutdown().await;

    write_out(&share.chunk_lists, &store, out)?;
    println!(
        "downloaded {} files ({} bytes) into {}",
        share.manifest.files.len(),
        share.manifest.total_size(),
        out.display(),
    );
    Ok(())
}

fn write_out(
    chunk_lists: &BTreeMap<String, gaggle_core::ChunkList>,
    store: &MemoryChunkStore,
    out: &Path,
) -> anyhow::Result<()> {
    for (rel, list) in chunk_lists {
        let path: PathBuf = out.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut bytes = Vec::with_capacity(list.total_size as usize);
        for chunk in &list.chunks {
            let data = store
                .get(&chunk.hash)
                .ok_or_else(|| anyhow::anyhow!("missing chunk {} for {rel}", chunk.hash))?;
            bytes.extend_from_slice(&data);
        }
        std::fs::write(&path, &bytes)?;
    }
    Ok(())
}
