//! Admin control API for a remote accelerator daemon.
//!
//! A daemon ([`crate::admin::router`]) exposes its status and lets an authorised
//! operator add / remove which shares it accelerates. Every request is signed by
//! the operator's Ed25519 key ([`gaggle_core::AgentKeypair`]) and checked
//! against the daemon's `authorized` set; every response is signed by the
//! daemon's own key so a client can pin it on first contact (TOFU). The whole
//! exchange runs over TLS (see [`crate::tls`]), terminated with a self-signed
//! certificate derived from that same daemon key, so [`AdminClient`] pins one
//! identity that governs both layers instead of trusting a CA.
//!
//! The router is deliberately transport-only: it forwards mutations to the
//! daemon over an [`mpsc`] channel and reads status from a [`watch`] channel, so
//! `control-plane` never has to depend on `net`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, HeaderName, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get};
use base64::Engine;
use gaggle_core::{AgentId, AgentKeypair, Hash, Signature};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, watch};

/// Header names for the signed-request scheme.
pub const H_AGENT: &str = "x-gaggle-agent";
pub const H_TIMESTAMP: &str = "x-gaggle-timestamp";
pub const H_NONCE: &str = "x-gaggle-nonce";
pub const H_SIGNATURE: &str = "x-gaggle-signature";
pub const H_DAEMON: &str = "x-gaggle-daemon";
pub const H_DAEMON_SIGNATURE: &str = "x-gaggle-daemon-signature";

/// Requests older than this (clock skew) are rejected.
const MAX_SKEW_SECS: i64 = 60;
const MAX_BODY: usize = 64 * 1024;
/// Cap on the in-flight `(agent, nonce)` replay cache. Entries expire after the
/// skew window, so this is only reached under a flood; hitting it fails closed.
const MAX_SEEN_NONCES: usize = 100_000;

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

/// `#[serde(default)]` for [`ShareStatus::seeding`] — a reported share is a
/// served one unless the daemon says otherwise.
fn serde_true() -> bool {
    true
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn unb64(s: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s.as_bytes()).ok()
}

/// Accept a bare `host:port` (or `//host:port`) and turn it into an absolute
/// `https://…` origin with no trailing slash, so a pasted `127.0.0.1:8749`
/// works. The admin API is TLS-only (see [`crate::tls`]); an explicit
/// `http://` is left as-is rather than silently upgraded, since a caller who
/// typed that scheme deliberately gets a connection error, not a
/// downgrade-without-noticing.
pub fn normalize_base(input: &str) -> String {
    let s = input.trim().trim_end_matches('/');
    if s.starts_with("http://") || s.starts_with("https://") {
        s.to_string()
    } else {
        format!("https://{}", s.trim_start_matches("//"))
    }
}

/// The bytes an operator signs / a daemon verifies for one request. `path`
/// carries the full path **and query string** (`/admin/shares/x?keep_data=1`),
/// so every request parameter is inside the signature.
fn canonical(method: &str, path: &str, ts: &str, nonce: &str, body: &[u8]) -> Vec<u8> {
    let body_hash = Hash::of(body).to_hex();
    format!("gaggle-admin\n{method}\n{path}\n{ts}\n{nonce}\n{body_hash}").into_bytes()
}

// --- status payloads -------------------------------------------------------

/// A snapshot of what a daemon is doing, returned by `GET /admin/status`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DaemonStatus {
    /// The daemon's Ed25519 identity, hex. Clients pin this.
    pub agent_id: String,
    /// The daemon's libp2p peer id.
    pub peer_id: String,
    /// `"relay"` or `"nas"`.
    pub role: String,
    /// Dialable listen addresses of the daemon's main node.
    pub listen_addrs: Vec<String>,
    pub shares: Vec<ShareStatus>,
    /// Cumulative chunk bytes this daemon has served to downloaders across all
    /// shares, since it started. A client samples this on a timer and diffs
    /// successive readings to plot the daemon's outbound throughput. Optional
    /// so an older daemon that never sets it round-trips unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_served_total: Option<u64>,
    /// NAS role: the resolved absolute path the replica chunk store lives at.
    /// An operator can move it with `POST /admin/storage`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replica_dir: Option<String>,
    /// NAS role: free space on the replica volume, bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replica_free_bytes: Option<u64>,
    /// NAS role: bytes every replica currently occupies on disk (sum of each
    /// share's `disk_bytes`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replica_used_bytes: Option<u64>,
    /// NAS role: the configured storage ceiling, bytes. `None` = unlimited. A
    /// share whose size would push `replica_used_bytes` over this is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_cap_bytes: Option<u64>,
}

/// One share a daemon accelerates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareStatus {
    pub manifest_id: String,
    pub name: String,
    pub files: usize,
    pub total_bytes: u64,
    pub version: u64,
    pub private: bool,
    /// Relay role: chunks of this share currently in the hot cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_chunks: Option<u64>,
    /// NAS role: chunks of this share on the durable replica.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replica_chunks: Option<u64>,
    /// NAS role: bytes the replica occupies on disk (the *compressed* footprint
    /// when compression is on).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_bytes: Option<u64>,
    /// NAS role: this share's own serving address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listen_addr: Option<String>,
    /// Whether the daemon is currently serving this share. `false` after an
    /// operator paused it (`POST /admin/shares/{id}` `{"seeding":false}`) — the
    /// replica / cache entry and token are kept, serving just stops until it is
    /// resumed. Defaults to `true` so an older daemon that never sets it (a
    /// share it reports is one it is serving) round-trips unchanged.
    #[serde(default = "serde_true")]
    pub seeding: bool,
    /// NAS role: set while the initial replication is still under way — the
    /// share is accepted and persisted (it'll retry across restarts) as soon
    /// as it's added, well before it's fully on disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replicating: Option<ReplicationProgress>,
    /// Populated if the share failed to start / replicate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A NAS share's replication progress, reported once per chunk while it is
/// still under way.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplicationProgress {
    pub chunks_done: usize,
    pub chunks_total: usize,
    pub bytes_done: u64,
    pub bytes_total: u64,
}

// --- backend channel -----------------------------------------------------

/// A mutation the router hands to the daemon's supervisor.
pub enum AdminCommand {
    AddShare { token: String, ack: oneshot::Sender<Result<(), String>> },
    RemoveShare {
        manifest_id: String,
        /// Keep the NAS replica's on-disk chunks (admin API `?keep_data=1`).
        /// Default is to delete them.
        keep_data: bool,
        ack: oneshot::Sender<Result<(), String>>,
    },
    /// Pause (`seeding = false`) or resume (`seeding = true`) serving one share
    /// without forgetting it — the replica / cache entry and token stay.
    SetSeeding {
        manifest_id: String,
        seeding: bool,
        ack: oneshot::Sender<Result<(), String>>,
    },
    /// NAS role: change where replica chunks are stored and/or the storage
    /// cap. `replica_dir` `Some(path)` moves the existing replicas to `path`
    /// (serving pauses for the move); `None` leaves it. `storage_cap_bytes`
    /// `Some(0)` clears the cap, `Some(n)` sets it, `None` leaves it.
    SetStorage {
        replica_dir: Option<String>,
        storage_cap_bytes: Option<u64>,
        ack: oneshot::Sender<Result<(), String>>,
    },
    /// Exit the daemon process (code 0) so a service manager restarts it —
    /// the way to pick up a newer binary when the daemon runs under
    /// `gaggle-accelerator-launcher` + systemd `Restart=always`.
    Restart { ack: oneshot::Sender<Result<(), String>> },
}

/// Everything the [`router`] needs. Cheap to clone.
#[derive(Clone)]
pub struct AdminState {
    authorized: Arc<Vec<AgentId>>,
    daemon: Arc<AgentKeypair>,
    commands: mpsc::Sender<AdminCommand>,
    status: watch::Receiver<DaemonStatus>,
    /// `(agent, nonce) -> expiry` — a signed request is honoured once inside the
    /// skew window so a captured request cannot be replayed.
    seen_nonces: Arc<Mutex<HashMap<String, i64>>>,
}

impl AdminState {
    pub fn new(
        authorized: Vec<AgentId>,
        daemon: AgentKeypair,
        commands: mpsc::Sender<AdminCommand>,
        status: watch::Receiver<DaemonStatus>,
    ) -> Self {
        Self {
            authorized: Arc::new(authorized),
            daemon: Arc::new(daemon),
            commands,
            status,
            seen_nonces: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// The daemon's own signing key — also the identity [`crate::tls::server_config`]
    /// derives the admin API's TLS certificate from.
    pub fn daemon_key(&self) -> &AgentKeypair {
        &self.daemon
    }
}

/// `GET /admin/status`, `GET /admin/shares`, `POST /admin/shares`,
/// `POST /admin/shares/{manifest_id}` (pause/resume),
/// `DELETE /admin/shares/{manifest_id}` — all behind operator-signature auth,
/// all responses daemon-signed.
pub fn router(state: AdminState) -> Router {
    Router::new()
        .route("/admin/status", get(status_handler))
        .route("/admin/shares", get(shares_handler).post(add_share_handler))
        .route(
            "/admin/shares/{manifest_id}",
            delete(remove_share_handler).post(set_seeding_handler),
        )
        .route("/admin/storage", axum::routing::post(set_storage_handler))
        .route("/admin/restart", axum::routing::post(restart_handler))
        .layer(middleware::from_fn_with_state(state.clone(), auth_and_sign))
        .with_state(state)
}

/// Serve the admin [`router`] on `listener` until the process ends, TLS
/// -terminated with a self-signed certificate derived from `state`'s own
/// daemon identity (see [`crate::tls`]) — the same posture as
/// [`crate::serve_daemon`], minus the rendezvous routes. A thin wrapper so
/// daemons need not depend on `axum`/`axum_server` directly.
pub async fn serve(listener: tokio::net::TcpListener, state: AdminState) -> anyhow::Result<()> {
    let tls_config = crate::tls::server_config(state.daemon_key())?;
    let rustls_config = axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(tls_config));
    axum_server::tls_rustls::from_tcp_rustls(listener.into_std()?, rustls_config)?
        .serve(router(state).into_make_service())
        .await?;
    Ok(())
}

async fn status_handler(State(state): State<AdminState>) -> Response {
    let status = state.status.borrow().clone();
    axum::Json(status).into_response()
}

async fn shares_handler(State(state): State<AdminState>) -> Response {
    let shares = state.status.borrow().shares.clone();
    axum::Json(shares).into_response()
}

#[derive(Deserialize)]
struct AddShareBody {
    /// A `gaggleshare1…` link token.
    link: String,
}

async fn add_share_handler(
    State(state): State<AdminState>,
    body: Result<axum::Json<AddShareBody>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(axum::Json(body)) = body else {
        return (StatusCode::BAD_REQUEST, "expected { \"link\": \"gaggleshare1…\" }").into_response();
    };
    let (ack, rx) = oneshot::channel();
    if state
        .commands
        .send(AdminCommand::AddShare { token: body.link, ack })
        .await
        .is_err()
    {
        return (StatusCode::SERVICE_UNAVAILABLE, "daemon is shutting down").into_response();
    }
    match rx.await {
        Ok(Ok(())) => StatusCode::ACCEPTED.into_response(),
        Ok(Err(e)) => (StatusCode::BAD_REQUEST, e).into_response(),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "daemon dropped the request").into_response(),
    }
}

async fn remove_share_handler(
    State(state): State<AdminState>,
    Path(manifest_id): Path<String>,
    axum::extract::Query(params): axum::extract::Query<HashMap<String, String>>,
) -> Response {
    let keep_data = params.get("keep_data").is_some_and(|v| {
        v.is_empty() || v == "1" || v.eq_ignore_ascii_case("true")
    });
    let (ack, rx) = oneshot::channel();
    if state
        .commands
        .send(AdminCommand::RemoveShare { manifest_id, keep_data, ack })
        .await
        .is_err()
    {
        return (StatusCode::SERVICE_UNAVAILABLE, "daemon is shutting down").into_response();
    }
    match rx.await {
        Ok(Ok(())) => StatusCode::ACCEPTED.into_response(),
        Ok(Err(e)) => (StatusCode::NOT_FOUND, e).into_response(),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "daemon dropped the request").into_response(),
    }
}

#[derive(Deserialize)]
struct SetSeedingBody {
    seeding: bool,
}

async fn set_seeding_handler(
    State(state): State<AdminState>,
    Path(manifest_id): Path<String>,
    body: Result<axum::Json<SetSeedingBody>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(axum::Json(body)) = body else {
        return (StatusCode::BAD_REQUEST, "expected { \"seeding\": true|false }").into_response();
    };
    let (ack, rx) = oneshot::channel();
    if state
        .commands
        .send(AdminCommand::SetSeeding { manifest_id, seeding: body.seeding, ack })
        .await
        .is_err()
    {
        return (StatusCode::SERVICE_UNAVAILABLE, "daemon is shutting down").into_response();
    }
    match rx.await {
        Ok(Ok(())) => StatusCode::ACCEPTED.into_response(),
        Ok(Err(e)) => (StatusCode::BAD_REQUEST, e).into_response(),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "daemon dropped the request").into_response(),
    }
}

#[derive(Deserialize)]
struct SetStorageBody {
    /// New replica root. Absent = leave it. The daemon moves existing replicas.
    #[serde(default)]
    replica_dir: Option<String>,
    /// `0` clears the cap, any other value sets it, absent leaves it.
    #[serde(default)]
    storage_cap_bytes: Option<u64>,
}

async fn set_storage_handler(
    State(state): State<AdminState>,
    body: Result<axum::Json<SetStorageBody>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(axum::Json(body)) = body else {
        return (
            StatusCode::BAD_REQUEST,
            "expected { \"replica_dir\"?: \"…\", \"storage_cap_bytes\"?: N }",
        )
            .into_response();
    };
    if body.replica_dir.is_none() && body.storage_cap_bytes.is_none() {
        return (StatusCode::BAD_REQUEST, "nothing to change").into_response();
    }
    let (ack, rx) = oneshot::channel();
    if state
        .commands
        .send(AdminCommand::SetStorage {
            replica_dir: body.replica_dir,
            storage_cap_bytes: body.storage_cap_bytes,
            ack,
        })
        .await
        .is_err()
    {
        return (StatusCode::SERVICE_UNAVAILABLE, "daemon is shutting down").into_response();
    }
    match rx.await {
        Ok(Ok(())) => StatusCode::ACCEPTED.into_response(),
        Ok(Err(e)) => (StatusCode::BAD_REQUEST, e).into_response(),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "daemon dropped the request").into_response(),
    }
}

async fn restart_handler(State(state): State<AdminState>) -> Response {
    let (ack, rx) = oneshot::channel();
    if state.commands.send(AdminCommand::Restart { ack }).await.is_err() {
        return (StatusCode::SERVICE_UNAVAILABLE, "daemon is shutting down").into_response();
    }
    match rx.await {
        Ok(Ok(())) => StatusCode::ACCEPTED.into_response(),
        Ok(Err(e)) => (StatusCode::BAD_REQUEST, e).into_response(),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "daemon dropped the request").into_response(),
    }
}

/// Verify the operator signature on the way in; sign the response on the way out.
async fn auth_and_sign(State(state): State<AdminState>, req: Request, next: Next) -> Response {
    let (parts, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_BODY).await {
        Ok(b) => b,
        Err(_) => return (StatusCode::BAD_REQUEST, "request body too large").into_response(),
    };

    // Sign over path *and* query — otherwise `?keep_data=…` on
    // `DELETE /admin/shares/{id}` (which flips whether a NAS replica's bytes are
    // kept or deleted) rides unsigned and an on-path attacker could add or strip
    // it.
    let path_and_query = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or_else(|| parts.uri.path());
    if let Err((code, msg)) = verify_request(
        &state,
        parts.method.as_str(),
        path_and_query,
        &parts.headers,
        &bytes,
    ) {
        return sign_response(&state.daemon, (code, msg).into_response()).await;
    }

    let req = Request::from_parts(parts, axum::body::Body::from(bytes));
    let resp = next.run(req).await;
    sign_response(&state.daemon, resp).await
}

fn verify_request(
    state: &AdminState,
    method: &str,
    path: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<(), (StatusCode, String)> {
    let unauth = |m: &str| (StatusCode::UNAUTHORIZED, m.to_string());
    let get = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());

    let agent_hex = get(H_AGENT).ok_or_else(|| unauth("missing agent header"))?;
    let ts = get(H_TIMESTAMP).ok_or_else(|| unauth("missing timestamp header"))?;
    let nonce = get(H_NONCE).ok_or_else(|| unauth("missing nonce header"))?;
    let sig_b64 = get(H_SIGNATURE).ok_or_else(|| unauth("missing signature header"))?;

    let agent = AgentId::from_hex(agent_hex).map_err(|_| unauth("malformed agent id"))?;
    if !state.authorized.contains(&agent) {
        return Err(unauth("agent is not authorised on this daemon"));
    }

    let ts_val: i64 = ts.parse().map_err(|_| unauth("malformed timestamp"))?;
    if (unix_now() as i64 - ts_val).abs() > MAX_SKEW_SECS {
        return Err(unauth("timestamp is outside the accepted window"));
    }

    let sig_bytes = unb64(sig_b64).ok_or_else(|| unauth("malformed signature"))?;
    let sig_arr: [u8; Signature::LEN] =
        sig_bytes.try_into().map_err(|_| unauth("signature is not 64 bytes"))?;
    let sig = Signature::from_bytes(sig_arr);

    let msg = canonical(method, path, ts, nonce, body);
    agent.verify(&msg, &sig).map_err(|_| unauth("signature does not verify"))?;

    // Replay protection: accept each (agent, nonce) at most once inside the skew
    // window. The signature is already valid here, so this only rejects a
    // byte-for-byte replay of a genuine request.
    let now = unix_now() as i64;
    let mut seen = state.seen_nonces.lock().unwrap_or_else(|e| e.into_inner());
    seen.retain(|_, exp| *exp > now);
    if seen.len() >= MAX_SEEN_NONCES {
        return Err((StatusCode::SERVICE_UNAVAILABLE, "replay cache saturated, retry shortly".into()));
    }
    if seen.insert(format!("{agent_hex}:{nonce}"), now + MAX_SKEW_SECS + 1).is_some() {
        return Err(unauth("nonce already used"));
    }
    Ok(())
}

/// Buffer `resp`, attach the daemon's identity + a signature over the body hash.
async fn sign_response(daemon: &AgentKeypair, resp: Response) -> Response {
    let (mut parts, body) = resp.into_parts();
    let bytes = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(b) => b,
        Err(_) => Bytes::new(),
    };
    let digest = Hash::of(&bytes).to_hex();
    let sig = daemon.sign(digest.as_bytes());

    insert(&mut parts.headers, H_DAEMON, &daemon.public().to_hex());
    insert(&mut parts.headers, H_DAEMON_SIGNATURE, &b64(&sig.to_bytes()));

    Response::from_parts(parts, axum::body::Body::from(bytes))
}

fn insert(headers: &mut HeaderMap, name: &str, value: &str) {
    if let (Ok(n), Ok(v)) = (
        HeaderName::from_bytes(name.as_bytes()),
        value.parse::<axum::http::HeaderValue>(),
    ) {
        headers.insert(n, v);
    }
}

// --- client ------------------------------------------------------------

/// HTTP(S) client for a daemon's [`router`]. Signs every request with the
/// operator key; verifies (and can pin) the daemon's response signature; and,
/// via [`crate::tls::client_config`], pins that exact same identity at the
/// TLS layer instead of trusting any CA.
pub struct AdminClient {
    base: String,
    http: crate::http_client::HttpClient,
    operator: AgentKeypair,
    /// The daemon identity we expect. `None` until the first successful
    /// connection, after which callers should persist [`AdminClient::pinned`].
    /// Shared with the TLS certificate verifier ([`crate::tls`]) so exactly
    /// one pin governs both layers.
    pinned: Arc<Mutex<Option<AgentId>>>,
}

impl AdminClient {
    pub fn new(base: impl Into<String>, operator: AgentKeypair, pinned: Option<AgentId>) -> anyhow::Result<Self> {
        let pinned = Arc::new(Mutex::new(pinned));
        let tls = crate::tls::client_config(pinned.clone())?;
        let http = crate::http_client::HttpClient::new(tls);
        Ok(Self { base: normalize_base(&base.into()), http, operator, pinned })
    }

    /// The daemon identity this client has locked onto, if any.
    pub fn pinned(&self) -> Option<AgentId> {
        *self.pinned.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The normalized base URL this client talks to.
    pub fn base(&self) -> &str {
        &self.base
    }

    pub async fn status(&mut self) -> anyhow::Result<DaemonStatus> {
        self.send("GET", "/admin/status", "", None).await
    }

    pub async fn list_shares(&mut self) -> anyhow::Result<Vec<ShareStatus>> {
        self.send("GET", "/admin/shares", "", None).await
    }

    pub async fn add_share(&mut self, link: &str) -> anyhow::Result<()> {
        let body = serde_json::to_vec(&serde_json::json!({ "link": link }))?;
        self.send_unit("POST", "/admin/shares", "", Some(body)).await
    }

    /// Stop accelerating a share and delete its NAS replica from disk.
    pub async fn remove_share(&mut self, manifest_id: &str) -> anyhow::Result<()> {
        self.send_unit("DELETE", &format!("/admin/shares/{manifest_id}"), "", None).await
    }

    /// Like [`remove_share`](Self::remove_share) but keep the on-disk replica so
    /// a later re-add resumes instead of re-fetching.
    pub async fn remove_share_keep_data(&mut self, manifest_id: &str) -> anyhow::Result<()> {
        self.send_unit("DELETE", &format!("/admin/shares/{manifest_id}"), "?keep_data=1", None)
            .await
    }

    /// Pause (`seeding = false`) or resume (`seeding = true`) serving one share.
    /// The daemon keeps its replica / cache entry and its persisted token — only
    /// serving stops or restarts.
    pub async fn set_share_seeding(&mut self, manifest_id: &str, seeding: bool) -> anyhow::Result<()> {
        let body = serde_json::to_vec(&serde_json::json!({ "seeding": seeding }))?;
        self.send_unit("POST", &format!("/admin/shares/{manifest_id}"), "", Some(body)).await
    }

    /// NAS role: change the replica storage folder and/or the storage cap.
    /// `replica_dir` `Some` moves existing replicas to the new path (serving
    /// pauses for the move). `storage_cap_bytes` `Some(0)` clears the cap,
    /// `Some(n)` sets it, `None` leaves it unchanged.
    pub async fn set_storage(
        &mut self,
        replica_dir: Option<&str>,
        storage_cap_bytes: Option<u64>,
    ) -> anyhow::Result<()> {
        let mut obj = serde_json::Map::new();
        if let Some(d) = replica_dir {
            obj.insert("replica_dir".into(), serde_json::Value::String(d.to_string()));
        }
        if let Some(n) = storage_cap_bytes {
            obj.insert("storage_cap_bytes".into(), serde_json::Value::from(n));
        }
        let body = serde_json::to_vec(&serde_json::Value::Object(obj))?;
        self.send_unit("POST", "/admin/storage", "", Some(body)).await
    }

    /// Ask the daemon to exit so its service manager restarts it on a newer
    /// binary. The connection may drop before a response is read — that is a
    /// successful restart, not an error, so a follow-up poll failing is
    /// expected.
    pub async fn restart(&mut self) -> anyhow::Result<()> {
        self.send_unit("POST", "/admin/restart", "", None).await
    }

    async fn send_unit(
        &mut self,
        method: &str,
        path: &str,
        query: &str,
        body: Option<Vec<u8>>,
    ) -> anyhow::Result<()> {
        let _: serde::de::IgnoredAny = self.send(method, path, query, body).await?;
        Ok(())
    }

    async fn send<T: serde::de::DeserializeOwned>(
        &mut self,
        method: &str,
        path: &str,
        query: &str,
        body: Option<Vec<u8>>,
    ) -> anyhow::Result<T> {
        let body = body.unwrap_or_default();
        let ts = unix_now().to_string();
        let mut nonce = [0u8; 12];
        getrandom::getrandom(&mut nonce).expect("system RNG unavailable");
        let nonce = b64(&nonce);
        // Signed over path + query together — the daemon verifies against
        // `uri.path_and_query()`, so `?keep_data=…` is covered by the signature.
        let signed_path = format!("{path}{query}");
        let sig = self.operator.sign(&canonical(method, &signed_path, &ts, &nonce, &body));
        let agent_hex = self.operator.public().to_hex();
        let sig_b64 = b64(&sig.to_bytes());

        let url = format!("{}{signed_path}", self.base);
        let headers = [
            (H_AGENT, agent_hex.as_str()),
            (H_TIMESTAMP, ts.as_str()),
            (H_NONCE, nonce.as_str()),
            (H_SIGNATURE, sig_b64.as_str()),
            ("content-type", "application/json"),
        ];
        let (status, headers, bytes) = self.http.send(method, &url, &headers, body).await?;
        let daemon_hdr = headers.get(H_DAEMON).and_then(|v| v.to_str().ok()).map(str::to_owned);
        let daemon_sig = headers
            .get(H_DAEMON_SIGNATURE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);

        self.verify_daemon(daemon_hdr.as_deref(), daemon_sig.as_deref(), &bytes)?;

        if !status.is_success() {
            anyhow::bail!(
                "daemon returned {status}: {}",
                String::from_utf8_lossy(&bytes).trim()
            );
        }
        if bytes.is_empty() {
            // e.g. 202 Accepted with no body — only valid for `()`-shaped calls.
            return Ok(serde_json::from_slice(b"null")?);
        }
        Ok(serde_json::from_slice(&bytes)?)
    }

    fn verify_daemon(
        &mut self,
        agent_hex: Option<&str>,
        sig_b64: Option<&str>,
        body: &[u8],
    ) -> anyhow::Result<()> {
        let agent_hex = agent_hex.ok_or_else(|| anyhow::anyhow!("daemon did not identify itself"))?;
        let sig_b64 = sig_b64.ok_or_else(|| anyhow::anyhow!("daemon did not sign its response"))?;
        let agent = AgentId::from_hex(agent_hex)?;
        let sig_bytes = unb64(sig_b64).ok_or_else(|| anyhow::anyhow!("malformed daemon signature"))?;
        let sig_arr: [u8; Signature::LEN] = sig_bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("daemon signature is not 64 bytes"))?;
        let digest = Hash::of(body).to_hex();
        agent
            .verify(digest.as_bytes(), &Signature::from_bytes(sig_arr))
            .map_err(|_| anyhow::anyhow!("daemon response signature does not verify"))?;

        // By the time an HTTP response reaches here, the TLS handshake for
        // this connection has already run (see `tls::PinningVerifier`) and,
        // on a fresh connection, already pinned whatever identity presented
        // the certificate. So this is also the cross-layer check: a valid
        // TLS connection whose *signed response* claims a different identity
        // than the one its certificate presented is rejected here.
        let mut pinned = self.pinned.lock().unwrap_or_else(|e| e.into_inner());
        match *pinned {
            Some(expected) if expected != agent => {
                anyhow::bail!("daemon identity changed — expected {expected}, got {agent}")
            }
            Some(_) => {}
            None => *pinned = Some(agent),
        }
        Ok(())
    }
}
