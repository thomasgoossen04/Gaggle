//! Data-plane networking for Gaggle.
//!
//! Milestone 2: a single libp2p **request-response** protocol over the **QUIC**
//! transport that lets one process serve a share and another pull it back over
//! loopback, verifying every chunk against the manifest as it arrives.
//!
//! - [`Catalog`] + [`ServerHandle`] — the serving side.
//! - [`Client`] + [`download_share`] — the subscribing side.
//! - [`proto`] — the wire messages; [`codec`](crate) frames them.
//!
//! Kademlia discovery and relay/dcutr NAT traversal (already pulled in as libp2p
//! features) layer onto this same [`Behaviour`] in milestone 3+.
#![forbid(unsafe_code)]

mod codec;
mod proto;

pub mod client;
pub mod server;

use std::time::Duration;

use libp2p::{StreamProtocol, Swarm, request_response};

pub use client::{Client, DownloadedShare, download_share};
pub use gaggle_core::manifest;
pub use proto::{PROTOCOL, Request, Response};
pub use server::{Catalog, ServerHandle};

/// The only network behaviour milestone 2 needs: request-response with our
/// [`codec`](crate::codec::GaggleCodec).
pub(crate) type Behaviour = request_response::Behaviour<codec::GaggleCodec>;

fn behaviour() -> Behaviour {
    request_response::Behaviour::with_codec(
        codec::GaggleCodec,
        std::iter::once((
            StreamProtocol::new(PROTOCOL),
            request_response::ProtocolSupport::Full,
        )),
        request_response::Config::default().with_request_timeout(Duration::from_secs(30)),
    )
}

/// Build a Tokio-driven libp2p swarm with QUIC transport and our [`Behaviour`].
pub(crate) fn build_swarm() -> anyhow::Result<Swarm<Behaviour>> {
    let swarm = libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_quic()
        .with_behaviour(|_| behaviour())?
        .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();
    Ok(swarm)
}

/// One-line status string for the accelerator daemon's start-up log.
pub fn describe() -> &'static str {
    "net: libp2p request-response over QUIC (milestone 2: loopback transfer)"
}
