//! A share announced on the Kademlia DHT is discovered by a peer
//! that only knows the bootstrap node, and a peer reachable only through a relay
//! circuit is still served (with an opportunistic dcutr upgrade to a direct
//! connection).

use std::fs;
use std::time::Duration;

use gaggle_core::{ChunkStore, Hash, MemoryChunkStore, snapshot_dir};
use net::{Catalog, Multiaddr, Node, NodeEvent, RelayConfig, RelayNode, ShareKey};
use tempfile::TempDir;
use tokio::time::timeout;

fn sample_share() -> TempDir {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::create_dir(root.join("mods")).unwrap();
    fs::write(root.join("readme.txt"), b"discover me over the dht\n").unwrap();
    fs::write(root.join("mods/a.cfg"), b"key=value\n").unwrap();

    let mut blob = Vec::with_capacity(3 * 1024 * 1024);
    let mut state = 0x1234_5678_9abc_def0u64;
    while blob.len() < 3 * 1024 * 1024 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        blob.extend_from_slice(&state.to_le_bytes());
    }
    fs::write(root.join("mods/big.pak"), &blob).unwrap();
    dir
}

/// Block until the node has learned a candidate for its own external address —
/// dcutr cannot hole-punch before it has one.
async fn wait_external_candidate(events: &mut tokio::sync::broadcast::Receiver<NodeEvent>) {
    timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(NodeEvent::ExternalAddressCandidate { .. }) = events.recv().await {
                break;
            }
        }
    })
    .await
    .expect("no external address candidate within 10s");
}

fn reconstruct_and_check(
    share: &TempDir,
    got: &net::DownloadedShare,
    store: &MemoryChunkStore,
) {
    for file in &got.manifest.files {
        let list = &got.chunk_lists[&file.path];
        let mut rebuilt = Vec::new();
        for chunk in &list.chunks {
            let bytes = store.get(&chunk.hash).expect("missing chunk after download");
            assert_eq!(Hash::of(&bytes), chunk.hash);
            rebuilt.extend_from_slice(&bytes);
        }
        let original = fs::read(share.path().join(&file.path)).unwrap();
        assert_eq!(rebuilt, original, "{} did not round-trip", file.path);
    }
}

#[tokio::test]
async fn share_is_discovered_through_the_dht() {
    let relay = RelayNode::spawn().await.unwrap();
    let bootstrap_addr = relay.listen_addr().await.unwrap();

    // Origin: snapshot a folder, join the DHT, announce the share.
    let share = sample_share();
    let mut origin_store = MemoryChunkStore::new();
    let snapshot = snapshot_dir(share.path(), "discover-me", 1, &mut origin_store).unwrap();
    let manifest = snapshot.manifest.clone();
    let key = ShareKey::from_manifest(&manifest);

    let origin =
        Node::spawn_serving(Catalog::new(snapshot.manifest, snapshot.chunk_lists, origin_store))
            .await
            .unwrap();
    origin.bootstrap(bootstrap_addr.clone()).await.unwrap();
    origin.provide(key.clone()).await.unwrap();

    // Subscriber: knows only the bootstrap node.
    let subscriber = Node::spawn().await.unwrap();
    subscriber.bootstrap(bootstrap_addr.clone()).await.unwrap();

    let providers = timeout(Duration::from_secs(20), subscriber.find_providers(key))
        .await
        .expect("find_providers timed out")
        .unwrap();
    assert!(
        providers.contains(&origin.peer_id()),
        "origin {} not among discovered providers {providers:?}",
        origin.peer_id()
    );

    // ...and the discovered peer can actually be reached and served.
    let mut store = MemoryChunkStore::new();
    let got = timeout(
        Duration::from_secs(20),
        subscriber.download_share(origin.peer_id(), &mut store),
    )
    .await
    .expect("download timed out")
    .unwrap();

    assert_eq!(got.manifest, manifest);
    reconstruct_and_check(&share, &got, &store);

    subscriber.shutdown().await;
    origin.shutdown().await;
    relay.shutdown().await;
}

#[tokio::test]
async fn peer_behind_a_relay_is_reachable_and_upgrades_to_direct() {
    // mDNS (deliberately) skips loopback interfaces, so keeping every node in
    // this test on `127.0.0.1` stops mDNS from short-circuiting the very
    // relay/dcutr path being tested — on a box with a real LAN interface, two
    // same-host nodes would otherwise find each other directly and dcutr
    // would have nothing to upgrade.
    let loopback: Multiaddr = "/ip4/127.0.0.1/udp/0/quic-v1".parse().unwrap();
    let relay = RelayNode::spawn_with_opts(RelayConfig::default(), None, Some(loopback.clone()))
        .await
        .unwrap();
    let relay_addr = relay.listen_addr().await.unwrap();

    // Origin reaches the relay and takes a circuit reservation. The circuit
    // address is the only way in.
    let share = sample_share();
    let mut origin_store = MemoryChunkStore::new();
    let snapshot = snapshot_dir(share.path(), "relayed", 1, &mut origin_store).unwrap();
    let manifest = snapshot.manifest.clone();
    let origin = Node::spawn_serving_with(
        Catalog::new(snapshot.manifest, snapshot.chunk_lists, origin_store),
        None,
        Some(loopback.clone()),
    )
    .await
    .unwrap();
    let mut origin_events = origin.events();
    origin.bootstrap(relay_addr.clone()).await.unwrap();
    wait_external_candidate(&mut origin_events).await;
    let circuit_addr = timeout(
        Duration::from_secs(20),
        origin.reserve_relay_slot(relay.peer_id(), relay_addr.clone()),
    )
    .await
    .expect("relay reservation timed out")
    .unwrap();

    // Subscriber also knows the relay, and is handed the circuit address.
    let subscriber = Node::spawn_with(None, Some(loopback)).await.unwrap();
    let mut upgrades = subscriber.events();
    subscriber.bootstrap(relay_addr.clone()).await.unwrap();
    wait_external_candidate(&mut upgrades).await;
    subscriber.add_peer_address(origin.peer_id(), circuit_addr).await.unwrap();

    let mut store = MemoryChunkStore::new();
    let got = timeout(
        Duration::from_secs(30),
        subscriber.download_share(origin.peer_id(), &mut store),
    )
    .await
    .expect("relayed download timed out")
    .unwrap();
    assert_eq!(got.manifest, manifest);
    reconstruct_and_check(&share, &got, &store);

    // dcutr should turn the relayed connection into a direct one.
    let direct = timeout(Duration::from_secs(20), async {
        loop {
            match upgrades.recv().await {
                Ok(NodeEvent::HolePunch { peer, direct }) if peer == origin.peer_id() => {
                    return direct;
                }
                Ok(_) => continue,
                Err(_) => return false,
            }
        }
    })
    .await
    .expect("no dcutr outcome for the relayed peer within 20s");
    assert!(direct, "dcutr ran but failed to upgrade the relayed connection to direct");

    subscriber.shutdown().await;
    origin.shutdown().await;
    relay.shutdown().await;
}
