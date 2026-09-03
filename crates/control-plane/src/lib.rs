//! Control plane: an axum HTTP server + reqwest client for bootstrap, invite
//! token exchange, accelerator registration, and admin/status endpoints.
//! Low-volume, request/response-shaped — deliberately off the QUIC data plane.
//!
//! The first real piece is [`invite`] exchange — publish a
//! [`gaggle_core::Invite`] and hand out a short code, fetch it back by that
//! code. Bootstrap / registration / admin routes are still to come.

pub mod invite;

pub use invite::{InviteClient, InviteRegistry, router as invite_router};

/// One-line status string for the accelerator daemon's start-up log.
pub fn describe() -> &'static str {
    "control-plane: axum invite exchange; bootstrap/admin still stubbed"
}
