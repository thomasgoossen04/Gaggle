//! Pulling a whole share from a request/response source, verifying every piece
//! against the manifest as it arrives.
//!
//! The transport is abstracted to a single async call: [`fetch_share`] takes any
//! `Request -> Result<Response>` function, so it works for any transport the
//! [`Node`](crate::node::Node) can resolve. The multi-source counterpart —
//! rarest-first swarming across several peers — is
//! [`fetch_share_from_swarm`](crate::fetch_share_from_swarm).

use std::collections::BTreeMap;
use std::future::Future;

use anyhow::Context;
use gaggle_core::{ChunkList, ChunkStore, Hash, Manifest};

use crate::proto::{Request, Response};

/// A share pulled in full by [`fetch_share`].
#[derive(Debug, Clone)]
pub struct DownloadedShare {
    pub manifest: Manifest,
    /// Chunk lists keyed by the manifest's relative file path. Each is verified
    /// against its [`FileEntry::root`](gaggle_core::FileEntry::root).
    pub chunk_lists: BTreeMap<String, ChunkList>,
}

/// Fetch and verify just the metadata of the share reachable through `request` —
/// the manifest and every file's chunk list — without pulling any chunk data.
/// The relay accelerator uses this to learn a share before it starts caching.
pub async fn fetch_manifest_and_lists<F, Fut>(
    mut request: F,
) -> anyhow::Result<(Manifest, BTreeMap<String, ChunkList>)>
where
    F: FnMut(Request) -> Fut,
    Fut: Future<Output = anyhow::Result<Response>>,
{
    let manifest = match request(Request::GetManifest).await? {
        Response::Manifest(m) => m,
        other => anyhow::bail!("asked for the manifest, got {}", other.kind()),
    };
    manifest.validate().context("peer sent an invalid manifest")?;

    let mut chunk_lists = BTreeMap::new();
    for file in &manifest.files {
        let list = match request(Request::GetChunkList(file.root)).await? {
            Response::ChunkList(list) => list,
            Response::NotFound => anyhow::bail!("peer has no chunk list for {}", file.path),
            other => {
                anyhow::bail!("asked for the chunk list of {}, got {}", file.path, other.kind())
            }
        };
        list.verify(&file.root, file.size)
            .with_context(|| format!("chunk list for {} failed verification", file.path))?;
        chunk_lists.insert(file.path.clone(), list);
    }
    Ok((manifest, chunk_lists))
}

/// Pull the share reachable through `request` into `store`, checking every piece
/// against the manifest. Chunks already present in `store` are not re-fetched,
/// so a partially-filled `store` (a resumed download, an on-disk replica) just
/// tops itself up.
pub async fn fetch_share<F, Fut, S>(
    mut request: F,
    store: &mut S,
) -> anyhow::Result<DownloadedShare>
where
    F: FnMut(Request) -> Fut,
    Fut: Future<Output = anyhow::Result<Response>>,
    S: ChunkStore + ?Sized,
{
    let (manifest, chunk_lists) = fetch_manifest_and_lists(&mut request).await?;

    for (path, list) in &chunk_lists {
        for chunk in &list.chunks {
            if store.contains(&chunk.hash) {
                continue;
            }
            let data = match request(Request::GetChunk(chunk.hash)).await? {
                Response::Chunk(data) => data,
                Response::NotFound => {
                    anyhow::bail!("peer is missing chunk {} of {path}", chunk.hash)
                }
                other => anyhow::bail!("asked for a chunk of {path}, got {}", other.kind()),
            };
            let got = Hash::of(&data);
            if got != chunk.hash {
                anyhow::bail!("{path}: a chunk hashed to {got} but the list expects {}", chunk.hash);
            }
            if data.len() as u64 != u64::from(chunk.len) {
                anyhow::bail!(
                    "{path}: chunk {} is {} bytes, the list expects {}",
                    chunk.hash,
                    data.len(),
                    chunk.len
                );
            }
            store.put(chunk.hash, data);
        }
    }

    Ok(DownloadedShare { manifest, chunk_lists })
}
