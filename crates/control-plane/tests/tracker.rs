//! The seeder-tracker endpoints round-trip announce / list / withdraw
//! through a live HTTP server, exactly as an origin, a NAS replica, and a
//! downloader would use them. Announces are signed with the libp2p Ed25519
//! identity key behind the announced peer id; the server refuses any that
//! aren't (that is what stops a stranger listing a victim's address).

use control_plane::{PeerInfo, TrackerClient, TrackerRegistry, tracker_router};
use gaggle_core::AgentKeypair;

async fn serve(registry: TrackerRegistry) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, tracker_router(registry)).await.unwrap();
    });
    format!("http://{addr}")
}

/// A test seeder: a real Ed25519 keypair, the libp2p-style peer id string that
/// embeds its public key, a matching `PeerInfo`, and a signing closure.
struct Seeder {
    kp: AgentKeypair,
    info: PeerInfo,
}

impl Seeder {
    fn new(seed: u8) -> Self {
        let kp = AgentKeypair::from_seed([seed; 32]);
        let mut mh = vec![0x00u8, 0x24, 0x08, 0x01, 0x12, 0x20];
        mh.extend_from_slice(kp.public().as_bytes());
        let peer_id = bs58::encode(mh).into_string();
        let info = PeerInfo {
            peer_id: peer_id.clone(),
            addrs: vec![format!("/ip4/203.0.113.7/udp/4001/quic-v1/p2p/{peer_id}")],
        };
        Self { kp, info }
    }

    fn sign(&self) -> impl Fn(&[u8]) -> [u8; 64] + '_ {
        move |m| self.kp.sign(m).to_bytes()
    }
}

#[tokio::test]
async fn a_downloader_discovers_every_announced_seeder() {
    let base = serve(TrackerRegistry::new()).await;
    let client = TrackerClient::new(&base);
    let origin = Seeder::new(1);
    let replica = Seeder::new(2);

    client.announce("share-1", &origin.info, origin.sign()).await.unwrap();
    client.announce("share-1", &replica.info, replica.sign()).await.unwrap();

    let mut ids: Vec<String> = client
        .seeders("share-1")
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.peer_id)
        .collect();
    ids.sort();
    let mut want = vec![origin.info.peer_id.clone(), replica.info.peer_id.clone()];
    want.sort();
    assert_eq!(ids, want);

    // A share nobody announced is just empty, not an error.
    assert!(client.seeders("share-2").await.unwrap().is_empty());
}

#[tokio::test]
async fn an_unsigned_or_forged_announce_is_refused() {
    let base = serve(TrackerRegistry::new()).await;
    let victim = Seeder::new(7);

    // Raw POST with no signature at all.
    let http = reqwest::Client::new();
    let body = serde_json::json!({
        "peer_id": victim.info.peer_id,
        "addrs": victim.info.addrs,
    });
    let resp = http
        .post(format!("{base}/tracker/share-x"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "an unsigned announce must be rejected");

    // A signature from a *different* key over the victim's peer id.
    let attacker = Seeder::new(8);
    let client = TrackerClient::new(&base);
    let forged = client
        .announce("share-x", &victim.info, attacker.sign())
        .await;
    assert!(forged.is_err(), "a signature from the wrong key must be rejected");

    assert!(client.seeders("share-x").await.unwrap().is_empty());
}

#[tokio::test]
async fn the_open_directory_lists_public_shares_by_name() {
    let base = serve(TrackerRegistry::new()).await;
    let client = TrackerClient::new(&base);
    let a1 = Seeder::new(11);
    let a2 = Seeder::new(12);
    let b1 = Seeder::new(13);
    let c1 = Seeder::new(14);

    client.announce_share("pub-a", &a1.info, Some("Skyrim Modpack"), false, a1.sign()).await.unwrap();
    client.announce_share("pub-a", &a2.info, Some("Skyrim Modpack"), false, a2.sign()).await.unwrap();
    client.announce_share("pub-b", &b1.info, Some("Blender Assets"), false, b1.sign()).await.unwrap();
    // A private share is tracked (invite holders still swarm) but never listed.
    client.announce_share("priv-c", &c1.info, Some("Private Backup"), true, c1.sign()).await.unwrap();

    let dir = client.directory().await.unwrap();
    let names: Vec<&str> = dir.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["Blender Assets", "Skyrim Modpack"]);

    let a = dir.iter().find(|e| e.manifest_id == "pub-a").unwrap();
    assert_eq!(a.name, "Skyrim Modpack");
    assert_eq!(a.seeders, 2);
    assert!(!dir.iter().any(|e| e.manifest_id == "priv-c"));

    // The private share is still reachable through the keyed query.
    assert_eq!(client.seeders("priv-c").await.unwrap().len(), 1);
}

#[tokio::test]
async fn re_announce_refreshes_rather_than_duplicating() {
    let base = serve(TrackerRegistry::new()).await;
    let client = TrackerClient::new(&base);
    let p = Seeder::new(21);
    client.announce("s", &p.info, p.sign()).await.unwrap();
    client.announce("s", &p.info, p.sign()).await.unwrap();
    assert_eq!(client.seeders("s").await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_withdrawn_seeder_stops_being_handed_out() {
    let base = serve(TrackerRegistry::new()).await;
    let client = TrackerClient::new(&base);
    let a = Seeder::new(31);
    let b = Seeder::new(32);
    client.announce("s", &a.info, a.sign()).await.unwrap();
    client.announce("s", &b.info, b.sign()).await.unwrap();

    client.withdraw("s", &a.info.peer_id).await.unwrap();
    let ids: Vec<String> =
        client.seeders("s").await.unwrap().into_iter().map(|s| s.peer_id).collect();
    assert_eq!(ids, vec![b.info.peer_id.clone()]);

    // Withdrawing something unknown is a no-op, not an error.
    client.withdraw("s", "never-here").await.unwrap();
}
