//! The composed libp2p behaviours.
//!
//! Two node shapes share one wire vocabulary:
//!
//! - [`PeerBehaviour`] — a standard peer (origin or subscriber): the chunk
//!   [`request_response`] protocol, plus **Kademlia** for discovery,
//!   **identify** so peers learn each other's addresses, a **relay client** and
//!   **dcutr** so a peer behind NAT can be reached through a relay and then
//!   upgraded to a direct connection.
//! - [`RelayBehaviour`] — an accelerator in its relay role: a **relay server**
//!   plus a Kademlia node that doubles as the swarm's bootstrap/rendezvous
//!   point, plus the chunk [`request_response`] protocol so it can also serve a
//!   **hot-chunk cache** read through to an upstream seed.
//!
//! Both use private protocol names so a Gaggle swarm never touches the public
//! IPFS DHT.

use std::time::Duration;

use libp2p::identity::Keypair;
use libp2p::kad::store::MemoryStore;
use libp2p::swarm::NetworkBehaviour;
use libp2p::{StreamProtocol, dcutr, identify, kad, relay, request_response};

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
/// the byte cap and stretch the durations.
///
/// The defaults also throttle *how often* a peer may open a reservation/circuit
/// at all: a per-peer token bucket of 30, refilling at just one token every 2
/// minutes once drained. That's sized for a public, untrusted relay — on our
/// private accelerator it means any firewalled peer leaning on relay fallback
/// (exactly the case it exists for) burns the burst in seconds and then gets a
/// "resource limit exceeded" on nearly every subsequent dial. Drop those rate
/// limiters entirely; `max_circuits(_per_peer)` / `max_reservations(_per_peer)`
/// below are the real ceiling.
fn relay_config() -> relay::Config {
    relay::Config {
        max_circuit_bytes: 0, // unlimited
        max_circuit_duration: Duration::from_secs(60 * 60),
        reservation_duration: Duration::from_secs(60 * 60),
        max_circuits: 512,
        max_circuits_per_peer: 64,
        max_reservations: 1024,
        max_reservations_per_peer: 16,
        reservation_rate_limiters: Vec::new(),
        circuit_src_rate_limiters: Vec::new(),
        ..relay::Config::default()
    }
}

fn kademlia(peer: libp2p::PeerId, config: kad::Config) -> kad::Behaviour<MemoryStore> {
    kad::Behaviour::with_config(peer, MemoryStore::new(peer), config)
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
    pub chunk_exchange: request_response::Behaviour<GaggleCodec>,
    pub kademlia: kad::Behaviour<MemoryStore>,
    pub identify: identify::Behaviour,
    pub relay_client: relay::client::Behaviour,
    pub dcutr: dcutr::Behaviour,
}

impl PeerBehaviour {
    /// `relay_client` is handed in by `SwarmBuilder::with_relay_client`, which
    /// also wires the matching circuit transport.
    pub fn new(key: &Keypair, relay_client: relay::client::Behaviour) -> Self {
        let peer = key.public().to_peer_id();
        Self {
            chunk_exchange: chunk_exchange(),
            kademlia: kademlia(peer, kad::Config::new(KAD_PROTOCOL)),
            identify: identify(key),
            relay_client,
            dcutr: dcutr::Behaviour::new(peer),
        }
    }
}

/// Behaviour for an accelerator acting as relay + bootstrap + hot-chunk cache.
#[derive(NetworkBehaviour)]
pub(crate) struct RelayBehaviour {
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
            relay: relay::Behaviour::new(peer, relay_config()),
            kademlia,
            identify: identify(key),
            chunk_exchange: chunk_exchange(),
        }
    }
}
