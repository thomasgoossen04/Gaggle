//! The composed libp2p behaviours.
//!
//! Two node shapes share one wire vocabulary:
//!
//! - [`PeerBehaviour`] — a standard peer (origin or subscriber): the chunk
//!   [`request_response`] protocol, plus **Kademlia** for discovery,
//!   **identify** so peers learn each other's addresses, **mDNS** so same-LAN
//!   peers find each other instantly with no DHT/relay/NAT concerns at all,
//!   **UPnP** to try for a directly-dialable public address with no relay
//!   involved at all, and a **relay client** + **dcutr** so a peer behind NAT
//!   that UPnP can't help (no IGD gateway, double NAT/CGNAT) can still be
//!   reached through a relay and then upgraded to a direct connection.
//! - [`RelayBehaviour`] — an accelerator in its relay role: a **relay server**
//!   plus a Kademlia node that doubles as the swarm's bootstrap/rendezvous
//!   point, plus the chunk [`request_response`] protocol so it can also serve a
//!   **hot-chunk cache** read through to an upstream seed.
//!
//! Both use private protocol names so a Gaggle swarm never touches the public
//! IPFS DHT.

use std::num::NonZeroU32;
use std::time::Duration;

use libp2p::identity::Keypair;
use libp2p::kad::store::MemoryStore;
use libp2p::swarm::NetworkBehaviour;
use libp2p::{
    StreamProtocol, connection_limits, dcutr, identify, kad, mdns, relay, request_response, upnp,
};

use crate::codec::GaggleCodec;
use crate::proto::PROTOCOL;

/// Private Kademlia protocol — isolates the swarm from the public DHT.
pub(crate) const KAD_PROTOCOL: StreamProtocol = StreamProtocol::new("/gaggle/kad/1.0.0");
/// Private identify protocol id.
pub(crate) const IDENTIFY_PROTOCOL: &str = "/gaggle/id/1.0.0";

const AGENT_VERSION: &str = concat!("gaggle/", env!("CARGO_PKG_VERSION"));

/// The request-response behaviour, shared by both node shapes.
pub(crate) fn chunk_exchange() -> request_response::Behaviour<GaggleCodec> {
    request_response::Behaviour::with_codec(
        GaggleCodec,
        std::iter::once((
            StreamProtocol::new(PROTOCOL),
            request_response::ProtocolSupport::Full,
        )),
        request_response::Config::default().with_request_timeout(Duration::from_secs(30)),
    )
}

/// The stock relay defaults cap a circuit at 128 KiB / 2 minutes — fine for
/// hole-punch signalling but far too tight for pulling chunks through the relay
/// as a fallback. An accelerator's whole job is to carry that traffic, so lift
/// the per-circuit byte cap (a 100 GB share stitched through the relay while
/// dcutr keeps failing to upgrade must not hit a wall) and stretch the
/// durations.
///
/// The stock *rate* limiters, though, are the only thing keeping an
/// unauthenticated relay from being an open, unmetered transit for anyone who
/// learns its address — 512 concurrent circuits, an hour each, no per-source
/// bound. The defaults (a 30-token burst refilling one per 2 minutes) are just
/// too tight: a firewalled peer leaning on relay fallback — exactly the case
/// this exists for — drains the burst in seconds and then gets "resource limit
/// exceeded" on nearly every dial. So keep rate limiting, but sized for real
/// Gaggle use: a downloader opens on the order of one circuit per relayed
/// source, holds a reservation for the session, and reconnects a handful of
/// times. These per-IP / per-peer buckets are generous for that and still cap
/// a flood well under `max_circuits`.
fn relay_config() -> relay::Config {
    let per_min = |n: u32| NonZeroU32::new(n).expect("non-zero");
    let minute = Duration::from_secs(60);
    relay::Config {
        max_circuit_bytes: 0, // unlimited — see above
        max_circuit_duration: Duration::from_secs(60 * 60),
        reservation_duration: Duration::from_secs(60 * 60),
        max_circuits: 512,
        max_circuits_per_peer: 16,
        max_reservations: 1024,
        max_reservations_per_peer: 8,
        reservation_rate_limiters: Vec::new(),
        circuit_src_rate_limiters: Vec::new(),
    }
    .reservation_rate_per_ip(per_min(60), minute)
    .reservation_rate_per_peer(per_min(30), minute)
    .circuit_src_per_ip(per_min(60), minute)
    .circuit_src_per_peer(per_min(30), minute)
}

fn kademlia(peer: libp2p::PeerId, config: kad::Config) -> kad::Behaviour<MemoryStore> {
    kad::Behaviour::with_config(peer, MemoryStore::new(peer), config)
}

/// A backstop against connection floods from an unauthenticated peer. Caps only
/// the two unambiguous abuse signals — half-open inbound connections, and how
/// many established connections a *single* peer may hold — and leaves the total
/// inbound count generous so a genuinely popular share isn't throttled. Every
/// request still costs the serving loop work, so this pairs with the memoized
/// inventory in [`crate::Catalog`] and the relay rate limiters above.
///
/// `per_peer` is higher for the relay role, where a single peer legitimately
/// holds one connection per relay circuit it opens (see `max_circuits_per_peer`
/// in [`relay_config`]).
fn connection_limits(per_peer: u32) -> connection_limits::Behaviour {
    let limits = connection_limits::ConnectionLimits::default()
        .with_max_pending_incoming(Some(256))
        .with_max_established_per_peer(Some(per_peer));
    connection_limits::Behaviour::new(limits)
}

fn identify(key: &Keypair) -> identify::Behaviour {
    identify::Behaviour::new(
        identify::Config::new(IDENTIFY_PROTOCOL.to_string(), key.public())
            .with_agent_version(AGENT_VERSION.to_string()),
    )
}

/// Behaviour for a standard peer.
#[derive(NetworkBehaviour)]
pub(crate) struct PeerBehaviour {
    pub connection_limits: connection_limits::Behaviour,
    pub chunk_exchange: request_response::Behaviour<GaggleCodec>,
    pub kademlia: kad::Behaviour<MemoryStore>,
    pub identify: identify::Behaviour,
    pub mdns: mdns::tokio::Behaviour,
    pub upnp: upnp::tokio::Behaviour,
    pub relay_client: relay::client::Behaviour,
    pub dcutr: dcutr::Behaviour,
}

impl PeerBehaviour {
    /// `relay_client` is handed in by `SwarmBuilder::with_relay_client`, which
    /// also wires the matching circuit transport.
    pub fn new(
        key: &Keypair,
        relay_client: relay::client::Behaviour,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let peer = key.public().to_peer_id();
        Ok(Self {
            connection_limits: connection_limits(16),
            chunk_exchange: chunk_exchange(),
            kademlia: kademlia(peer, kad::Config::new(KAD_PROTOCOL)),
            identify: identify(key),
            mdns: mdns::tokio::Behaviour::new(mdns::Config::default(), peer)?,
            upnp: upnp::tokio::Behaviour::default(),
            relay_client,
            dcutr: dcutr::Behaviour::new(peer),
        })
    }
}

/// Behaviour for an accelerator acting as relay + bootstrap + hot-chunk cache.
#[derive(NetworkBehaviour)]
pub(crate) struct RelayBehaviour {
    pub connection_limits: connection_limits::Behaviour,
    pub relay: relay::Behaviour,
    pub kademlia: kad::Behaviour<MemoryStore>,
    pub identify: identify::Behaviour,
    pub chunk_exchange: request_response::Behaviour<GaggleCodec>,
}

impl RelayBehaviour {
    pub fn new(key: &Keypair) -> Self {
        let peer = key.public().to_peer_id();
        // A bootstrap node has nobody to bootstrap *from*, so drop the periodic
        // self-bootstrap (it only logs "No known peers" until the first peer
        // dials in).
        let mut config = kad::Config::new(KAD_PROTOCOL);
        config.set_periodic_bootstrap_interval(None);
        let mut kademlia = kademlia(peer, config);
        // A bootstrap node answers queries from the start.
        kademlia.set_mode(Some(kad::Mode::Server));
        Self {
            // Above `max_circuits_per_peer` (16) plus the peer's own direct +
            // reservation connections and dcutr's transient upgrade sockets.
            connection_limits: connection_limits(96),
            relay: relay::Behaviour::new(peer, relay_config()),
            kademlia,
            identify: identify(key),
            chunk_exchange: chunk_exchange(),
        }
    }
}
