//! Control plane: an axum HTTP server + reqwest client for bootstrap, invite
//! token exchange, accelerator registration, and admin/status endpoints.
//! Low-volume, request/response-shaped — deliberately off the QUIC data plane.
//!
//! The first real piece is [`invite`] exchange — publish a
//! [`gaggle_core::Invite`] and hand out a short code, fetch it back by that
//! code. Bootstrap / registration / admin routes are still to come.

pub mod admin;
pub mod invite;
pub mod rendezvous;

pub use admin::{
    AdminClient, AdminCommand, AdminState, DaemonStatus, ShareStatus, router as admin_router,
};
pub use invite::{InviteClient, InviteRegistry, router as invite_router};
pub use rendezvous::{
    PeerInfo, PendingRequest, RendezvousClient, RendezvousRegistry, RequestState,
    router as rendezvous_router,
};

/// One-line status string for the accelerator daemon's start-up log.
pub fn describe() -> &'static str {
    "control-plane: axum invite exchange + NAT rendezvous; bootstrap/admin still stubbed"
}

/// Serve the signed admin API and the unauthenticated NAT-rendezvous
/// endpoints on the same listener — an accelerator has one HTTP port, and any
/// peer trying to reach one of its shares may need rendezvous, not just its
/// operator.
pub async fn serve_daemon(
    listener: tokio::net::TcpListener,
    admin: AdminState,
    rendezvous: RendezvousRegistry,
) -> anyhow::Result<()> {
    let app = admin::router(admin).merge(rendezvous::router(rendezvous));
    axum::serve(listener, app).await?;
    Ok(())
}
