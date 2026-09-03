//! Control plane: an axum HTTPS server + reqwest client for bootstrap, invite
//! token exchange, accelerator registration, and admin/status endpoints.
//! Low-volume, request/response-shaped — deliberately off the QUIC data plane.

/// Placeholder until the bootstrap/admin routes land.
pub fn describe() -> &'static str {
    "control-plane: axum bootstrap/admin API (not yet implemented)"
}
