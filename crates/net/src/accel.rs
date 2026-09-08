//! Per-share start-up helpers shared by the `accelerator` daemon and
//! `app-state`'s in-process accelerator, so "add this share to the relay" and
//! "replicate this share onto disk" are written once.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Context;
use gaggle_core::{
    ChunkList, ChunkStore, DiskChunkStore, Hash, Manifest, Scope, SharedChunkStore,
};

use crate::{Catalog, Keypair, Node, PeerId, RelayNode, ShareLink, SwarmConfig, SwarmProgress};

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
        // A replica usually pulls from a single upstream (the origin) over a
        // fast link. The default per-peer cap of 4 leaves a LAN/gigabit path
        // idle between round trips — now that landed chunks are written
        // off-thread, keep many more chunk requests in flight so the pipe stays
        // full. Peak in-flight bytes stay bounded (this × sources × chunk size).
        per_peer_parallelism: 16,
        ..SwarmConfig::default()
    }
}

/// Connect to every source `link` names through `downloader`, present the
/// invite credential if it carries one, and — when `max_bytes` is `Some(n)` —
/// fetch the share metadata and refuse (before any replica directory is
/// created) if the share is larger than `n`. Returns the distinct upstream peer
/// ids.
///
/// `max_bytes` lets a NAS never start a replication it cannot finish within its
/// storage cap; a scoped invite is measured against its full manifest size, so
/// the guard is conservative for that case.
async fn connect_auth_budget(
    downloader: &Node,
    link: &ShareLink,
    max_bytes: Option<u64>,
) -> anyhow::Result<Vec<PeerId>> {
    anyhow::ensure!(!link.sources.is_empty(), "share link names no sources");

    let peers = downloader.connect_all(&link.sources).await?;
    if let Some(cred) = link.credential() {
        downloader.authenticate_all(&peers, cred).await?;
    }

    if let Some(budget) = max_bytes {
        let (manifest, _) = fetch_meta_from_any(downloader, &peers, link.manifest_id).await?;
        let size = manifest.total_size();
        anyhow::ensure!(
            size <= budget,
            "share is {size} bytes but only {budget} bytes of the replica storage \
             budget are free — raise or clear the storage cap to replicate it"
        );
    }
    Ok(peers)
}

/// Fetch the share `want` from whichever of `peers` answers first.
async fn fetch_meta_from_any(
    downloader: &Node,
    peers: &[PeerId],
    want: Hash,
) -> anyhow::Result<(Manifest, BTreeMap<String, ChunkList>)> {
    let mut last_err = None;
    for &peer in peers {
        match downloader.fetch_share_meta(peer, Some(want)).await {
            Ok(m) => return Ok(m),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("no source returned the share metadata")))
}

/// A [`ShareMeta`] describing what *actually* lands on this replica — for a
/// scoped invite that is the granted subset, not the origin's full share (see
/// [`replica_swarm_config`]).
fn share_meta(manifest: &Manifest, chunk_lists: &BTreeMap<String, ChunkList>, link: &ShareLink) -> ShareMeta {
    ShareMeta {
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
///
/// This is the plain "pull, then serve separately" form. Prefer
/// [`nas_seed_start`] + [`nas_seed_finish`] when you want the replica to upload
/// what it already holds *while* it is still filling.
pub async fn nas_pull_with_progress<P>(
    downloader: &Node,
    dir_root: &Path,
    link: &ShareLink,
    compress: bool,
    max_bytes: Option<u64>,
    on_progress: P,
) -> anyhow::Result<(Manifest, BTreeMap<String, ChunkList>, DiskChunkStore, usize)>
where
    P: FnMut(SwarmProgress),
{
    let peers = connect_auth_budget(downloader, link, max_bytes).await?;

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
    let info = share_meta(&manifest, &chunk_lists, link);
    let node = Node::spawn_serving_with_identity_accelerator(
        Catalog::new(manifest, chunk_lists, disk),
        identity,
    )
    .await?;
    if let Some(invite) = &link.invite {
        node.restrict_to_invite_holders(invite.share).await?;
    }
    Ok((node, info, chunks))
}

/// What [`nas_seed_start`] hands back: a [`Node`] that is *already serving* the
/// share (over the still-filling replica), plus everything [`nas_seed_finish`]
/// needs to drive the replication pull.
pub struct NasSeedStart {
    /// Spawned on the persistent `identity` and already answering chunk
    /// requests for whatever is on disk so far. The caller keeps this — it
    /// becomes the permanent seed once the pull finishes.
    pub serving: Node,
    /// The replica store, shared between `serving`'s catalog and the pull. Pass
    /// it back to [`nas_seed_finish`].
    pub store: SharedChunkStore<DiskChunkStore>,
    /// Metadata for what this replica will hold (the granted subset for a
    /// scoped invite).
    pub meta: ShareMeta,
    /// Upstream peers already connected/authenticated. Pass back to
    /// [`nas_seed_finish`].
    pub peers: Vec<PeerId>,
}

/// Phase one of a seed-while-replicating NAS add: connect + authenticate +
/// storage-budget check, open the on-disk replica, learn the share, and stand a
/// serving [`Node`] up over the (still mostly empty) replica **now** — so the
/// NAS uploads the chunks it already has from the first one on, instead of only
/// once the whole share has landed. `downloader` is the caller's throw-away
/// pulling node (already NAT-punched if the caller wanted that); the serving
/// node uses the persistent `identity`.
pub async fn nas_seed_start(
    downloader: &Node,
    dir_root: &Path,
    identity: Keypair,
    link: &ShareLink,
    compress: bool,
    max_bytes: Option<u64>,
) -> anyhow::Result<NasSeedStart> {
    let peers = connect_auth_budget(downloader, link, max_bytes).await?;
    let (manifest, chunk_lists) = fetch_meta_from_any(downloader, &peers, link.manifest_id).await?;

    let dir = dir_root.join(link.manifest_id.to_hex());
    let open_dir = dir.clone();
    let disk =
        tokio::task::spawn_blocking(move || DiskChunkStore::open_with_opts(&open_dir, compress))
            .await?
            .with_context(|| format!("opening {}", dir.display()))?;
    let store = SharedChunkStore::new(disk);

    let meta = share_meta(&manifest, &chunk_lists, link);
    let serving = Node::spawn_serving_with_identity_accelerator(
        Catalog::new(manifest, chunk_lists, store.clone()),
        identity,
    )
    .await?;
    if let Some(invite) = &link.invite {
        serving.restrict_to_invite_holders(invite.share).await?;
    }

    Ok(NasSeedStart { serving, store, meta, peers })
}

/// Phase two: drive the replication pull into the store [`nas_seed_start`]
/// opened, reporting [`SwarmProgress`] once per chunk. The serving node from
/// phase one sees every chunk the moment it lands (same backing store). Returns
/// the chunk count now on disk.
pub async fn nas_seed_finish<P>(
    downloader: &Node,
    store: &SharedChunkStore<DiskChunkStore>,
    peers: &[PeerId],
    link: &ShareLink,
    on_progress: P,
) -> anyhow::Result<usize>
where
    P: FnMut(SwarmProgress),
{
    let mut store = store.clone();
    downloader
        .download_share_multi_with_progress(peers, &mut store, replica_swarm_config(link), on_progress)
        .await?;
    Ok(store.len())
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
    max_bytes: Option<u64>,
) -> anyhow::Result<(Node, ShareMeta, usize)> {
    nas_add_share_with_progress(dir_root, identity, link, compress, max_bytes, |_| {}).await
}

/// [`nas_add_share`] that also reports [`SwarmProgress`] once per chunk as it
/// lands — for driving a progress bar.
///
/// The serving node is stood up over the replica *before* the pull runs (via
/// [`nas_seed_start`] / [`nas_seed_finish`]), so the NAS is already uploading
/// the chunks it holds while it fills; it is still returned only once the pull
/// completes. A caller that wants the node handle earlier (to announce the
/// replica to a tracker mid-fill) should drive the two phases itself.
pub async fn nas_add_share_with_progress<P>(
    dir_root: &Path,
    identity: Keypair,
    link: &ShareLink,
    compress: bool,
    max_bytes: Option<u64>,
    on_progress: P,
) -> anyhow::Result<(Node, ShareMeta, usize)>
where
    P: FnMut(SwarmProgress),
{
    let scratch = Node::spawn_accelerator().await?;
    let start = match nas_seed_start(&scratch, dir_root, identity, link, compress, max_bytes).await {
        Ok(s) => s,
        Err(e) => {
            scratch.shutdown().await;
            return Err(e);
        }
    };
    let NasSeedStart { serving, store, meta, peers } = start;
    let chunks = nas_seed_finish(&scratch, &store, &peers, link, on_progress).await;
    scratch.shutdown().await;
    Ok((serving, meta, chunks?))
}
