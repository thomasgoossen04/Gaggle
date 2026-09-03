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

pub mod accel;
mod behaviour;
mod catalog;
mod codec;
mod link;
mod node;
mod proto;
mod relay;
mod swarm;
mod transfer;

use std::path::Path;
use std::time::Duration;

use gaggle_core::{Hash, Manifest};
use libp2p::Swarm;
use libp2p::kad::RecordKey;

use behaviour::{PeerBehaviour, RelayBehaviour};

pub use catalog::Catalog;
pub use link::ShareLink;
pub use gaggle_core::{
    CacheStats, Capability, DiskChunkStore, Invite, LruChunkCache, Scope, ShareKeypair,
    SharePublicKey, SignedCapability, manifest,
};
pub use libp2p::identity::Keypair;
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
    build_peer_swarm_with(Keypair::generate_ed25519())
}

pub(crate) fn build_peer_swarm_with(keypair: Keypair) -> anyhow::Result<Swarm<PeerBehaviour>> {
    let swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_quic()
        .with_relay_client(libp2p::noise::Config::new, libp2p::yamux::Config::default)?
        .with_behaviour(PeerBehaviour::new)?
        .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();
    Ok(swarm)
}

pub(crate) fn build_relay_swarm() -> anyhow::Result<Swarm<RelayBehaviour>> {
    build_relay_swarm_with(Keypair::generate_ed25519())
}

pub(crate) fn build_relay_swarm_with(keypair: Keypair) -> anyhow::Result<Swarm<RelayBehaviour>> {
    let swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_quic()
        .with_behaviour(RelayBehaviour::new)?
        .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();
    Ok(swarm)
}

/// A libp2p Ed25519 [`Keypair`] from a 32-byte seed. Deterministic — the same
/// seed always yields the same [`PeerId`]. Used to give a NAS accelerator's
/// per-share serving node a stable identity derived from the daemon's own key.
pub fn keypair_from_seed(mut seed: [u8; 32]) -> Keypair {
    // `ed25519_from_bytes` consumes/zeroes the buffer.
    Keypair::ed25519_from_bytes(&mut seed).expect("32 bytes is a valid ed25519 seed")
}

/// The 32-byte Ed25519 secret seed backing `keypair`, if it is an Ed25519 key.
/// Lets a caller derive sibling identities (e.g. one per share) from one stored
/// key.
pub fn identity_seed(keypair: &Keypair) -> anyhow::Result<[u8; 32]> {
    let ed = keypair
        .clone()
        .try_into_ed25519()
        .map_err(|_| anyhow::anyhow!("identity is not an Ed25519 key"))?;
    Ok(ed.secret().as_ref().try_into().expect("ed25519 secret is 32 bytes"))
}

/// Load a persistent libp2p identity from `path`, creating (and persisting) a
/// fresh Ed25519 keypair there if the file does not exist yet. The file holds
/// the key's protobuf encoding; it is created with `0o600` where the platform
/// supports it. A stable identity means a stable [`PeerId`] across restarts.
pub fn load_or_create_identity(path: &Path) -> anyhow::Result<Keypair> {
    use anyhow::Context;

    match std::fs::read(path) {
        Ok(bytes) => Keypair::from_protobuf_encoding(&bytes)
            .with_context(|| format!("parsing identity file {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let keypair = Keypair::generate_ed25519();
            let encoded = keypair.to_protobuf_encoding().context("encoding fresh identity")?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            write_private(path, &encoded)
                .with_context(|| format!("writing identity file {}", path.display()))?;
            Ok(keypair)
        }
        Err(e) => Err(e).with_context(|| format!("reading identity file {}", path.display())),
    }
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}

/// One-line status string for the accelerator daemon's start-up log.
pub fn describe() -> &'static str {
    "net: libp2p QUIC + Kademlia DHT + relay/dcutr + rarest-first swarm + hot-cache + invite ACLs"
}
