//! Private swarms and per-item permissions.
//!
//! A restricted `Node` / `RelayNode` serves nothing until a connection presents
//! a capability the share's keypair signed, and then only the files that
//! capability's [`Scope`] names. Invites round-trip through a `gaggle1…` string.

use std::collections::BTreeMap;
use std::fs;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gaggle_core::{
    Capability, ChunkList, ChunkStore, Hash, Invite, Manifest, MemoryChunkStore, Scope,
    ShareKeypair, snapshot_dir,
};
use net::{Catalog, Node, Request, Response};
use tempfile::TempDir;
use tokio::time::timeout;

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

/// Two files that both chunk, so per-file scope is observable.
fn sample_share() -> TempDir {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::create_dir(root.join("mods")).unwrap();
    fs::write(root.join("mods/a.cfg"), b"quality=ultra\n").unwrap();

    let mut blob = Vec::with_capacity(4 * 1024 * 1024);
    let mut state = 0x51ed_2701_9c3a_f24bu64;
    while blob.len() < 4 * 1024 * 1024 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        blob.extend_from_slice(&state.to_le_bytes());
    }
    fs::write(root.join("mods/big.pak"), &blob).unwrap();
    dir
}

fn snapshot(share: &TempDir) -> (Manifest, BTreeMap<String, ChunkList>, MemoryChunkStore) {
    let mut store = MemoryChunkStore::new();
    let snap = snapshot_dir(share.path(), "private-share", 1, &mut store).unwrap();
    (snap.manifest, snap.chunk_lists, store)
}

/// A private origin plus its share keypair.
async fn private_origin(share: &TempDir) -> (Node, ShareKeypair, Manifest, BTreeMap<String, ChunkList>) {
    let (manifest, lists, store) = snapshot(share);
    let keypair = ShareKeypair::from_seed([77u8; 32]);
    let origin = Node::spawn_serving(Catalog::new(manifest.clone(), lists.clone(), store))
        .await
        .unwrap();
    origin.restrict_to_invite_holders(keypair.public()).await.unwrap();
    (origin, keypair, manifest, lists)
}

async fn connect(sub: &Node, origin: &Node) {
    let addr = origin.listen_addrs().await.unwrap().into_iter().next().unwrap();
    sub.add_peer_address(origin.peer_id(), addr).await.unwrap();
}

fn reconstruct_ok(share: &TempDir, manifest: &Manifest, store: &dyn ChunkStore) {
    let (_, lists, _) = snapshot(share);
    for file in &manifest.files {
        let mut rebuilt = Vec::new();
        for chunk in &lists[&file.path].chunks {
            rebuilt.extend_from_slice(&store.get(&chunk.hash).expect("missing chunk"));
        }
        assert_eq!(rebuilt, fs::read(share.path().join(&file.path)).unwrap(), "{}", file.path);
    }
}

#[tokio::test]
async fn a_private_share_refuses_a_peer_with_no_invite() {
    let share = sample_share();
    let (origin, _kp, manifest, _lists) = private_origin(&share).await;

    let sub = Node::spawn().await.unwrap();
    connect(&sub, &origin).await;

    // A raw request is bounced.
    let resp = sub.request(origin.peer_id(), Request::GetManifest).await.unwrap();
    assert!(matches!(resp, Response::Unauthorized(_)), "got {resp:?}");

    // And so is a full download.
    let mut store = MemoryChunkStore::new();
    let err = sub.download_share(origin.peer_id(), &mut store).await.unwrap_err();
    assert!(format!("{err:#}").contains("Unauthorized"), "unexpected error: {err:#}");
    let _ = manifest;

    sub.shutdown().await;
    origin.shutdown().await;
}

#[tokio::test]
async fn an_invited_peer_downloads_the_whole_share() {
    let share = sample_share();
    let (origin, keypair, manifest, _lists) = private_origin(&share).await;

    let credential = keypair.issue(Capability::new(keypair.public(), manifest.id()));

    let sub = Node::spawn().await.unwrap();
    connect(&sub, &origin).await;
    sub.authenticate(origin.peer_id(), &credential).await.unwrap();

    let mut store = MemoryChunkStore::new();
    let got = timeout(
        Duration::from_secs(20),
        sub.download_share(origin.peer_id(), &mut store),
    )
    .await
    .expect("download timed out")
    .unwrap();
    assert_eq!(got.manifest, manifest);
    reconstruct_ok(&share, &manifest, &store);

    sub.shutdown().await;
    origin.shutdown().await;
}

#[tokio::test]
async fn per_file_scope_is_enforced() {
    let share = sample_share();
    let (origin, keypair, manifest, lists) = private_origin(&share).await;

    // Grant only the small file.
    let cred = keypair.issue(
        Capability::new(keypair.public(), manifest.id())
            .with_scope(Scope::files(["mods/a.cfg"])),
    );
    let sub = Node::spawn().await.unwrap();
    connect(&sub, &origin).await;
    sub.authenticate(origin.peer_id(), &cred).await.unwrap();

    let allowed_root = manifest.file("mods/a.cfg").unwrap().root;
    let denied_root = manifest.file("mods/big.pak").unwrap().root;

    // The allowed file's chunk list and chunks come back.
    let list = match sub.request(origin.peer_id(), Request::GetChunkList(allowed_root)).await.unwrap() {
        Response::ChunkList(l) => l,
        other => panic!("expected the allowed chunk list, got {other:?}"),
    };
    let a_chunk = list.chunks[0].hash;
    assert!(matches!(
        sub.request(origin.peer_id(), Request::GetChunk(a_chunk)).await.unwrap(),
        Response::Chunk(_)
    ));

    // The denied file is walled off.
    assert!(matches!(
        sub.request(origin.peer_id(), Request::GetChunkList(denied_root)).await.unwrap(),
        Response::Unauthorized(_)
    ));
    let big_chunk = lists["mods/big.pak"].chunks[0].hash;
    assert!(matches!(
        sub.request(origin.peer_id(), Request::GetChunk(big_chunk)).await.unwrap(),
        Response::Unauthorized(_)
    ));

    // Inventory is filtered to the granted file.
    let inv = match sub.request(origin.peer_id(), Request::GetInventory).await.unwrap() {
        Response::Inventory(h) => h,
        other => panic!("expected inventory, got {other:?}"),
    };
    let allowed: std::collections::HashSet<Hash> =
        lists["mods/a.cfg"].chunks.iter().map(|c| c.hash).collect();
    assert!(!inv.is_empty() && inv.iter().all(|h| allowed.contains(h)));

    // A whole-share download stops as soon as it reaches the denied file.
    let mut store = MemoryChunkStore::new();
    assert!(sub.download_share(origin.peer_id(), &mut store).await.is_err());

    sub.shutdown().await;
    origin.shutdown().await;
}

#[tokio::test]
async fn an_expired_capability_is_refused() {
    let share = sample_share();
    let (origin, keypair, manifest, _lists) = private_origin(&share).await;

    let cred = keypair.issue(
        Capability::new(keypair.public(), manifest.id()).expiring_at(now().saturating_sub(60)),
    );
    let sub = Node::spawn().await.unwrap();
    connect(&sub, &origin).await;

    let err = sub.authenticate(origin.peer_id(), &cred).await.unwrap_err();
    assert!(format!("{err:#}").contains("expired"), "unexpected: {err:#}");

    sub.shutdown().await;
    origin.shutdown().await;
}

#[tokio::test]
async fn a_capability_from_the_wrong_key_is_refused() {
    let share = sample_share();
    let (origin, _keypair, manifest, _lists) = private_origin(&share).await;

    let impostor = ShareKeypair::from_seed([0xabu8; 32]);
    let cred = impostor.issue(Capability::new(impostor.public(), manifest.id()));

    let sub = Node::spawn().await.unwrap();
    connect(&sub, &origin).await;
    assert!(sub.authenticate(origin.peer_id(), &cred).await.is_err());

    sub.shutdown().await;
    origin.shutdown().await;
}

#[tokio::test]
async fn an_invite_url_round_trips_through_a_real_swarm() {
    let share = sample_share();
    let (origin, keypair, manifest, _lists) = private_origin(&share).await;

    // Origin mints an invite link.
    let cred = keypair.issue(Capability::new(keypair.public(), manifest.id()));
    let url = Invite::new(keypair.public(), manifest.id(), &manifest.name, cred).to_url();

    // Subscriber parses it, checks it, and joins.
    let invite = Invite::parse(&url).unwrap();
    invite.validate(now()).unwrap();
    assert_eq!(invite.manifest_id, manifest.id());

    let sub = Node::spawn().await.unwrap();
    connect(&sub, &origin).await;
    sub.authenticate(origin.peer_id(), &invite.credential).await.unwrap();

    let mut store = MemoryChunkStore::new();
    timeout(Duration::from_secs(20), sub.download_share(origin.peer_id(), &mut store))
        .await
        .expect("download timed out")
        .unwrap();
    reconstruct_ok(&share, &manifest, &store);

    sub.shutdown().await;
    origin.shutdown().await;
}

#[tokio::test]
async fn a_private_relay_only_serves_invite_holders() {
    let share = sample_share();
    let (manifest, lists, store) = snapshot(&share);
    let keypair = ShareKeypair::from_seed([12u8; 32]);

    // Public origin (it trusts the relay), private relay in front of it.
    let origin = Node::spawn_serving(Catalog::new(manifest.clone(), lists.clone(), store))
        .await
        .unwrap();
    let relay = net::RelayNode::spawn_with(net::RelayConfig { cache_capacity_bytes: 64 << 20 })
        .await
        .unwrap();
    let origin_id = relay.add_upstream(origin.listen_addr().await.unwrap()).await.unwrap();
    relay.cache_share(manifest.clone(), lists.values().cloned(), vec![origin_id]).await.unwrap();
    relay.restrict_to_invite_holders(keypair.public()).await.unwrap();

    let relay_addr = relay.listen_addrs().await.unwrap().into_iter().next().unwrap();

    // No invite → the relay refuses.
    let outsider = Node::spawn().await.unwrap();
    outsider.add_peer_address(relay.peer_id(), relay_addr.clone()).await.unwrap();
    let mut s = MemoryChunkStore::new();
    assert!(outsider.download_share(relay.peer_id(), &mut s).await.is_err());

    // With an invite → served from the relay's cache.
    let cred = keypair.issue(Capability::new(keypair.public(), manifest.id()));
    let member = Node::spawn().await.unwrap();
    member.add_peer_address(relay.peer_id(), relay_addr).await.unwrap();
    member.authenticate(relay.peer_id(), &cred).await.unwrap();

    let mut store = MemoryChunkStore::new();
    timeout(Duration::from_secs(20), member.download_share(relay.peer_id(), &mut store))
        .await
        .expect("relay download timed out")
        .unwrap();
    reconstruct_ok(&share, &manifest, &store);
    assert!(relay.cache_stats().await.unwrap().misses > 0);

    outsider.shutdown().await;
    member.shutdown().await;
    origin.shutdown().await;
    relay.shutdown().await;
}
