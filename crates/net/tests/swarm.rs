//! Milestone 4: one subscriber pulls a share from several peers at once.
//!
//! Covers the three things the swarm downloader has to get right — spreading
//! load across full seeds, stitching a share together from peers that each hold
//! only part of it, and routing around a source that dies mid-download — while
//! still verifying every chunk against the manifest.

use std::collections::BTreeMap;
use std::fs;
use std::time::Duration;

use gaggle_core::{
    ChunkList, ChunkStore, Hash, Manifest, MemoryChunkStore, Snapshot, snapshot_dir,
};
use net::{Catalog, Node, catalog_from_download};
use tempfile::TempDir;
use tokio::time::timeout;

/// A share whose big file chunks into a dozen-ish pieces, so there is real work
/// to spread across sources.
fn sample_share() -> TempDir {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::create_dir(root.join("mods")).unwrap();
    fs::write(root.join("readme.txt"), b"multi-peer swarm test\n").unwrap();
    fs::write(root.join("mods/a.cfg"), b"quality=ultra\n").unwrap();

    let mut blob = Vec::with_capacity(12 * 1024 * 1024);
    let mut state = 0x2545_f491_4f6c_dd1du64;
    while blob.len() < 12 * 1024 * 1024 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        blob.extend_from_slice(&state.to_le_bytes());
    }
    fs::write(root.join("mods/pack.bin"), &blob).unwrap();
    dir
}

/// Snapshotting is deterministic, so every seed derives the same manifest and
/// chunk hashes from its own independent snapshot.
fn snapshot(share: &TempDir) -> (Snapshot, MemoryChunkStore) {
    let mut store = MemoryChunkStore::new();
    let snap = snapshot_dir(share.path(), "swarm-share", 1, &mut store).unwrap();
    (snap, store)
}

/// The share's chunk hashes, de-duplicated, in a stable order.
fn distinct_chunks(chunk_lists: &BTreeMap<String, ChunkList>) -> Vec<Hash> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for list in chunk_lists.values() {
        for c in &list.chunks {
            if seen.insert(c.hash) {
                out.push(c.hash);
            }
        }
    }
    out
}

/// A seed that serves the whole share.
async fn full_seed(share: &TempDir) -> Node {
    let (snap, store) = snapshot(share);
    let catalog = Catalog::new(snap.manifest, snap.chunk_lists, store);
    Node::spawn_serving(catalog).await.unwrap()
}

/// A seed that serves only the chunks `keep` selects (by index into
/// [`distinct_chunks`]). The manifest and chunk lists are always served in full.
async fn partial_seed(share: &TempDir, keep: impl Fn(usize) -> bool) -> Node {
    let (snap, store) = snapshot(share);
    let chunks = distinct_chunks(&snap.chunk_lists);

    let mut partial = MemoryChunkStore::new();
    for (i, hash) in chunks.iter().enumerate() {
        if keep(i) {
            partial.put(*hash, store.get(hash).unwrap());
        }
    }
    let catalog = Catalog::new(snap.manifest, snap.chunk_lists, partial);
    Node::spawn_serving(catalog).await.unwrap()
}

async fn subscriber_wired_to(seeds: &[&Node]) -> Node {
    let sub = Node::spawn().await.unwrap();
    for seed in seeds {
        let addr = seed.listen_addrs().await.unwrap().into_iter().next().unwrap();
        sub.add_peer_address(seed.peer_id(), addr).await.unwrap();
    }
    sub
}

fn reconstruct_and_check(share: &TempDir, manifest: &Manifest, store: &MemoryChunkStore) {
    let (snap, _) = snapshot(share);
    for file in &manifest.files {
        let list = &snap.chunk_lists[&file.path];
        let mut rebuilt = Vec::new();
        for chunk in &list.chunks {
            let bytes = store.get(&chunk.hash).expect("missing chunk after swarm download");
            assert_eq!(Hash::of(&bytes), chunk.hash);
            rebuilt.extend_from_slice(&bytes);
        }
        let original = fs::read(share.path().join(&file.path)).unwrap();
        assert_eq!(rebuilt, original, "{} did not round-trip", file.path);
    }
}

#[tokio::test]
async fn swarm_download_spreads_load_across_full_seeds() {
    let share = sample_share();
    let manifest = snapshot(&share).0.manifest;
    let needed = distinct_chunks(&snapshot(&share).0.chunk_lists).len();
    assert!(needed >= 6, "test share should chunk into several pieces, got {needed}");

    let a = full_seed(&share).await;
    let b = full_seed(&share).await;
    let c = full_seed(&share).await;
    let sub = subscriber_wired_to(&[&a, &b, &c]).await;

    let mut store = MemoryChunkStore::new();
    let out = timeout(
        Duration::from_secs(30),
        sub.download_share_multi(&[a.peer_id(), b.peer_id(), c.peer_id()], &mut store),
    )
    .await
    .expect("swarm download timed out")
    .unwrap();

    assert_eq!(out.share.manifest, manifest);
    reconstruct_and_check(&share, &manifest, &store);

    let supplied: usize = out.chunks_per_source.values().sum();
    assert_eq!(supplied, needed, "every needed chunk should be accounted to a source");
    assert!(
        out.chunks_per_source.len() >= 2,
        "work should have been spread across seeds, only {:?} contributed",
        out.chunks_per_source
    );

    sub.shutdown().await;
    a.shutdown().await;
    b.shutdown().await;
    c.shutdown().await;
}

#[tokio::test]
async fn swarm_download_stitches_a_share_from_partial_seeds() {
    let share = sample_share();
    let manifest = snapshot(&share).0.manifest;
    let chunks = distinct_chunks(&snapshot(&share).0.chunk_lists);
    let split = chunks.len() / 2;

    // `a` has the first half, `b` the second half; no chunk is on both.
    let a = partial_seed(&share, move |i| i < split).await;
    let b = partial_seed(&share, move |i| i >= split).await;
    let sub = subscriber_wired_to(&[&a, &b]).await;

    let mut store = MemoryChunkStore::new();
    let out = timeout(
        Duration::from_secs(30),
        sub.download_share_multi(&[a.peer_id(), b.peer_id()], &mut store),
    )
    .await
    .expect("partial swarm download timed out")
    .unwrap();

    assert_eq!(out.share.manifest, manifest);
    reconstruct_and_check(&share, &manifest, &store);
    assert!(
        out.chunks_per_source.get(&a.peer_id()).copied().unwrap_or(0) > 0
            && out.chunks_per_source.get(&b.peer_id()).copied().unwrap_or(0) > 0,
        "both partial seeds should have contributed: {:?}",
        out.chunks_per_source
    );

    sub.shutdown().await;
    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test]
async fn swarm_download_routes_around_a_dead_source() {
    let share = sample_share();
    let manifest = snapshot(&share).0.manifest;

    let alive = full_seed(&share).await;
    let dead = full_seed(&share).await;
    let sub = subscriber_wired_to(&[&alive, &dead]).await;

    let alive_id = alive.peer_id();
    let dead_id = dead.peer_id();
    dead.shutdown().await;

    let mut store = MemoryChunkStore::new();
    let out = timeout(
        Duration::from_secs(30),
        sub.download_share_multi(&[alive_id, dead_id], &mut store),
    )
    .await
    .expect("swarm download with a dead source timed out")
    .unwrap();

    assert_eq!(out.share.manifest, manifest);
    reconstruct_and_check(&share, &manifest, &store);
    assert_eq!(
        out.chunks_per_source.keys().copied().collect::<Vec<_>>(),
        vec![alive_id],
        "only the live seed should have supplied chunks"
    );

    sub.shutdown().await;
    alive.shutdown().await;
}

/// The point of a peer swarm: whoever finishes downloading turns around and
/// serves what they pulled.
#[tokio::test]
async fn a_downloader_re_seeds_what_it_pulled() {
    let share = sample_share();
    let manifest = snapshot(&share).0.manifest;

    let origin = full_seed(&share).await;

    // `mid` pulls the whole share from `origin`, then serves it.
    let mid = Node::spawn().await.unwrap();
    let origin_addr =
        origin.listen_addrs().await.unwrap().into_iter().next().unwrap();
    mid.add_peer_address(origin.peer_id(), origin_addr).await.unwrap();
    let mut mid_store = MemoryChunkStore::new();
    let pulled = timeout(
        Duration::from_secs(30),
        mid.download_share_multi(&[origin.peer_id()], &mut mid_store),
    )
    .await
    .expect("mid download timed out")
    .unwrap();
    mid.serve(catalog_from_download(pulled.share, mid_store)).await.unwrap();

    // `leech` knows only `mid` and must get everything from it.
    let leech = subscriber_wired_to(&[&mid]).await;
    let mut store = MemoryChunkStore::new();
    let out = timeout(
        Duration::from_secs(30),
        leech.download_share_multi(&[mid.peer_id()], &mut store),
    )
    .await
    .expect("leech download timed out")
    .unwrap();

    assert_eq!(out.share.manifest, manifest);
    reconstruct_and_check(&share, &manifest, &store);
    assert_eq!(out.chunks_per_source.keys().copied().collect::<Vec<_>>(), vec![mid.peer_id()]);

    leech.shutdown().await;
    mid.shutdown().await;
    origin.shutdown().await;
}
