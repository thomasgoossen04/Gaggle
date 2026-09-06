//! Per-share start-up helpers shared by the `accelerator` daemon and
//! `app-state`'s in-process accelerator, so "add this share to the relay" and
//! "replicate this share onto disk" are written once.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Context;
use gaggle_core::{ChunkList, ChunkStore, DiskChunkStore, Hash, Manifest, Scope};

use crate::{Catalog, Keypair, Node, RelayNode, ShareLink, SwarmConfig, SwarmProgress};

/// What a caller needs to show for an accelerated share.
#[derive(Debug, Clone)]
pub struct ShareMeta {
    pub manifest_id: Hash,
    pub name: String,
    pub files: usize,
    pub total_bytes: u64,
    pub version: u64,
    pub private: bool,
}

/// Point `meta` at every seed the link names, learn the share's metadata, and
/// register it with `relay` for read-through caching — gating it to invite
/// holders when the link carries one. `meta` is a throw-away downloading
/// [`Node`] the caller keeps alive across shares.
pub async fn relay_add_share(
    relay: &RelayNode,
    meta: &Node,
    link: &ShareLink,
) -> anyhow::Result<ShareMeta> {
    anyhow::ensure!(!link.sources.is_empty(), "share link names no sources");

    for addr in &link.sources {
        relay.add_upstream(addr.clone()).await?;
    }
    let upstream_ids = meta.connect_all(&link.sources).await?;
    if let Some(cred) = link.credential() {
        meta.authenticate_all(&upstream_ids, cred).await?;
    }

    let mut fetched = None;
    let mut last_err = None;
    for &peer in &upstream_ids {
        match meta.fetch_share_meta(peer, Some(link.manifest_id)).await {
            Ok(m) => {
                fetched = Some(m);
                break;
            }
            Err(e) => last_err = Some(e),
        }
    }
    let (manifest, chunk_lists) = fetched.ok_or_else(|| {
        last_err.unwrap_or_else(|| anyhow::anyhow!("no upstream returned the share metadata"))
    })?;

    let info = ShareMeta {
        manifest_id: manifest.id(),
        name: manifest.name.clone(),
        files: manifest.files.len(),
        total_bytes: manifest.total_size(),
        version: manifest.version,
        private: link.invite.is_some(),
    };
    relay.cache_share(manifest, chunk_lists.into_values(), upstream_ids).await?;
    if let Some(invite) = &link.invite {
        relay.restrict_to_invite_holders(invite.share, info.manifest_id).await?;
    }
    Ok(info)
}

/// The [`SwarmConfig`] a NAS replica pulls `link` with: pinned to the exact
/// share (never substitutes a different manifest from a multi-share source),
/// narrowed to a scoped invite's granted files when it carries one — but
/// keeping the *served* manifest full. `Manifest::id()` hashes the manifest's
/// content, so narrowing it would change the id and break every legitimate
/// invite holder's manifest-id check against this replica; the chunk lists
/// (and so the on-disk store) still only cover the granted files.
fn replica_swarm_config(link: &ShareLink) -> SwarmConfig {
    let allowed_paths = link.credential().and_then(|c| match &c.capability.scope {
        Scope::All => None,
        Scope::Files(paths) => Some(paths.clone()),
    });
    SwarmConfig {
        manifest_id: Some(link.manifest_id),
        allowed_paths,
        narrow_manifest: false,
        ..SwarmConfig::default()
    }
}

/// Pull `link`'s share into `dir_root/<manifest-id>` on disk through
/// `downloader` (already spawned by the caller — so a caller that wants to try
/// a NAT-rendezvous punch first can do it through this same node before
/// calling in, since the punch only opens a hole in *this* node's own NAT
/// mapping; ordinary callers can just use [`nas_add_share`] /
/// [`nas_add_share_with_progress`] instead). Chunks already on disk from a
/// previous run are topped up, not re-fetched. `compress` opens the replica
/// with zstd on-disk compression (see [`DiskChunkStore::open_with_opts`]).
/// Reports [`SwarmProgress`] once per chunk via `on_progress`.
pub async fn nas_pull_with_progress<P>(
    downloader: &Node,
    dir_root: &Path,
    link: &ShareLink,
    compress: bool,
    on_progress: P,
) -> anyhow::Result<(Manifest, BTreeMap<String, ChunkList>, DiskChunkStore, usize)>
where
    P: FnMut(SwarmProgress),
{
    anyhow::ensure!(!link.sources.is_empty(), "share link names no sources");

    let peers = downloader.connect_all(&link.sources).await?;
    if let Some(cred) = link.credential() {
        downloader.authenticate_all(&peers, cred).await?;
    }

    let dir = dir_root.join(link.manifest_id.to_hex());
    let open_dir = dir.clone();
    let mut disk = tokio::task::spawn_blocking(move || DiskChunkStore::open_with_opts(&open_dir, compress))
        .await?
        .with_context(|| format!("opening {}", dir.display()))?;

    let pulled = downloader
        .download_share_multi_with_progress(&peers, &mut disk, replica_swarm_config(link), on_progress)
        .await?;
    let chunks = disk.len();
    Ok((pulled.share.manifest, pulled.share.chunk_lists, disk, chunks))
}

/// Start a [`Node`] serving a pulled share on `identity` (a persistent key, so
/// the replica keeps a stable peer id across restarts), gating it to invite
/// holders when `link` carries one.
pub async fn nas_serve(
    manifest: Manifest,
    chunk_lists: BTreeMap<String, ChunkList>,
    disk: DiskChunkStore,
    chunks: usize,
    identity: Keypair,
    link: &ShareLink,
) -> anyhow::Result<(Node, ShareMeta, usize)> {
    // Reflect what actually landed on disk, not the origin's full share — for
    // a scoped invite these differ on purpose (see `replica_swarm_config`).
    let info = ShareMeta {
        manifest_id: link.manifest_id,
        name: manifest.name.clone(),
        files: chunk_lists.len(),
        total_bytes: manifest
            .files
            .iter()
            .filter(|f| chunk_lists.contains_key(&f.path))
            .map(|f| f.size)
            .sum(),
        version: manifest.version,
        private: link.invite.is_some(),
    };

    let node = Node::spawn_serving_with_identity(Catalog::new(manifest, chunk_lists, disk), identity)
        .await?;
    if let Some(invite) = &link.invite {
        node.restrict_to_invite_holders(invite.share).await?;
    }
    Ok((node, info, chunks))
}

/// Replicate the linked share into `dir_root/<manifest-id>` on disk and start a
/// [`Node`] serving it (its own persistent `identity`, so the replica keeps a
/// stable peer id). `compress` opts the on-disk replica into zstd compression.
/// Returns the serving node, the share's metadata and the chunk count now on
/// disk.
pub async fn nas_add_share(
    dir_root: &Path,
    identity: Keypair,
    link: &ShareLink,
    compress: bool,
) -> anyhow::Result<(Node, ShareMeta, usize)> {
    nas_add_share_with_progress(dir_root, identity, link, compress, |_| {}).await
}

/// [`nas_add_share`] that also reports [`SwarmProgress`] once per chunk as it
/// lands — for driving a progress bar.
pub async fn nas_add_share_with_progress<P>(
    dir_root: &Path,
    identity: Keypair,
    link: &ShareLink,
    compress: bool,
    on_progress: P,
) -> anyhow::Result<(Node, ShareMeta, usize)>
where
    P: FnMut(SwarmProgress),
{
    let scratch = Node::spawn().await?;
    let pulled = nas_pull_with_progress(&scratch, dir_root, link, compress, on_progress).await;
    scratch.shutdown().await;
    let (manifest, chunk_lists, disk, chunks) = pulled?;
    nas_serve(manifest, chunk_lists, disk, chunks, identity, link).await
}
