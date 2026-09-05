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
