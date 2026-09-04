//! Per-share start-up helpers shared by the `accelerator` daemon and
//! `app-state`'s in-process accelerator, so "add this share to the relay" and
//! "replicate this share onto disk" are written once.

use std::path::Path;

use anyhow::Context;
use gaggle_core::{ChunkStore, DiskChunkStore, Hash};

use crate::{Catalog, Keypair, Node, RelayNode, ShareLink};

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

/// Replicate the linked share into `dir_root/<manifest-id>` on disk and start a
/// [`Node`] serving it (its own persistent `identity`, so the replica keeps a
/// stable peer id). Returns the serving node, the share's metadata and the
/// chunk count now on disk.
pub async fn nas_add_share(
    dir_root: &Path,
    identity: Keypair,
    link: &ShareLink,
) -> anyhow::Result<(Node, ShareMeta, usize)> {
    anyhow::ensure!(!link.sources.is_empty(), "share link names no sources");

    let scratch = Node::spawn().await?;
    let peers = scratch.connect_all(&link.sources).await?;
    if let Some(cred) = link.credential() {
        scratch.authenticate_all(&peers, cred).await?;
    }

    let (manifest, _) = scratch
        .fetch_share_meta(peers[0], Some(link.manifest_id))
        .await
        .context("learning the share metadata")?;
    let manifest_id = manifest.id();

    let dir = dir_root.join(manifest_id.to_hex());
    let open_dir = dir.clone();
    let mut disk = tokio::task::spawn_blocking(move || DiskChunkStore::open(&open_dir))
        .await?
        .with_context(|| format!("opening {}", dir.display()))?;

    let pulled = scratch.download_share_multi(&peers, &mut disk).await?;
    scratch.shutdown().await;

    let manifest = pulled.share.manifest.clone();
    let chunk_lists = pulled.share.chunk_lists.clone();
    let chunks = disk.len();
    let info = ShareMeta {
        manifest_id,
        name: manifest.name.clone(),
        files: manifest.files.len(),
        total_bytes: manifest.total_size(),
        version: manifest.version,
        private: link.invite.is_some(),
    };

    let node = Node::spawn_serving_with_identity(
        Catalog::new(manifest, chunk_lists, disk),
        identity,
    )
    .await?;
    if let Some(invite) = &link.invite {
        node.restrict_to_invite_holders(invite.share).await?;
    }
    Ok((node, info, chunks))
}
