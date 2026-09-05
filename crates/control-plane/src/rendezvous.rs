//! NAT-traversal rendezvous — the "ICE-lite" signaling half of a relay-free
//! direct connection.
//!
//! Two peers that are each behind NAT can still connect to each other
//! directly (no data ever flows through this service, unlike the libp2p
//! relay role) if they each learn the other's currently-reachable candidate
//! addresses and dial them at close to the same moment — that second outbound
//! packet is what opens each side's own NAT pinhole for the other's inbound
//! one. This module is just the address exchange: an already-running
//! accelerator hosts it (unauthenticated — any peer trying to reach *any*
//! share on that accelerator's network may need it, not just the operator),
//! and it never sees chunk data or a share's contents, only libp2p peer ids
//! and multiaddrs.
//!
//! Flow, keyed by the *origin*'s (the side being connected to) libp2p peer id
//! so a subscriber that has never talked to the origin before still knows
//! where to knock:
//!
//! 1. Subscriber `POST /rendezvous/{origin}` with its own [`PeerInfo`] →
//!    gets back a `request_id`.
//! 2. Origin polls `GET /rendezvous/{origin}/pending`, sees the request,
//!    dials the subscriber's addresses itself (the actual punch) and
//!    `POST /rendezvous/{origin}/{request_id}/answer`s with its own
//!    [`PeerInfo`].
//! 3. Subscriber polls `GET /rendezvous/{origin}/{request_id}` until an
//!    answer appears, then dials the origin's addresses.
//!
//! Entries are held in memory only, for [`ENTRY_TTL`] — long enough for both
//! sides' short poll loops to meet, short enough that a abandoned request
//! doesn't linger.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};

/// How long an entry is kept without an answer (or without being polled)
/// before it is treated as abandoned. Generous relative to the few-second
/// poll intervals both sides use — this bounds memory, not the punch timing.
const ENTRY_TTL: Duration = Duration::from_secs(60);
/// Upper bound on requests held per origin, FIFO-evicted — an origin that
/// never polls (e.g. isn't actually online) must not let requests accumulate
/// without limit.
const MAX_PER_ORIGIN: usize = 256;
/// Upper bound on distinct origins tracked at all, same FIFO reasoning.
const MAX_ORIGINS: usize = 10_000;

/// A peer's libp2p identity and the addresses it can currently be dialed on,
/// stringified so this crate need not depend on `net`/`libp2p` types.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    pub peer_id: String,
    pub addrs: Vec<String>,
}

/// One pending request, as returned by the `pending` listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingRequest {
    pub request_id: String,
    pub subscriber: PeerInfo,
}

/// The full state of one request, as returned by the subscriber's poll.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestState {
    pub subscriber: PeerInfo,
    pub answer: Option<PeerInfo>,
}

struct Entry {
    subscriber: PeerInfo,
    answer: Option<PeerInfo>,
    created_at: u64,
}

#[derive(Default)]
struct Store {
    /// `origin peer id hex -> request id -> Entry`, plus per-origin insertion
    /// order for FIFO eviction.
    by_origin: HashMap<String, HashMap<String, Entry>>,
    order: HashMap<String, std::collections::VecDeque<String>>,
    origin_order: std::collections::VecDeque<String>,
}

/// In-memory rendezvous mailbox, shared by the router.
#[derive(Clone, Default)]
pub struct RendezvousRegistry {
    inner: Arc<Mutex<Store>>,
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

impl RendezvousRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a fresh request under `origin`; returns its request id.
    pub fn register(&self, origin: &str, subscriber: PeerInfo) -> String {
        let request_id = random_id();
        let mut store = self.inner.lock().unwrap();
        prune(&mut store);

        if !store.by_origin.contains_key(origin) {
            while store.origin_order.len() >= MAX_ORIGINS {
                match store.origin_order.pop_front() {
                    Some(old) => {
                        store.by_origin.remove(&old);
                        store.order.remove(&old);
                    }
                    None => break,
                }
            }
            store.origin_order.push_back(origin.to_string());
        }

        let Store { by_origin, order, .. } = &mut *store;
        let entries = by_origin.entry(origin.to_string()).or_default();
        let order = order.entry(origin.to_string()).or_default();
        while order.len() >= MAX_PER_ORIGIN {
            match order.pop_front() {
                Some(old) => {
                    entries.remove(&old);
                }
                None => break,
            }
        }
        entries.insert(
            request_id.clone(),
            Entry { subscriber, answer: None, created_at: unix_now() },
        );
        order.push_back(request_id.clone());
        request_id
    }

    /// Requests under `origin` that have no answer yet.
    pub fn pending(&self, origin: &str) -> Vec<PendingRequest> {
        let mut store = self.inner.lock().unwrap();
        prune(&mut store);
        store
            .by_origin
            .get(origin)
            .map(|entries| {
                entries
                    .iter()
                    .filter(|(_, e)| e.answer.is_none())
                    .map(|(id, e)| PendingRequest {
                        request_id: id.clone(),
                        subscriber: e.subscriber.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Publish the origin's own address for `request_id`. `false` if the
    /// request doesn't exist (already expired, or never existed).
    pub fn answer(&self, origin: &str, request_id: &str, answer: PeerInfo) -> bool {
        let mut store = self.inner.lock().unwrap();
        prune(&mut store);
        match store.by_origin.get_mut(origin).and_then(|e| e.get_mut(request_id)) {
            Some(entry) => {
                entry.answer = Some(answer);
                true
            }
            None => false,
        }
    }

    /// The current state of `request_id`, if it still exists.
    pub fn get(&self, origin: &str, request_id: &str) -> Option<RequestState> {
        let mut store = self.inner.lock().unwrap();
        prune(&mut store);
        store.by_origin.get(origin).and_then(|e| e.get(request_id)).map(|e| RequestState {
            subscriber: e.subscriber.clone(),
            answer: e.answer.clone(),
        })
    }
}

/// Drop entries older than [`ENTRY_TTL`]. Called on every access rather than
/// on a timer — this is a low-traffic signaling endpoint, not the data plane.
fn prune(store: &mut Store) {
    let cutoff = unix_now().saturating_sub(ENTRY_TTL.as_secs());
    for (origin, entries) in store.by_origin.iter_mut() {
        entries.retain(|_, e| e.created_at >= cutoff);
        if let Some(order) = store.order.get_mut(origin) {
            order.retain(|id| entries.contains_key(id));
        }
    }
}

fn random_id() -> String {
    let mut bytes = [0u8; 12];
    getrandom::getrandom(&mut bytes).expect("system RNG unavailable");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `POST /rendezvous/{origin}`, `GET /rendezvous/{origin}/pending`,
/// `POST /rendezvous/{origin}/{request_id}/answer`,
/// `GET /rendezvous/{origin}/{request_id}`. Deliberately unauthenticated —
/// unlike the admin API, any subscriber may need this, not just the
/// accelerator's operator — and it only ever carries ephemeral network
/// addresses, never a share secret or chunk data.
pub fn router(registry: RendezvousRegistry) -> Router {
    Router::new()
        .route("/rendezvous/{origin}", post(register_handler))
        .route("/rendezvous/{origin}/pending", get(pending_handler))
        .route("/rendezvous/{origin}/{request_id}/answer", post(answer_handler))
        .route("/rendezvous/{origin}/{request_id}", get(get_handler))
        .with_state(registry)
}

async fn register_handler(
    State(registry): State<RendezvousRegistry>,
    Path(origin): Path<String>,
    body: Result<axum::Json<PeerInfo>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(axum::Json(subscriber)) = body else {
        return (StatusCode::BAD_REQUEST, "expected a PeerInfo body").into_response();
    };
    let request_id = registry.register(&origin, subscriber);
    axum::Json(serde_json::json!({ "request_id": request_id })).into_response()
}

async fn pending_handler(
    State(registry): State<RendezvousRegistry>,
    Path(origin): Path<String>,
) -> Response {
    axum::Json(registry.pending(&origin)).into_response()
}

async fn answer_handler(
    State(registry): State<RendezvousRegistry>,
    Path((origin, request_id)): Path<(String, String)>,
    body: Result<axum::Json<PeerInfo>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(axum::Json(answer)) = body else {
        return (StatusCode::BAD_REQUEST, "expected a PeerInfo body").into_response();
    };
    if registry.answer(&origin, &request_id, answer) {
        StatusCode::NO_CONTENT.into_response()
    } else {
        (StatusCode::NOT_FOUND, "no such request (expired?)").into_response()
    }
}

async fn get_handler(
    State(registry): State<RendezvousRegistry>,
    Path((origin, request_id)): Path<(String, String)>,
) -> Response {
    match registry.get(&origin, &request_id) {
        Some(state) => axum::Json(state).into_response(),
        None => (StatusCode::NOT_FOUND, "no such request (expired?)").into_response(),
    }
}

/// Serve the [`router`] on `listener` until the process ends. A thin wrapper
/// so callers need not depend on `axum` directly.
pub async fn serve(listener: tokio::net::TcpListener, registry: RendezvousRegistry) -> anyhow::Result<()> {
    axum::serve(listener, router(registry)).await?;
    Ok(())
}

/// HTTP(S) client for the [`router`] endpoints.
pub struct RendezvousClient {
    base: String,
    http: crate::http_client::HttpClient,
}

const JSON: &[(&str, &str)] = &[("content-type", "application/json")];

impl RendezvousClient {
    /// `base` is the accelerator's control-plane origin, e.g.
    /// `https://accelerator.example:8749` (a bare `host:port` is accepted too
    /// and normalized to `https://` — see [`crate::admin::normalize_base`]).
    /// This talks TLS like the admin API on the same port, but — unlike
    /// [`AdminClient`](crate::admin::AdminClient) — pins nothing: rendezvous
    /// is unauthenticated by design, so there's no operator identity to trust
    /// on first use here (see [`crate::tls::rendezvous_client_config`]).
    pub fn new(base: impl Into<String>) -> Self {
        let tls = crate::tls::rendezvous_client_config()
            .expect("fixed rustls client config is always constructible");
        Self { base: crate::admin::normalize_base(&base.into()), http: crate::http_client::HttpClient::new(tls) }
    }

    /// Subscriber side: register with `origin`, returning a request id to
    /// poll with [`poll_answer`](Self::poll_answer).
    pub async fn register(&self, origin: &str, me: &PeerInfo) -> anyhow::Result<String> {
        #[derive(Deserialize)]
        struct Registered {
            request_id: String,
        }
        let url = format!("{}/rendezvous/{origin}", self.base);
        let (status, _, bytes) = self.http.send("POST", &url, JSON, serde_json::to_vec(me)?).await?;
        anyhow::ensure!(status.is_success(), "rendezvous register failed: {status}");
        Ok(serde_json::from_slice::<Registered>(&bytes)?.request_id)
    }

    /// Subscriber side: poll for the origin's answer. `Ok(None)` while still
    /// pending; an error once the request has expired.
    pub async fn poll_answer(
        &self,
        origin: &str,
        request_id: &str,
    ) -> anyhow::Result<Option<PeerInfo>> {
        let url = format!("{}/rendezvous/{origin}/{request_id}", self.base);
        let (status, _, bytes) = self.http.send("GET", &url, &[], Vec::new()).await?;
        anyhow::ensure!(status.is_success(), "rendezvous poll failed: {status}");
        Ok(serde_json::from_slice::<RequestState>(&bytes)?.answer)
    }

    /// Origin side: requests waiting for this origin's address.
    pub async fn pending(&self, origin: &str) -> anyhow::Result<Vec<PendingRequest>> {
        let url = format!("{}/rendezvous/{origin}/pending", self.base);
        let (status, _, bytes) = self.http.send("GET", &url, &[], Vec::new()).await?;
        anyhow::ensure!(status.is_success(), "rendezvous pending failed: {status}");
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// Origin side: publish this origin's address for one pending request.
    pub async fn answer(&self, origin: &str, request_id: &str, me: &PeerInfo) -> anyhow::Result<()> {
        let url = format!("{}/rendezvous/{origin}/{request_id}/answer", self.base);
        let (status, _, _) = self.http.send("POST", &url, JSON, serde_json::to_vec(me)?).await?;
        anyhow::ensure!(status.is_success(), "rendezvous answer failed: {status}");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: &str) -> PeerInfo {
        PeerInfo { peer_id: id.to_string(), addrs: vec![format!("/ip4/1.2.3.4/udp/{id}/quic-v1")] }
    }

    #[test]
    fn register_then_answer_round_trips() {
        let reg = RendezvousRegistry::new();
        let id = reg.register("origin1", info("sub1"));

        let pending = reg.pending("origin1");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].request_id, id);
        assert_eq!(pending[0].subscriber.peer_id, "sub1");

        assert!(reg.answer("origin1", &id, info("origin1")));
        // Answered requests drop out of `pending`.
        assert!(reg.pending("origin1").is_empty());

        let state = reg.get("origin1", &id).unwrap();
        assert_eq!(state.subscriber.peer_id, "sub1");
        assert_eq!(state.answer.unwrap().peer_id, "origin1");
    }

    #[test]
    fn answering_an_unknown_request_fails() {
        let reg = RendezvousRegistry::new();
        assert!(!reg.answer("origin1", "no-such-id", info("origin1")));
    }

    #[test]
    fn expired_entries_are_pruned_on_access() {
        let reg = RendezvousRegistry::new();
        let id = reg.register("origin1", info("sub1"));
        {
            let mut store = reg.inner.lock().unwrap();
            for entries in store.by_origin.values_mut() {
                for e in entries.values_mut() {
                    e.created_at = 0;
                }
            }
        }
        assert!(reg.get("origin1", &id).is_none());
        assert!(reg.pending("origin1").is_empty());
    }
}
