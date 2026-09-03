//! Data-plane networking for Gaggle.
//!
//! - A libp2p **request-response over QUIC** protocol
//!   ([`proto`], [`transfer`]) that pulls a share and verifies every chunk
//!   against the manifest.
//! - [`Node`] wires that protocol together with **Kademlia**
//!   discovery, **identify**, a **relay client** and **dcutr** so peers can find
//!   each other's shares on the DHT and reach each other through a relay before
//!   upgrading to a direct connection. [`RelayNode`] is the always-on relay +
//!   bootstrap point (the accelerator's relay role).
//! - [`fetch_share_from_swarm`] pulls a share from many peers at
//!   once, choosing the rarest chunk first and spreading load across sources;
//!   [`Node::download_share_multi`] is the wired-up entry point.
#![forbid(unsafe_code)]

mod behaviour;
mod catalog;
mod codec;
mod node;
mod proto;
mod relay;
mod swarm;
mod transfer;

use std::time::Duration;

use gaggle_core::{Hash, Manifest};
use libp2p::Swarm;
use libp2p::kad::RecordKey;

use behaviour::{PeerBehaviour, RelayBehaviour};

pub use catalog::Catalog;
pub use gaggle_core::{
    CacheStats, Capability, DiskChunkStore, Invite, LruChunkCache, Scope, ShareKeypair,
    SharePublicKey, SignedCapability, manifest,
};
pub use libp2p::{Multiaddr, PeerId};
pub use node::{Node, NodeEvent};
pub use proto::{PROTOCOL, Request, Response};
pub use relay::{RelayConfig, RelayNode};
pub use swarm::{
    SwarmConfig, SwarmDownload, SwarmProgress, catalog_from_download, fetch_share_from_swarm,
    fetch_share_from_swarm_with_progress,
};
pub use transfer::{DownloadedShare, fetch_manifest_and_lists, fetch_share};

/// The loopback QUIC address every node listens on (port chosen by the OS).
pub(crate) const LISTEN_QUIC: &str = "/ip4/127.0.0.1/udp/0/quic-v1";

/// The DHT key a share is announced and discovered under: the manifest's
/// [content id](Manifest::id).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareKey(RecordKey);

impl ShareKey {
    /// Key for `manifest`. [`canonicalize`](Manifest::canonicalize) it first.
    pub fn from_manifest(manifest: &Manifest) -> Self {
        Self::from_id(manifest.id())
    }

    /// Key from a manifest id received out of band (an invite link).
    pub fn from_id(id: Hash) -> Self {
        Self(RecordKey::new(id.as_bytes()))
    }

    pub(crate) fn into_record_key(self) -> RecordKey {
        self.0
    }
}

pub(crate) fn build_peer_swarm() -> anyhow::Result<Swarm<PeerBehaviour>> {
    let swarm = libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_quic()
        .with_relay_client(libp2p::noise::Config::new, libp2p::yamux::Config::default)?
        .with_behaviour(PeerBehaviour::new)?
        .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();
    Ok(swarm)
}

pub(crate) fn build_relay_swarm() -> anyhow::Result<Swarm<RelayBehaviour>> {
    let swarm = libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_quic()
        .with_behaviour(RelayBehaviour::new)?
        .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();
    Ok(swarm)
}

/// One-line status string for the accelerator daemon's start-up log.
pub fn describe() -> &'static str {
    "net: libp2p QUIC + Kademlia DHT + relay/dcutr + rarest-first swarm + hot-cache + invite ACLs"
}
