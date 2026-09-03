//! Invite exchange over plain HTTP.
//!
//! The data plane is private, so an invite ([`gaggle_core::Invite`]) has to
//! reach a new subscriber out of band. Copy-paste / QR is one channel; this is
//! the other — the origin `POST`s an invite to a small control-plane service and
//! hands out the short code it gets back, and the subscriber `GET`s the invite
//! by that code.
//!
//! The service holds invites in memory only. It never sees a share secret key —
//! an [`Invite`] carries just the public key and a signed, already-scoped
//! capability.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use gaggle_core::Invite;

/// In-memory store of published invites, keyed by a short random code.
#[derive(Clone, Default)]
pub struct InviteRegistry {
    inner: Arc<Mutex<HashMap<String, Invite>>>,
}

impl InviteRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Store `invite` under a fresh random code and return the code.
    pub fn publish(&self, invite: Invite) -> String {
        let code = random_code();
        self.inner.lock().unwrap().insert(code.clone(), invite);
        code
    }

    pub fn get(&self, code: &str) -> Option<Invite> {
        self.inner.lock().unwrap().get(code).cloned()
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// `POST /invites` (body: an [`Invite`]) → `{ "code": "…" }`;
/// `GET /invites/{code}` → the [`Invite`], or `404`.
pub fn router(registry: InviteRegistry) -> Router {
    Router::new()
        .route("/invites", post(publish_handler))
        .route("/invites/{code}", get(fetch_handler))
        .with_state(registry)
}

async fn publish_handler(
    State(registry): State<InviteRegistry>,
    body: axum::extract::Json<Invite>,
) -> Response {
    // Reject an invite whose embedded signature does not check out, so garbage
    // never enters the registry. Expiry is the subscriber's problem.
    if body.0.credential.verify(0).is_err() {
        return (StatusCode::BAD_REQUEST, "invite credential does not verify").into_response();
    }
    let code = registry.publish(body.0);
    axum::extract::Json(serde_json::json!({ "code": code })).into_response()
}

async fn fetch_handler(
    State(registry): State<InviteRegistry>,
    Path(code): Path<String>,
) -> Response {
    match registry.get(&code) {
        Some(invite) => axum::extract::Json(invite).into_response(),
        None => (StatusCode::NOT_FOUND, "no such invite").into_response(),
    }
}

/// `reqwest` client for the [`router`] endpoints.
pub struct InviteClient {
    base: String,
    http: reqwest::Client,
}

impl InviteClient {
    /// `base` is the service origin, e.g. `http://accelerator.example:8080`.
    pub fn new(base: impl Into<String>) -> Self {
        Self { base: base.into().trim_end_matches('/').to_string(), http: reqwest::Client::new() }
    }

    /// Publish `invite`; returns its code.
    pub async fn publish(&self, invite: &Invite) -> anyhow::Result<String> {
        #[derive(serde::Deserialize)]
        struct Published {
            code: String,
        }
        let published: Published = self
            .http
            .post(format!("{}/invites", self.base))
            .json(invite)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(published.code)
    }

    /// Fetch the invite published under `code`.
    pub async fn fetch(&self, code: &str) -> anyhow::Result<Invite> {
        let invite = self
            .http
            .get(format!("{}/invites/{code}", self.base))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(invite)
    }
}

fn random_code() -> String {
    let mut bytes = [0u8; 12];
    getrandom::getrandom(&mut bytes).expect("system RNG unavailable");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
