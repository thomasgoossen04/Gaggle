//! The seeder-tracker endpoints round-trip announce / list / withdraw
//! through a live HTTP server, exactly as an origin, a NAS replica, and a
//! downloader would use them.

use control_plane::{PeerInfo, TrackerClient, TrackerRegistry, tracker_router};

async fn serve(registry: TrackerRegistry) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, tracker_router(registry)).await.unwrap();
    });
    format!("http://{addr}")
}

fn info(id: &str) -> PeerInfo {
    PeerInfo {
        peer_id: id.to_string(),
        addrs: vec![format!("/ip4/203.0.113.7/udp/4001/quic-v1/p2p/{id}")],
    }
}

#[tokio::test]
async fn a_downloader_discovers_every_announced_seeder() {
    let base = serve(TrackerRegistry::new()).await;
    let origin = TrackerClient::new(&base);
    let replica = TrackerClient::new(&base);
    let downloader = TrackerClient::new(&base);

    origin.announce("share-1", &info("origin-peer")).await.unwrap();
    replica.announce("share-1", &info("replica-peer")).await.unwrap();

    let mut ids: Vec<String> = downloader
        .seeders("share-1")
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.peer_id)
        .collect();
    ids.sort();
    assert_eq!(ids, vec!["origin-peer".to_string(), "replica-peer".to_string()]);

    // A share nobody announced is just empty, not an error.
    assert!(downloader.seeders("share-2").await.unwrap().is_empty());
}

#[tokio::test]
async fn the_open_directory_lists_public_shares_by_name() {
    let base = serve(TrackerRegistry::new()).await;
    let seeder = TrackerClient::new(&base);
    let browser = TrackerClient::new(&base);

    seeder.announce_share("pub-a", &info("a1"), Some("Skyrim Modpack"), false).await.unwrap();
    seeder.announce_share("pub-a", &info("a2"), Some("Skyrim Modpack"), false).await.unwrap();
    seeder.announce_share("pub-b", &info("b1"), Some("Blender Assets"), false).await.unwrap();
    // A private share is tracked (invite holders still swarm) but never listed.
    seeder.announce_share("priv-c", &info("c1"), Some("Private Backup"), true).await.unwrap();

    let dir = browser.directory().await.unwrap();
    let names: Vec<&str> = dir.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["Blender Assets", "Skyrim Modpack"]);

    let a = dir.iter().find(|e| e.manifest_id == "pub-a").unwrap();
    assert_eq!(a.name, "Skyrim Modpack");
    assert_eq!(a.seeders, 2);
    assert!(!dir.iter().any(|e| e.manifest_id == "priv-c"));

    // The private share is still reachable through the keyed query.
    assert_eq!(browser.seeders("priv-c").await.unwrap().len(), 1);
}

#[tokio::test]
async fn re_announce_refreshes_rather_than_duplicating() {
    let base = serve(TrackerRegistry::new()).await;
    let client = TrackerClient::new(&base);
    client.announce("s", &info("p")).await.unwrap();
    client.announce("s", &info("p")).await.unwrap();
    assert_eq!(client.seeders("s").await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_withdrawn_seeder_stops_being_handed_out() {
    let base = serve(TrackerRegistry::new()).await;
    let client = TrackerClient::new(&base);
    client.announce("s", &info("a")).await.unwrap();
    client.announce("s", &info("b")).await.unwrap();

    client.withdraw("s", "a").await.unwrap();
    let ids: Vec<String> =
        client.seeders("s").await.unwrap().into_iter().map(|s| s.peer_id).collect();
    assert_eq!(ids, vec!["b".to_string()]);

    // Withdrawing something unknown is a no-op, not an error.
    client.withdraw("s", "never-here").await.unwrap();
}
