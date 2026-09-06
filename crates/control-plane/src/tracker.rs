//! Seeder tracker — a lightweight, in-memory directory of which peers
//! currently serve a given share, so a downloader can pull from *every*
//! origin and replica at once instead of only the address baked into its
//! share link.
//!
//! Like [`rendezvous`](crate::rendezvous) this is unauthenticated signaling
//! that never sees chunk data or a share secret — only libp2p peer ids and
//! multiaddrs — and any already-running accelerator hosts it for free
//! ([`serve_daemon`](crate::serve_daemon) merges it onto the same listener,
//! next to the rendezvous mailbox). The two complement each other: the
//! tracker says *who* has a share; rendezvous helps punch a hole to one of
//! them.
//!
//! Flow, keyed by the share's manifest id (hex):
//!
//! 1. Every peer serving the share `POST /tracker/{manifest_id}`s its own
//!    [`PeerInfo`] every so often; an entry is dropped after [`ENTRY_TTL`]
//!    without a refresh, so a seed that goes away stops being handed out on
//!    its own.
//! 2. A downloader `GET /tracker/{manifest_id}` once before it starts and
//!    dials every address it gets back alongside the ones in its share link.
//! 3. A seed shutting down cleanly may `DELETE /tracker/{manifest_id}/{peer_id}`
//!    to leave immediately rather than waiting out the TTL.
//!
//! `GET /tracker` (no id) is the open **directory**: every *public* share the
//! tracker currently knows a live seeder for, with its human-readable name and
//! seeder count — so a downloader can browse and join a public share it was
//! never handed a link for. Invite-only shares announce with `private: true`
//! and are kept out of that listing (their invite is still the only way in),
//! but invite holders still swarm across every replica via step 2.
//!
//! Entries are in memory only; this is a discovery hint, not a source of
//! truth — every chunk a discovered peer serves is still verified against
//! the manifest root exactly as one from the link would be.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use gaggle_core::{AgentId, Signature};
use serde::{Deserialize, Serialize};

use crate::rendezvous::PeerInfo;

/// How long a seeder entry survives without a refresh. Generous relative to
/// the ~30 s re-announce cadence a seed uses, so one missed announce (a
/// transient network blip) doesn't drop a live seed from the directory.
const ENTRY_TTL: Duration = Duration::from_secs(150);
/// Upper bound on distinct seeders tracked per share; the least-recently
/// refreshed is evicted past this. A swarm larger than this doesn't need a
/// tracker to find peers — the DHT/mDNS/relay paths carry it.
const MAX_SEEDERS_PER_SHARE: usize = 128;
/// Upper bound on distinct shares tracked at all, FIFO-evicted.
const MAX_SHARES: usize = 100_000;
/// How far a signed announce's `signed_at` may be from the tracker's clock.
/// Generous — this only needs to bound signature replay, and a seeder
/// re-announces every ~30 s anyway.
const ANNOUNCE_MAX_SKEW_SECS: i64 = 300;

const JSON: &[(&str, &str)] = &[("content-type", "application/json")];

const ANNOUNCE_DOMAIN: &str = "gaggle-tracker-announce-v1";

/// The exact bytes a seeder signs with its libp2p Ed25519 identity key to prove
/// it controls `peer_id` before the tracker will hand that peer's address out.
/// `addrs` is sorted so the seeder and the tracker agree regardless of order.
pub fn announce_signing_bytes(
    manifest_id: &str,
    peer_id: &str,
    addrs: &[String],
    signed_at: u64,
) -> Vec<u8> {
    let mut addrs = addrs.to_vec();
    addrs.sort();
    format!(
        "{ANNOUNCE_DOMAIN}\n{manifest_id}\n{peer_id}\n{}\n{signed_at}",
        addrs.join(",")
    )
    .into_bytes()
}

/// Pull the raw Ed25519 public key out of a libp2p peer id string. Gaggle nodes
/// all use Ed25519 identities, whose protobuf-encoded public key (36 bytes) is
/// short enough that libp2p wraps it in an *identity* multihash — so the peer id
/// literally contains the key: `bs58( 0x00 0x24 | 0x08 0x01 0x12 0x20 | key32 )`.
/// Returns `None` for anything else (a hashed multihash, a non-Ed25519 key).
fn ed25519_pubkey_from_peer_id(peer_id: &str) -> Option<[u8; 32]> {
    let mh = bs58::decode(peer_id).into_vec().ok()?;
    // identity-hash (0x00), length 0x24 = 36, then protobuf PublicKey:
    //   field 1 (type) = 1 (Ed25519)  -> 0x08 0x01
    //   field 2 (data), len 32        -> 0x12 0x20
    const PREFIX: [u8; 6] = [0x00, 0x24, 0x08, 0x01, 0x12, 0x20];
    if mh.len() == PREFIX.len() + 32 && mh[..PREFIX.len()] == PREFIX {
        mh[PREFIX.len()..].try_into().ok()
    } else {
        None
    }
}

fn hex64(bytes: &[u8; 64]) -> String {
    let mut s = String::with_capacity(128);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn unhex64(s: &str) -> Option<[u8; 64]> {
    let s = s.trim();
    if s.len() != 128 {
        return None;
    }
    let mut out = [0u8; 64];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// Check a signed announce: the peer id must be an Ed25519 identity, the
/// timestamp fresh, and the signature valid over [`announce_signing_bytes`]
/// under the key the peer id embeds.
fn verify_announce(manifest_id: &str, a: &SeederAnnounce) -> Result<(), &'static str> {
    let now = unix_now() as i64;
    if (now - a.signed_at as i64).abs() > ANNOUNCE_MAX_SKEW_SECS {
        return Err("announce timestamp is outside the accepted window");
    }
    let pubkey = ed25519_pubkey_from_peer_id(&a.peer.peer_id)
        .ok_or("peer id is not a self-describing Ed25519 identity")?;
    let sig = unhex64(&a.signature).ok_or("signature is not 128 hex chars")?;
    let msg = announce_signing_bytes(manifest_id, &a.peer.peer_id, &a.peer.addrs, a.signed_at);
    AgentId::from_bytes(pubkey)
        .verify(&msg, &Signature::from_bytes(sig))
        .map_err(|_| "announce signature does not verify")
}

/// The body of a `POST /tracker/{manifest_id}` — a seeder's [`PeerInfo`] plus
/// the two bits of share metadata the open [directory](TrackerRegistry::directory)
/// listing needs. `name` / `private` are `#[serde(default)]` and the peer
/// fields are flattened in, so an older caller that only cares about the
/// who-serves-what half can still `POST` a bare `PeerInfo` and it deserializes
/// with `name: None`, `private: false`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeederAnnounce {
    #[serde(flatten)]
    pub peer: PeerInfo,
    /// Human-readable share name, for the directory listing. The last
    /// non-empty value announced for a share wins.
    #[serde(default)]
    pub name: Option<String>,
    /// `true` for an invite-only share — such shares are tracked (so invite
    /// holders still swarm across every replica) but never appear in the open
    /// [directory](TrackerRegistry::directory).
    #[serde(default)]
    pub private: bool,
    /// Unix seconds when this announce was signed. Bounds signature replay.
    #[serde(default)]
    pub signed_at: u64,
    /// Hex Ed25519 signature (128 chars) over [`announce_signing_bytes`], made
    /// with the libp2p identity key whose public half is embedded in
    /// `peer.peer_id`. The HTTP endpoint rejects an announce without a valid
    /// one — that is what stops a stranger announcing a victim's address as a
    /// seeder (a traffic-reflection vector).
    #[serde(default)]
    pub signature: String,
}

/// One row of the open share [directory](TrackerRegistry::directory): a public
/// share some peer is currently serving, discoverable without a share link.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareDirEntry {
    /// Manifest id, hex — the key to `GET /tracker/{manifest_id}` for the
    /// actual seeder addresses, and to a `SubscribeRequest`.
    pub manifest_id: String,
    /// Human-readable name, or empty if no announce carried one.
    pub name: String,
    /// How many distinct peers are serving it right now.
    pub seeders: usize,
}

struct Seeder {
    info: PeerInfo,
    refreshed_at: u64,
    name: Option<String>,
    private: bool,
}

#[derive(Default)]
struct Store {
    /// `manifest id hex -> peer id -> Seeder`.
    by_share: HashMap<String, HashMap<String, Seeder>>,
    /// Share insertion order, for FIFO eviction past [`MAX_SHARES`].
    share_order: VecDeque<String>,
}

/// In-memory seeder directory, shared by the [`router`].
#[derive(Clone, Default)]
pub struct TrackerRegistry {
    inner: Arc<Mutex<Store>>,
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

impl TrackerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record (or refresh) `seeder` as a current source for `share`. A
    /// refresh of an already-known peer id never counts against the
    /// per-share cap.
    pub fn announce(&self, share: &str, seeder: PeerInfo) {
        self.announce_with_meta(share, seeder, None, false);
    }

    /// Like [`announce`](Self::announce) but also records the share's
    /// human-readable `name` and whether it is `private` — the two fields the
    /// open [`directory`](Self::directory) listing needs.
    pub fn announce_with_meta(
        &self,
        share: &str,
        seeder: PeerInfo,
        name: Option<String>,
        private: bool,
    ) {
        if seeder.peer_id.is_empty() {
            return;
        }
        let name = name.filter(|n| !n.is_empty());
        let mut store = self.inner.lock().unwrap();
        prune(&mut store);

        if !store.by_share.contains_key(share) {
            while store.share_order.len() >= MAX_SHARES {
                match store.share_order.pop_front() {
                    Some(old) => {
                        store.by_share.remove(&old);
                    }
                    None => break,
                }
            }
            store.share_order.push_back(share.to_string());
        }

        let entries = store.by_share.entry(share.to_string()).or_default();
        if !entries.contains_key(&seeder.peer_id)
            && entries.len() >= MAX_SEEDERS_PER_SHARE
            && let Some(oldest) =
                entries.iter().min_by_key(|(_, s)| s.refreshed_at).map(|(k, _)| k.clone())
        {
            entries.remove(&oldest);
        }
        entries.insert(
            seeder.peer_id.clone(),
            Seeder { info: seeder, refreshed_at: unix_now(), name, private },
        );
    }

    /// Every *public* share the tracker currently knows a live seeder for,
    /// name-then-id ordered. Invite-only shares are omitted — discovering one
    /// still needs its invite. A discovery hint only: chunks a discovered
    /// source serves are verified against the manifest root regardless.
    pub fn directory(&self) -> Vec<ShareDirEntry> {
        let mut store = self.inner.lock().unwrap();
        prune(&mut store);
        let mut out: Vec<ShareDirEntry> = store
            .by_share
            .iter()
            .filter(|(_, entries)| !entries.values().any(|s| s.private))
            .filter(|(_, entries)| !entries.is_empty())
            .map(|(id, entries)| ShareDirEntry {
                manifest_id: id.clone(),
                name: entries
                    .values()
                    .filter_map(|s| s.name.clone())
                    .next()
                    .unwrap_or_default(),
                seeders: entries.len(),
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.manifest_id.cmp(&b.manifest_id)));
        out
    }

    /// The seeders currently known for `share`, freshest announce first.
    pub fn seeders(&self, share: &str) -> Vec<PeerInfo> {
        let mut store = self.inner.lock().unwrap();
        prune(&mut store);
        let mut list: Vec<(u64, PeerInfo)> = store
            .by_share
            .get(share)
            .map(|e| e.values().map(|s| (s.refreshed_at, s.info.clone())).collect())
            .unwrap_or_default();
        list.sort_by_key(|(refreshed_at, _)| std::cmp::Reverse(*refreshed_at));
        list.into_iter().map(|(_, info)| info).collect()
    }

    /// Drop one seeder immediately (a clean shutdown). A missing entry is
    /// not an error — the TTL would have taken it anyway.
    pub fn withdraw(&self, share: &str, peer_id: &str) {
        let mut store = self.inner.lock().unwrap();
        prune(&mut store);
        if let Some(entries) = store.by_share.get_mut(share) {
            entries.remove(peer_id);
        }
    }
}

/// Drop entries older than [`ENTRY_TTL`], then any share left with no
/// seeders. Called on every access — this is a low-traffic hint endpoint,
/// not the data plane.
fn prune(store: &mut Store) {
    let cutoff = unix_now().saturating_sub(ENTRY_TTL.as_secs());
    store.by_share.retain(|_, entries| {
        entries.retain(|_, s| s.refreshed_at >= cutoff);
        !entries.is_empty()
    });
    let by_share = &store.by_share;
    store.share_order.retain(|k| by_share.contains_key(k));
}

/// `POST /tracker/{manifest_id}` (announce/refresh a seeder),
/// `GET /tracker/{manifest_id}` (list seeders), and
/// `DELETE /tracker/{manifest_id}/{peer_id}` (withdraw one). Deliberately
/// unauthenticated — like [`rendezvous::router`](crate::rendezvous::router),
/// any peer reaching one of this daemon's shares may need it — and it only
/// ever carries ephemeral network addresses, never a share secret or chunk
/// data.
pub fn router(registry: TrackerRegistry) -> Router {
    Router::new()
        .route("/tracker", get(directory_handler))
        .route("/tracker/{manifest_id}", post(announce_handler).get(seeders_handler))
        .route("/tracker/{manifest_id}/{peer_id}", delete(withdraw_handler))
        .with_state(registry)
}

async fn announce_handler(
    State(registry): State<TrackerRegistry>,
    Path(manifest_id): Path<String>,
    body: Result<axum::Json<SeederAnnounce>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(axum::Json(a)) = body else {
        return (StatusCode::BAD_REQUEST, "expected a SeederAnnounce body").into_response();
    };
    // The one authenticated step in an otherwise-open service: prove control of
    // the announced peer id, so nobody can list a third party's address as a
    // seeder and turn every downloader into a packet source aimed at them.
    if let Err(why) = verify_announce(&manifest_id, &a) {
        return (StatusCode::UNAUTHORIZED, why).into_response();
    }
    registry.announce_with_meta(&manifest_id, a.peer, a.name, a.private);
    StatusCode::NO_CONTENT.into_response()
}

async fn directory_handler(State(registry): State<TrackerRegistry>) -> Response {
    axum::Json(registry.directory()).into_response()
}

async fn seeders_handler(
    State(registry): State<TrackerRegistry>,
    Path(manifest_id): Path<String>,
) -> Response {
    axum::Json(registry.seeders(&manifest_id)).into_response()
}

async fn withdraw_handler(
    State(registry): State<TrackerRegistry>,
    Path((manifest_id, peer_id)): Path<(String, String)>,
) -> Response {
    registry.withdraw(&manifest_id, &peer_id);
    StatusCode::NO_CONTENT.into_response()
}

/// Serve the [`router`] on `listener` until the process ends — a thin
/// wrapper so callers (mostly tests) need not depend on `axum` directly.
pub async fn serve(listener: tokio::net::TcpListener, registry: TrackerRegistry) -> anyhow::Result<()> {
    axum::serve(listener, router(registry)).await?;
    Ok(())
}

/// HTTP(S) client for the [`router`] endpoints. Talks TLS like the rest of
/// the daemon API on the same port but — like
/// [`RendezvousClient`](crate::rendezvous::RendezvousClient) — pins nothing:
/// the tracker is unauthenticated by design.
pub struct TrackerClient {
    base: String,
    http: crate::http_client::HttpClient,
}

impl TrackerClient {
    /// `base` is the accelerator's control-plane origin, e.g.
    /// `https://accelerator.example:8749` (a bare `host:port` is accepted and
    /// normalized to `https://` — see [`crate::admin::normalize_base`]).
    pub fn new(base: impl Into<String>) -> Self {
        let tls = crate::tls::rendezvous_client_config()
            .expect("fixed rustls client config is always constructible");
        Self {
            base: crate::admin::normalize_base(&base.into()),
            http: crate::http_client::HttpClient::new(tls),
        }
    }

    /// Publish (or refresh) `me` as a current seeder of `manifest_id`, with no
    /// share metadata — the share never shows up in the open [`directory`].
    ///
    /// `sign` must produce a raw 64-byte Ed25519 signature over its argument
    /// using the identity key behind `me.peer_id` (in `net`, that is
    /// `Node::sign_identity` / `RelayNode::sign_identity`). The tracker rejects
    /// an announce whose signature does not check out against the key its peer
    /// id embeds.
    pub async fn announce(
        &self,
        manifest_id: &str,
        me: &PeerInfo,
        sign: impl Fn(&[u8]) -> [u8; 64],
    ) -> anyhow::Result<()> {
        self.announce_share(manifest_id, me, None, false, sign).await
    }

    /// Publish (or refresh) `me` as a current seeder of `manifest_id`, tagging
    /// it with a human-readable `name` and whether it is `private`. A public
    /// share announced this way is discoverable through [`directory`](Self::directory)
    /// with no share link. See [`announce`](Self::announce) for `sign`.
    pub async fn announce_share(
        &self,
        manifest_id: &str,
        me: &PeerInfo,
        name: Option<&str>,
        private: bool,
        sign: impl Fn(&[u8]) -> [u8; 64],
    ) -> anyhow::Result<()> {
        let url = format!("{}/tracker/{manifest_id}", self.base);
        let signed_at =
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        let msg = announce_signing_bytes(manifest_id, &me.peer_id, &me.addrs, signed_at);
        let signature = hex64(&sign(&msg));
        let body = SeederAnnounce {
            peer: me.clone(),
            name: name.map(str::to_string),
            private,
            signed_at,
            signature,
        };
        let (status, _, _) =
            self.http.send("POST", &url, JSON, serde_json::to_vec(&body)?).await?;
        anyhow::ensure!(status.is_success(), "tracker announce failed: {status}");
        Ok(())
    }

    /// Every public share the tracker currently knows a live seeder for.
    pub async fn directory(&self) -> anyhow::Result<Vec<ShareDirEntry>> {
        let url = format!("{}/tracker", self.base);
        let (status, _, bytes) = self.http.send("GET", &url, &[], Vec::new()).await?;
        anyhow::ensure!(status.is_success(), "tracker directory query failed: {status}");
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// The seeders the tracker currently knows for `manifest_id`.
    pub async fn seeders(&self, manifest_id: &str) -> anyhow::Result<Vec<PeerInfo>> {
        let url = format!("{}/tracker/{manifest_id}", self.base);
        let (status, _, bytes) = self.http.send("GET", &url, &[], Vec::new()).await?;
        anyhow::ensure!(status.is_success(), "tracker query failed: {status}");
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// Withdraw `peer_id` from `manifest_id`'s seeder list (a clean shutdown).
    pub async fn withdraw(&self, manifest_id: &str, peer_id: &str) -> anyhow::Result<()> {
        let url = format!("{}/tracker/{manifest_id}/{peer_id}", self.base);
        let (status, _, _) = self.http.send("DELETE", &url, &[], Vec::new()).await?;
        anyhow::ensure!(status.is_success(), "tracker withdraw failed: {status}");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: &str) -> PeerInfo {
        PeerInfo {
            peer_id: id.to_string(),
            addrs: vec![format!("/ip4/203.0.113.1/udp/4001/quic-v1/p2p/{id}")],
        }
    }

    #[test]
    fn announce_then_list_round_trips() {
        let reg = TrackerRegistry::new();
        reg.announce("share1", info("origin"));
        reg.announce("share1", info("replica"));

        let mut ids: Vec<String> =
            reg.seeders("share1").into_iter().map(|s| s.peer_id).collect();
        ids.sort();
        assert_eq!(ids, vec!["origin".to_string(), "replica".to_string()]);
        assert!(reg.seeders("other").is_empty());
    }

    #[test]
    fn re_announce_refreshes_without_duplicating() {
        let reg = TrackerRegistry::new();
        reg.announce("s", info("p"));
        reg.announce("s", info("p"));
        assert_eq!(reg.seeders("s").len(), 1);
    }

    /// A libp2p-style peer id string for a raw Ed25519 public key: the identity
    /// multihash of the protobuf-wrapped key, base58btc-encoded.
    fn peer_id_for(pubkey: &[u8; 32]) -> String {
        let mut buf = vec![0x00u8, 0x24, 0x08, 0x01, 0x12, 0x20];
        buf.extend_from_slice(pubkey);
        bs58::encode(buf).into_string()
    }

    #[test]
    fn a_signed_announce_verifies_and_tampering_is_caught() {
        use gaggle_core::AgentKeypair;
        let kp = AgentKeypair::from_seed([7u8; 32]);
        let peer_id = peer_id_for(kp.public().as_bytes());
        let addrs = vec![format!("/ip4/203.0.113.9/udp/4001/quic-v1/p2p/{peer_id}")];
        let signed_at = unix_now();

        let msg = announce_signing_bytes("mani-1", &peer_id, &addrs, signed_at);
        let sig = hex64(&kp.sign(&msg).to_bytes());

        let good = SeederAnnounce {
            peer: PeerInfo { peer_id: peer_id.clone(), addrs: addrs.clone() },
            name: None,
            private: false,
            signed_at,
            signature: sig.clone(),
        };
        assert!(verify_announce("mani-1", &good).is_ok());

        // Wrong manifest id, swapped address, and a future timestamp each fail.
        assert!(verify_announce("mani-2", &good).is_err());
        let mut moved = good.clone();
        moved.peer.addrs = vec!["/ip4/198.51.100.1/udp/4001/quic-v1".into()];
        assert!(verify_announce("mani-1", &moved).is_err());
        let mut stale = good.clone();
        stale.signed_at = signed_at + (ANNOUNCE_MAX_SKEW_SECS as u64) + 10;
        assert!(verify_announce("mani-1", &stale).is_err());

        // A different key's signature for the same peer id is rejected.
        let other = AgentKeypair::from_seed([9u8; 32]);
        let mut forged = good.clone();
        forged.signature = hex64(&other.sign(&msg).to_bytes());
        assert!(verify_announce("mani-1", &forged).is_err());
    }

    #[test]
    fn a_bare_peer_id_string_has_no_embedded_key() {
        assert!(ed25519_pubkey_from_peer_id("origin-peer").is_none());
        assert!(ed25519_pubkey_from_peer_id("12D3KooWnotreal").is_none());
    }

    #[test]
    fn withdraw_removes_a_seeder_and_empties_the_share() {
        let reg = TrackerRegistry::new();
        reg.announce("s", info("p"));
        reg.withdraw("s", "p");
        assert!(reg.seeders("s").is_empty());
        // The now-empty share is pruned from bookkeeping too.
        assert!(reg.inner.lock().unwrap().by_share.is_empty());
    }

    #[test]
    fn stale_entries_are_pruned_on_access() {
        let reg = TrackerRegistry::new();
        reg.announce("s", info("p"));
        {
            let mut store = reg.inner.lock().unwrap();
            for entries in store.by_share.values_mut() {
                for e in entries.values_mut() {
                    e.refreshed_at = 0;
                }
            }
        }
        assert!(reg.seeders("s").is_empty());
    }

    #[test]
    fn directory_lists_public_shares_only() {
        let reg = TrackerRegistry::new();
        reg.announce_with_meta("pub1", info("a"), Some("Alpha".into()), false);
        reg.announce_with_meta("pub1", info("b"), None, false);
        reg.announce_with_meta("pub2", info("c"), Some("Bravo".into()), false);
        reg.announce_with_meta("priv1", info("d"), Some("Secret".into()), true);

        let dir = reg.directory();
        let names: Vec<&str> = dir.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["Alpha", "Bravo"]);
        let alpha = dir.iter().find(|e| e.name == "Alpha").unwrap();
        assert_eq!(alpha.manifest_id, "pub1");
        assert_eq!(alpha.seeders, 2);
        assert!(!dir.iter().any(|e| e.manifest_id == "priv1"));
    }

    #[test]
    fn a_private_announce_hides_an_otherwise_public_share() {
        let reg = TrackerRegistry::new();
        reg.announce_with_meta("s", info("a"), Some("Name".into()), false);
        reg.announce_with_meta("s", info("b"), Some("Name".into()), true);
        assert!(reg.directory().is_empty());
        // …but invite holders still get every seeder from the keyed query.
        assert_eq!(reg.seeders("s").len(), 2);
    }

    #[test]
    fn per_share_cap_evicts_the_least_recently_refreshed() {
        let reg = TrackerRegistry::new();
        for i in 0..(MAX_SEEDERS_PER_SHARE + 10) {
            reg.announce("s", info(&format!("p{i}")));
        }
        assert_eq!(reg.seeders("s").len(), MAX_SEEDERS_PER_SHARE);
    }
}
