//! Control plane: an axum HTTP server + reqwest client for bootstrap, invite
//! token exchange, accelerator registration, and admin/status endpoints.
//! Low-volume, request/response-shaped — deliberately off the QUIC data plane.
//!
//! The first real piece is [`invite`] exchange — publish a
//! [`gaggle_core::Invite`] and hand out a short code, fetch it back by that
//! code. Bootstrap / registration / admin routes are still to come.

pub mod admin;
mod http_client;
pub mod invite;
pub mod rendezvous;
mod tls;
pub mod tracker;

pub use admin::{
    AdminClient, AdminCommand, AdminState, DaemonStatus, ShareStatus, router as admin_router,
};
pub use invite::{InviteClient, InviteRegistry, router as invite_router};
pub use rendezvous::{
    PeerInfo, PendingRequest, RendezvousClient, RendezvousRegistry, RequestState,
    router as rendezvous_router,
};
pub use tracker::{TrackerClient, TrackerRegistry, router as tracker_router};

/// One-line status string for the accelerator daemon's start-up log.
pub fn describe() -> &'static str {
    "control-plane: axum invite exchange + NAT rendezvous + seeder tracker, TLS-terminated admin API; bootstrap still stubbed"
}

/// Serve the signed admin API on `listener` and the unauthenticated
/// discovery endpoints — the NAT-[`rendezvous`] mailbox and the seeder
/// [`tracker`] — on `rendezvous_listener` (or, if `None`, merged onto
/// `listener` too). All TLS-terminated with a self-signed certificate
/// derived from `admin`'s own daemon identity (see [`tls`]); `AdminClient`
/// pins that same identity, so nothing else needs to trust a CA.
///
/// Splitting the two listeners is what lets an operator put the admin API
/// behind a private network (a Tailscale/VPN address, or just `127.0.0.1`)
/// while the discovery endpoints — unauthenticated by design, since any peer
/// trying to reach one of this daemon's shares may need them, not just the
/// operator — sit on a publicly reachable address instead. The common case
/// (one address, `rendezvous_listener: None`) is unchanged from before this
/// split existed.
pub async fn serve_daemon(
    listener: tokio::net::TcpListener,
    admin: AdminState,
    rendezvous: RendezvousRegistry,
    tracker: TrackerRegistry,
    rendezvous_listener: Option<tokio::net::TcpListener>,
) -> anyhow::Result<()> {
    let tls_config = std::sync::Arc::new(tls::server_config(admin.daemon_key())?);

    let Some(rendezvous_listener) = rendezvous_listener else {
        let app = admin::router(admin)
            .merge(rendezvous::router(rendezvous))
            .merge(tracker::router(tracker));
        return serve_tls(listener, tls_config, app).await;
    };

    tokio::try_join!(
        serve_tls(listener, tls_config.clone(), admin::router(admin)),
        serve_tls(
            rendezvous_listener,
            tls_config,
            rendezvous::router(rendezvous).merge(tracker::router(tracker)),
        ),
    )?;
    Ok(())
}

async fn serve_tls(
    listener: tokio::net::TcpListener,
    tls_config: std::sync::Arc<rustls::ServerConfig>,
    app: axum::Router,
) -> anyhow::Result<()> {
    let rustls_config = axum_server::tls_rustls::RustlsConfig::from_config(tls_config);
    axum_server::tls_rustls::from_tcp_rustls(listener.into_std()?, rustls_config)?
        .serve(app.into_make_service())
        .await?;
    Ok(())
}
