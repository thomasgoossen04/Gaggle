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
use axum::routing::{delete, post};

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

const JSON: &[(&str, &str)] = &[("content-type", "application/json")];

struct Seeder {
    info: PeerInfo,
    refreshed_at: u64,
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
        if seeder.peer_id.is_empty() {
            return;
        }
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
        entries.insert(seeder.peer_id.clone(), Seeder { info: seeder, refreshed_at: unix_now() });
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
        .route("/tracker/{manifest_id}", post(announce_handler).get(seeders_handler))
        .route("/tracker/{manifest_id}/{peer_id}", delete(withdraw_handler))
        .with_state(registry)
}

async fn announce_handler(
    State(registry): State<TrackerRegistry>,
    Path(manifest_id): Path<String>,
    body: Result<axum::Json<PeerInfo>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(axum::Json(seeder)) = body else {
        return (StatusCode::BAD_REQUEST, "expected a PeerInfo body").into_response();
    };
    registry.announce(&manifest_id, seeder);
    StatusCode::NO_CONTENT.into_response()
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

    /// Publish (or refresh) `me` as a current seeder of `manifest_id`.
    pub async fn announce(&self, manifest_id: &str, me: &PeerInfo) -> anyhow::Result<()> {
        let url = format!("{}/tracker/{manifest_id}", self.base);
        let (status, _, _) = self.http.send("POST", &url, JSON, serde_json::to_vec(me)?).await?;
        anyhow::ensure!(status.is_success(), "tracker announce failed: {status}");
        Ok(())
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
    fn per_share_cap_evicts_the_least_recently_refreshed() {
        let reg = TrackerRegistry::new();
        for i in 0..(MAX_SEEDERS_PER_SHARE + 10) {
            reg.announce("s", info(&format!("p{i}")));
        }
        assert_eq!(reg.seeders("s").len(), MAX_SEEDERS_PER_SHARE);
    }
}
