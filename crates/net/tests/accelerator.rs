//! The two accelerator roles.
//!
//! * **Relay accelerator** — a hot-chunk cache in front of the chunk protocol.
//!   A swarm of peers pulling from the relay costs the origin only one fetch per
//!   hot chunk, and a tight cache budget still serves a share larger than it by
//!   re-fetching cold chunks from upstream.
//! * **NAS accelerator** — a durable on-disk full replica that keeps serving
//!   after a restart with the origin offline, and that a downloader prefers over
//!   WAN peers when told to.

use std::collections::BTreeMap;
use std::fs;
use std::time::Duration;

use gaggle_core::{
    ChunkList, ChunkStore, DiskChunkStore, Hash, Manifest, MemoryChunkStore, snapshot_dir,
};
use net::{Catalog, Node, RelayConfig, RelayNode, SwarmConfig};
use tempfile::TempDir;
use tokio::time::timeout;

const MIB: usize = 1024 * 1024;

/// A share whose big file splits into a dozen-ish ~1 MiB chunks.
fn sample_share() -> TempDir {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::create_dir(root.join("mods")).unwrap();
    fs::write(root.join("readme.txt"), b"accelerator test\n").unwrap();

    let mut blob = Vec::with_capacity(12 * MIB);
    let mut state = 0x8d2f_1c07_a4b9_e351u64;
    while blob.len() < 12 * MIB {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        blob.extend_from_slice(&state.to_le_bytes());
    }
    fs::write(root.join("mods/pack.bin"), &blob).unwrap();
    dir
}

fn snapshot(share: &TempDir) -> (Manifest, BTreeMap<String, ChunkList>, MemoryChunkStore) {
    let mut store = MemoryChunkStore::new();
    let snap = snapshot_dir(share.path(), "accel-share", 1, &mut store).unwrap();
    (snap.manifest, snap.chunk_lists, store)
}

fn distinct_chunk_count(lists: &BTreeMap<String, ChunkList>) -> usize {
    let mut seen = std::collections::HashSet::new();
    for list in lists.values() {
        for c in &list.chunks {
            seen.insert(c.hash);
        }
    }
    seen.len()
}

async fn full_seed(share: &TempDir) -> (Node, Manifest, BTreeMap<String, ChunkList>) {
    let (manifest, lists, store) = snapshot(share);
    let node =
        Node::spawn_serving(Catalog::new(manifest.clone(), lists.clone(), store)).await.unwrap();
    (node, manifest, lists)
}

async fn bare_addr(node: &Node) -> net::Multiaddr {
    node.listen_addrs().await.unwrap().into_iter().next().unwrap()
}

fn reconstruct_and_check(share: &TempDir, manifest: &Manifest, store: &dyn ChunkStore) {
    let (_, lists, _) = snapshot(share);
    for file in &manifest.files {
        let mut rebuilt = Vec::new();
        for chunk in &lists[&file.path].chunks {
            let bytes = store.get(&chunk.hash).expect("missing chunk after download");
            assert_eq!(Hash::of(&bytes), chunk.hash);
            rebuilt.extend_from_slice(&bytes);
        }
        let original = fs::read(share.path().join(&file.path)).unwrap();
        assert_eq!(rebuilt, original, "{} did not round-trip", file.path);
    }
}

// --- Relay accelerator hot-chunk cache ----------------------------------

#[tokio::test]
async fn relay_cache_serves_a_share_and_shields_the_origin() {
    let share = sample_share();
    let (origin, manifest, lists) = full_seed(&share).await;
    let n_chunks = distinct_chunk_count(&lists);

    let relay = RelayNode::spawn_with(RelayConfig { cache_capacity_bytes: 64 * MIB as u64 })
        .await
        .unwrap();
    let origin_id = relay.add_upstream(origin.listen_addr().await.unwrap()).await.unwrap();
    relay.cache_share(manifest.clone(), lists.values().cloned(), vec![origin_id]).await.unwrap();

    // First downloader knows only the relay.
    let relay_addr = relay.listen_addrs().await.unwrap().into_iter().next().unwrap();
    let sub_a = Node::spawn().await.unwrap();
    sub_a.add_peer_address(relay.peer_id(), relay_addr.clone()).await.unwrap();

    let mut store_a = MemoryChunkStore::new();
    let got_a = timeout(
        Duration::from_secs(30),
        sub_a.download_share(relay.peer_id(), &mut store_a),
    )
    .await
    .expect("download via relay timed out")
    .unwrap();
    assert_eq!(got_a.manifest, manifest);
    reconstruct_and_check(&share, &manifest, &store_a);

    let after_a = relay.cache_stats().await.unwrap();
    assert_eq!(after_a.misses as usize, n_chunks, "each chunk should be one upstream fill");
    assert_eq!(after_a.hits, 0);
    assert!(after_a.used_bytes > 0 && after_a.chunks as usize == n_chunks);

    // Second downloader, also relay-only: now everything is a cache hit and the
    // origin is not touched again.
    let sub_b = Node::spawn().await.unwrap();
    sub_b.add_peer_address(relay.peer_id(), relay_addr).await.unwrap();
    let mut store_b = MemoryChunkStore::new();
    timeout(Duration::from_secs(30), sub_b.download_share(relay.peer_id(), &mut store_b))
        .await
        .expect("second download via relay timed out")
        .unwrap();
    reconstruct_and_check(&share, &manifest, &store_b);

    let after_b = relay.cache_stats().await.unwrap();
    assert_eq!(after_b.misses, after_a.misses, "origin was not asked again for hot chunks");
    assert_eq!(after_b.hits as usize, n_chunks, "second pull came entirely from cache");

    sub_a.shutdown().await;
    sub_b.shutdown().await;
    origin.shutdown().await;
    relay.shutdown().await;
}

#[tokio::test]
async fn relay_cache_evicts_under_a_tight_budget() {
    let share = sample_share();
    let (origin, manifest, lists) = full_seed(&share).await;
    let n_chunks = distinct_chunk_count(&lists);

    // Budget for only ~3 of a dozen chunks — eviction is forced.
    let budget = 3 * MIB as u64;
    let relay = RelayNode::spawn_with(RelayConfig { cache_capacity_bytes: budget }).await.unwrap();
    let origin_id = relay.add_upstream(origin.listen_addr().await.unwrap()).await.unwrap();
    relay.cache_share(manifest.clone(), lists.values().cloned(), vec![origin_id]).await.unwrap();

    let relay_addr = relay.listen_addrs().await.unwrap().into_iter().next().unwrap();
    let sub = Node::spawn().await.unwrap();
    sub.add_peer_address(relay.peer_id(), relay_addr).await.unwrap();

    let mut store = MemoryChunkStore::new();
    let got = timeout(Duration::from_secs(30), sub.download_share(relay.peer_id(), &mut store))
        .await
        .expect("tight-budget download timed out")
        .unwrap();
    assert_eq!(got.manifest, manifest);
    reconstruct_and_check(&share, &manifest, &store);

    let stats = relay.cache_stats().await.unwrap();
    assert!(stats.used_bytes <= budget, "cache stayed within its budget: {stats:?}");
    assert!(stats.evictions > 0, "a share larger than the budget should have evicted: {stats:?}");
    assert_eq!(stats.misses as usize, n_chunks);

    sub.shutdown().await;
    origin.shutdown().await;
    relay.shutdown().await;
}

// --- Cache/NAS accelerator durable replica -----------------------------

#[tokio::test]
async fn nas_replica_is_durable_and_serves_after_restart() {
    let share = sample_share();
    let chunk_dir = TempDir::new().unwrap();

    // Phase 1: replicate the whole share onto disk, then everything shuts down.
    let (manifest, lists) = {
        let (origin, manifest, _) = full_seed(&share).await;
        let nas = Node::spawn().await.unwrap();
        nas.add_peer_address(origin.peer_id(), bare_addr(&origin).await).await.unwrap();

        let mut disk = DiskChunkStore::open(chunk_dir.path()).unwrap();
        let pulled = timeout(
            Duration::from_secs(30),
            nas.download_share_multi(&[origin.peer_id()], &mut disk),
        )
        .await
        .expect("nas replication timed out")
        .unwrap();
        assert_eq!(disk.io_errors(), 0);

        nas.shutdown().await;
        origin.shutdown().await;
        (manifest, pulled.share.chunk_lists)
    };

    // Phase 2: a fresh process re-opens the chunk directory and serves from it,
    // with the origin gone.
    let disk = DiskChunkStore::open(chunk_dir.path()).unwrap();
    assert_eq!(disk.len(), distinct_chunk_count(&lists));
    let nas = Node::spawn_serving(Catalog::new(manifest.clone(), lists.clone(), disk))
        .await
        .unwrap();

    let sub = Node::spawn().await.unwrap();
    sub.add_peer_address(nas.peer_id(), bare_addr(&nas).await).await.unwrap();
    let mut store = MemoryChunkStore::new();
    let got = timeout(Duration::from_secs(30), sub.download_share(nas.peer_id(), &mut store))
        .await
        .expect("download from restarted nas timed out")
        .unwrap();

    assert_eq!(got.manifest, manifest);
    reconstruct_and_check(&share, &manifest, &store);

    sub.shutdown().await;
    nas.shutdown().await;
}

#[tokio::test]
async fn nas_replication_resumes_a_partial_disk_store() {
    let share = sample_share();
    let chunk_dir = TempDir::new().unwrap();
    let (origin, _, lists) = full_seed(&share).await;
    let n_chunks = distinct_chunk_count(&lists);

    // Pre-seed the disk store with a couple of the share's chunks.
    {
        let (_, _, full) = snapshot(&share);
        let mut disk = DiskChunkStore::open(chunk_dir.path()).unwrap();
        let some: Vec<Hash> =
            lists.values().flat_map(|l| l.chunks.iter().map(|c| c.hash)).take(2).collect();
        for h in some {
            disk.put(h, full.get(&h).unwrap());
        }
        assert_eq!(disk.len(), 2);
    }

    let nas = Node::spawn().await.unwrap();
    nas.add_peer_address(origin.peer_id(), bare_addr(&origin).await).await.unwrap();
    let mut disk = DiskChunkStore::open(chunk_dir.path()).unwrap();
    let pulled = timeout(
        Duration::from_secs(30),
        nas.download_share_multi(&[origin.peer_id()], &mut disk),
    )
    .await
    .expect("resumed replication timed out")
    .unwrap();

    assert_eq!(disk.len(), n_chunks, "replica is complete");
    let fetched: usize = pulled.chunks_per_source.values().sum();
    assert_eq!(fetched, n_chunks - 2, "the 2 pre-seeded chunks were not re-fetched");

    nas.shutdown().await;
    origin.shutdown().await;
}

#[tokio::test]
async fn nas_lan_priority_pulls_from_the_replica_first() {
    let share = sample_share();
    let (origin, manifest, lists) = full_seed(&share).await;
    let (nas, _, _) = full_seed(&share).await;
    let n_chunks = distinct_chunk_count(&lists);

    let sub = Node::spawn().await.unwrap();
    sub.add_peer_address(origin.peer_id(), bare_addr(&origin).await).await.unwrap();
    sub.add_peer_address(nas.peer_id(), bare_addr(&nas).await).await.unwrap();

    // High parallelism so the preferred (LAN) replica never saturates and the
    // WAN origin is never needed.
    let cfg = SwarmConfig {
        per_peer_parallelism: 64,
        prefer: vec![nas.peer_id()],
        ..SwarmConfig::default()
    };
    let mut store = MemoryChunkStore::new();
    let out = timeout(
        Duration::from_secs(30),
        sub.download_share_multi_with(&[origin.peer_id(), nas.peer_id()], &mut store, cfg),
    )
    .await
    .expect("lan-priority download timed out")
    .unwrap();

    assert_eq!(out.share.manifest, manifest);
    reconstruct_and_check(&share, &manifest, &store);
    assert_eq!(
        out.chunks_per_source.get(&origin.peer_id()).copied().unwrap_or(0),
        0,
        "nothing should have come from the WAN origin: {:?}",
        out.chunks_per_source
    );
    assert_eq!(out.chunks_per_source.get(&nas.peer_id()).copied(), Some(n_chunks));

    sub.shutdown().await;
    origin.shutdown().await;
    nas.shutdown().await;
}
