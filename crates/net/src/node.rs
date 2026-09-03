//! [`Node`] — a standard peer: a background libp2p swarm task that can serve a
//! share, discover other peers' shares through the Kademlia DHT, and reach peers
//! behind NAT through a relay (with an opportunistic dcutr upgrade to a direct
//! connection).
//!
//! The public surface is a handful of async methods; everything else happens on
//! the spawned task. Chunk verification still lives in
//! [`fetch_share`](crate::transfer::fetch_share) — a discovered peer is trusted
//! for availability only.

use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use gaggle_core::{ChunkStore, Scope, SharePublicKey, SignedCapability};
use libp2p::futures::StreamExt;
use libp2p::kad::{self, QueryId};
use libp2p::multiaddr::Protocol;
use libp2p::request_response::{self, OutboundRequestId};
use libp2p::swarm::SwarmEvent;
use libp2p::swarm::dial_opts::{DialOpts, PeerCondition};
use libp2p::{Multiaddr, PeerId, Swarm};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::behaviour::{PeerBehaviour, PeerBehaviourEvent};
use crate::catalog::Catalog;
use crate::proto::{Request, Response};
use crate::swarm::{
    SwarmConfig, SwarmDownload, SwarmProgress, fetch_share_from_swarm,
    fetch_share_from_swarm_with_progress,
};
use crate::transfer::{DownloadedShare, fetch_manifest_and_lists, fetch_share};
use crate::{LISTEN_QUIC, ShareKey, build_peer_swarm};

/// Something worth telling the caller about that is not a direct reply to a
/// command.
#[derive(Debug, Clone)]
pub enum NodeEvent {
    /// identify negotiated with `peer`; its addresses are now known.
    Identified { peer: PeerId },
    /// A relay accepted this node's reservation; the node is now reachable at
    /// `circuit_addr`.
    RelayReservationAccepted { relay: PeerId, circuit_addr: Multiaddr },
    /// A dcutr hole-punch attempt against `peer` finished. `direct` is `true`
    /// when it produced a direct connection, `false` when every attempt failed
    /// and traffic stays on the relay.
    HolePunch { peer: PeerId, direct: bool },
    /// A peer's identify report gave us a candidate for our own external
    /// address. dcutr needs at least one of these before it can hole-punch.
    ExternalAddressCandidate { address: Multiaddr },
}

type ReplyResult<T> = oneshot::Sender<anyhow::Result<T>>;

/// Who this node will serve chunks to.
#[derive(Debug, Clone)]
enum Access {
    /// Anyone who connects (the default).
    Public,
    /// Only connections that have presented a valid [`SignedCapability`] for
    /// this share, and only within that capability's [`Scope`].
    Private { share: SharePublicKey, manifest_id: gaggle_core::Hash },
}

/// What a connection that presented a valid capability is allowed to pull.
#[derive(Debug, Clone)]
struct Grant {
    scope: Scope,
    expires_at: Option<u64>,
}

enum Command {
    ListenAddrs(oneshot::Sender<Vec<Multiaddr>>),
    Bootstrap { addr: Multiaddr, reply: ReplyResult<PeerId> },
    AddPeerAddress { peer: PeerId, addr: Multiaddr },
    Provide { key: ShareKey, reply: ReplyResult<()> },
    FindProviders { key: ShareKey, reply: ReplyResult<HashSet<PeerId>> },
    ReserveRelaySlot { relay: PeerId, relay_addr: Multiaddr, reply: ReplyResult<Multiaddr> },
    Request { peer: PeerId, request: Request, reply: ReplyResult<Response> },
    SetCatalog(Box<Catalog>),
    Restrict { share: SharePublicKey, reply: ReplyResult<()> },
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

/// Handle to a running peer. Cloneable-cheap parts are behind the channel; drop
/// stops the task.
pub struct Node {
    commands: Option<mpsc::Sender<Command>>,
    peer_id: PeerId,
    events: broadcast::Sender<NodeEvent>,
    task: Option<JoinHandle<()>>,
}

impl Node {
    /// Start a peer that only downloads.
    pub async fn spawn() -> anyhow::Result<Self> {
        Self::spawn_inner(None).await
    }

    /// Start a peer that also serves `catalog` over the chunk-exchange protocol.
    pub async fn spawn_serving(catalog: Catalog) -> anyhow::Result<Self> {
        Self::spawn_inner(Some(catalog)).await
    }

    async fn spawn_inner(catalog: Option<Catalog>) -> anyhow::Result<Self> {
        let mut swarm = build_peer_swarm()?;
        let peer_id = *swarm.local_peer_id();
        swarm.listen_on(LISTEN_QUIC.parse()?)?;

        // Wait until the OS has assigned us a port so `listen_addrs()` is useful
        // straight away. We deliberately do *not* `add_external_address` our own
        // loopback addr: that would mark it confirmed and make the swarm swallow
        // the identify-derived address candidates that dcutr needs to hole-punch.
        loop {
            match swarm.select_next_some().await {
                SwarmEvent::NewListenAddr { .. } => break,
                SwarmEvent::ListenerError { error, .. } => {
                    anyhow::bail!("listener failed before start-up: {error}");
                }
                _ => {}
            }
        }

        let (commands_tx, commands_rx) = mpsc::channel(64);
        let (events_tx, _) = broadcast::channel(128);
        let loop_events = events_tx.clone();
        let task = tokio::spawn(async move {
            EventLoop::new(swarm, catalog, commands_rx, loop_events).run().await;
        });

        Ok(Self { commands: Some(commands_tx), peer_id, events: events_tx, task: Some(task) })
    }

    pub fn peer_id(&self) -> PeerId {
        self.peer_id
    }

    /// Subscribe to [`NodeEvent`]s. Late subscribers miss earlier events.
    pub fn events(&self) -> broadcast::Receiver<NodeEvent> {
        self.events.subscribe()
    }

    pub async fn listen_addrs(&self) -> anyhow::Result<Vec<Multiaddr>> {
        let (tx, rx) = oneshot::channel();
        self.send(Command::ListenAddrs(tx)).await?;
        Ok(rx.await?)
    }

    /// The first listen address with this node's `/p2p/<id>` appended — the form
    /// to hand to another node.
    pub async fn listen_addr(&self) -> anyhow::Result<Multiaddr> {
        let p2p = Protocol::P2p(self.peer_id);
        self.listen_addrs()
            .await?
            .into_iter()
            .next()
            .map(|addr| addr.with(p2p))
            .ok_or_else(|| anyhow::anyhow!("node has no listen address yet"))
    }

    /// Register a dialable address for the peer named in `addr`'s `/p2p/<id>`
    /// component and return that peer id. A later [`request`](Self::request) or
    /// [`download_share`](Self::download_share) dials it lazily.
    pub async fn connect(&self, addr: Multiaddr) -> anyhow::Result<PeerId> {
        let peer = peer_id_of(&addr)
            .ok_or_else(|| anyhow::anyhow!("address {addr} has no /p2p/<peer-id>"))?;
        self.add_peer_address(peer, addr).await?;
        Ok(peer)
    }

    /// Connect to a bootstrap node (its multiaddr must carry `/p2p/<id>`), add it
    /// to the routing table, and kick off a DHT bootstrap. Returns its peer id.
    pub async fn bootstrap(&self, addr: Multiaddr) -> anyhow::Result<PeerId> {
        let (tx, rx) = oneshot::channel();
        self.send(Command::Bootstrap { addr, reply: tx }).await?;
        rx.await?
    }

    /// Teach the node a dialable address for `peer` (e.g. a relay circuit addr
    /// handed over out of band).
    pub async fn add_peer_address(&self, peer: PeerId, addr: Multiaddr) -> anyhow::Result<()> {
        self.send(Command::AddPeerAddress { peer, addr }).await
    }

    /// Announce on the DHT that this node serves the share identified by `key`.
    pub async fn provide(&self, key: ShareKey) -> anyhow::Result<()> {
        let (tx, rx) = oneshot::channel();
        self.send(Command::Provide { key, reply: tx }).await?;
        rx.await?
    }

    /// Look the share `key` up on the DHT and return the peers announcing it.
    pub async fn find_providers(&self, key: ShareKey) -> anyhow::Result<HashSet<PeerId>> {
        let (tx, rx) = oneshot::channel();
        self.send(Command::FindProviders { key, reply: tx }).await?;
        rx.await?
    }

    /// Reserve a slot on the relay at `relay_addr` (must carry `/p2p/<relay>`)
    /// and start listening on the resulting circuit address, which is returned
    /// for handing to peers that need to reach this node.
    pub async fn reserve_relay_slot(
        &self,
        relay: PeerId,
        relay_addr: Multiaddr,
    ) -> anyhow::Result<Multiaddr> {
        let (tx, rx) = oneshot::channel();
        self.send(Command::ReserveRelaySlot { relay, relay_addr, reply: tx }).await?;
        rx.await?
    }

    /// Send one chunk-exchange request to `peer`, resolving a route to it
    /// (routing table, learned addresses, or a DHT lookup) if not already
    /// connected.
    pub async fn request(&self, peer: PeerId, request: Request) -> anyhow::Result<Response> {
        let (tx, rx) = oneshot::channel();
        self.send(Command::Request { peer, request, reply: tx }).await?;
        rx.await?
    }

    /// Replace the served catalog.
    pub async fn serve(&self, catalog: Catalog) -> anyhow::Result<()> {
        self.send(Command::SetCatalog(Box::new(catalog))).await
    }

    /// Turn this node's share private (milestone 7): from now on it answers a
    /// request only on a connection that has presented a valid
    /// [`SignedCapability`] for `share` and the currently-served manifest, and
    /// only within that capability's [`Scope`]. Call [`serve`](Self::serve)
    /// first — the manifest id is taken from the current catalog.
    pub async fn restrict_to_invite_holders(
        &self,
        share: SharePublicKey,
    ) -> anyhow::Result<()> {
        let (tx, rx) = oneshot::channel();
        self.send(Command::Restrict { share, reply: tx }).await?;
        rx.await?
    }

    /// Present `credential` to `peer` for this connection. Required before any
    /// download from a peer whose share is private; a no-op-safe call on a
    /// public share (the peer just replies [`Response::Welcome`]).
    pub async fn authenticate(
        &self,
        peer: PeerId,
        credential: &SignedCapability,
    ) -> anyhow::Result<()> {
        match self.request(peer, Request::Hello(credential.clone())).await? {
            Response::Welcome => Ok(()),
            Response::Unauthorized(why) => anyhow::bail!("{peer} rejected the invite: {why}"),
            other => anyhow::bail!("expected Welcome from {peer}, got {}", other.kind()),
        }
    }

    /// [`authenticate`](Self::authenticate) against every peer in `peers`.
    pub async fn authenticate_all(
        &self,
        peers: &[PeerId],
        credential: &SignedCapability,
    ) -> anyhow::Result<()> {
        for &peer in peers {
            self.authenticate(peer, credential).await?;
        }
        Ok(())
    }

    /// Fetch and verify just `peer`'s share metadata — the manifest and every
    /// file's chunk list — without pulling any chunk data.
    pub async fn fetch_share_meta(
        &self,
        peer: PeerId,
    ) -> anyhow::Result<(gaggle_core::Manifest, std::collections::BTreeMap<String, gaggle_core::ChunkList>)>
    {
        fetch_manifest_and_lists(|request| self.request(peer, request)).await
    }

    /// Pull `peer`'s whole share into `store`, verifying every piece. `store`
    /// can be any [`ChunkStore`] — an in-RAM one, or a
    /// [`DiskChunkStore`](gaggle_core::DiskChunkStore) for a durable replica.
    pub async fn download_share<S: ChunkStore + ?Sized>(
        &self,
        peer: PeerId,
        store: &mut S,
    ) -> anyhow::Result<DownloadedShare> {
        fetch_share(|request| self.request(peer, request), store).await
    }

    /// Pull one share from several `sources` at once into `store`, fetching the
    /// rarest chunks first and spreading requests across the sources. Every
    /// chunk is still verified against the manifest, and a chunk is re-routed to
    /// another holder if a source fails or lacks it.
    pub async fn download_share_multi<S: ChunkStore + ?Sized>(
        &self,
        sources: &[PeerId],
        store: &mut S,
    ) -> anyhow::Result<SwarmDownload> {
        self.download_share_multi_with(sources, store, SwarmConfig::default()).await
    }

    /// [`download_share_multi`](Self::download_share_multi) but pulling from the
    /// sources in `prefer` first and only spilling to the rest when they
    /// saturate — for preferring a fast LAN/NAS replica over WAN peers.
    pub async fn download_share_multi_preferring<S: ChunkStore + ?Sized>(
        &self,
        sources: &[PeerId],
        prefer: impl IntoIterator<Item = PeerId>,
        store: &mut S,
    ) -> anyhow::Result<SwarmDownload> {
        self.download_share_multi_with(sources, store, SwarmConfig::preferring(prefer)).await
    }

    /// [`download_share_multi`](Self::download_share_multi) with an explicit
    /// [`SwarmConfig`].
    pub async fn download_share_multi_with<S: ChunkStore + ?Sized>(
        &self,
        sources: &[PeerId],
        store: &mut S,
        config: SwarmConfig,
    ) -> anyhow::Result<SwarmDownload> {
        fetch_share_from_swarm(
            sources,
            |peer, request| self.request(peer, request),
            store,
            config,
        )
        .await
    }

    /// [`download_share_multi`](Self::download_share_multi) that reports
    /// [`SwarmProgress`] once per chunk as it lands — for a progress bar.
    pub async fn download_share_multi_with_progress<S, P>(
        &self,
        sources: &[PeerId],
        store: &mut S,
        config: SwarmConfig,
        on_progress: P,
    ) -> anyhow::Result<SwarmDownload>
    where
        S: ChunkStore + ?Sized,
        P: FnMut(SwarmProgress),
    {
        fetch_share_from_swarm_with_progress(
            sources,
            |peer, request| self.request(peer, request),
            store,
            config,
            on_progress,
        )
        .await
    }

    pub async fn shutdown(mut self) {
        self.commands = None;
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }

    async fn send(&self, command: Command) -> anyhow::Result<()> {
        self.commands
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("node has been shut down"))?
            .send(command)
            .await
            .map_err(|_| anyhow::anyhow!("node task has stopped"))
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

struct PendingRequest {
    request: Request,
    reply: ReplyResult<Response>,
}

struct EventLoop {
    swarm: Swarm<PeerBehaviour>,
    catalog: Option<Catalog>,
    commands: mpsc::Receiver<Command>,
    events: broadcast::Sender<NodeEvent>,

    access: Access,
    /// Per-connection capability grants, keyed by peer. Populated by a
    /// successful [`Request::Hello`], dropped when the connection closes.
    grants: HashMap<PeerId, Grant>,

    peer_addrs: HashMap<PeerId, HashSet<Multiaddr>>,

    // In-flight bookkeeping.
    pending_connect: HashMap<PeerId, ReplyResult<PeerId>>,
    pending_provide: HashMap<QueryId, ReplyResult<()>>,
    pending_find: HashMap<QueryId, (HashSet<PeerId>, ReplyResult<HashSet<PeerId>>)>,
    pending_route: HashMap<QueryId, PeerId>,
    pending_relay: HashMap<PeerId, (Multiaddr, ReplyResult<Multiaddr>)>,
    requests_awaiting_conn: HashMap<PeerId, Vec<PendingRequest>>,
    pending_requests: HashMap<OutboundRequestId, ReplyResult<Response>>,
}

impl EventLoop {
    fn new(
        swarm: Swarm<PeerBehaviour>,
        catalog: Option<Catalog>,
        commands: mpsc::Receiver<Command>,
        events: broadcast::Sender<NodeEvent>,
    ) -> Self {
        Self {
            swarm,
            catalog,
            commands,
            events,
            access: Access::Public,
            grants: HashMap::new(),
            peer_addrs: HashMap::new(),
            pending_connect: HashMap::new(),
            pending_provide: HashMap::new(),
            pending_find: HashMap::new(),
            pending_route: HashMap::new(),
            pending_relay: HashMap::new(),
            requests_awaiting_conn: HashMap::new(),
            pending_requests: HashMap::new(),
        }
    }

    async fn run(mut self) {
        loop {
            tokio::select! {
                command = self.commands.recv() => match command {
                    None => break,
                    Some(command) => self.on_command(command),
                },
                event = self.swarm.select_next_some() => self.on_swarm_event(event),
            }
        }
    }

    fn learn_addr(&mut self, peer: PeerId, addr: Multiaddr) {
        self.peer_addrs.entry(peer).or_default().insert(addr.clone());
        self.swarm.behaviour_mut().kademlia.add_address(&peer, addr);
    }

    fn on_command(&mut self, command: Command) {
        match command {
            Command::ListenAddrs(reply) => {
                let _ = reply.send(self.swarm.listeners().cloned().collect());
            }
            Command::Bootstrap { addr, reply } => {
                let Some(peer) = peer_id_of(&addr) else {
                    let _ = reply.send(Err(anyhow::anyhow!(
                        "bootstrap address {addr} has no /p2p/<peer-id>"
                    )));
                    return;
                };
                let dial_addr = strip_p2p(addr);
                self.swarm.behaviour_mut().kademlia.add_address(&peer, dial_addr.clone());
                self.peer_addrs.entry(peer).or_default().insert(dial_addr.clone());
                match self.swarm.dial(
                    DialOpts::peer_id(peer)
                        .addresses(vec![dial_addr])
                        .condition(PeerCondition::DisconnectedAndNotDialing)
                        .build(),
                ) {
                    Ok(()) | Err(libp2p::swarm::DialError::DialPeerConditionFalse(_)) => {
                        self.pending_connect.insert(peer, reply);
                    }
                    Err(e) => {
                        let _ = reply.send(Err(anyhow::anyhow!("dialing bootstrap {peer}: {e}")));
                    }
                }
            }
            Command::AddPeerAddress { peer, addr } => self.learn_addr(peer, addr),
            Command::Provide { key, reply } => {
                match self.swarm.behaviour_mut().kademlia.start_providing(key.into_record_key()) {
                    Ok(qid) => {
                        self.pending_provide.insert(qid, reply);
                    }
                    Err(e) => {
                        let _ = reply.send(Err(anyhow::anyhow!("start_providing: {e}")));
                    }
                }
            }
            Command::FindProviders { key, reply } => {
                let qid = self.swarm.behaviour_mut().kademlia.get_providers(key.into_record_key());
                self.pending_find.insert(qid, (HashSet::new(), reply));
            }
            Command::ReserveRelaySlot { relay, relay_addr, reply } => {
                let base = strip_p2p(relay_addr.clone());
                let circuit_listen = relay_addr
                    .clone()
                    .with(Protocol::P2pCircuit);
                self.peer_addrs.entry(relay).or_default().insert(base.clone());
                self.swarm.behaviour_mut().kademlia.add_address(&relay, base);
                if let Err(e) = self.swarm.listen_on(circuit_listen) {
                    let _ = reply.send(Err(anyhow::anyhow!("listening on relay circuit: {e}")));
                    return;
                }
                self.pending_relay.insert(relay, (relay_addr, reply));
            }
            Command::Request { peer, request, reply } => self.dispatch_request(peer, request, reply),
            Command::SetCatalog(catalog) => self.catalog = Some(*catalog),
            Command::Restrict { share, reply } => {
                let result = match &self.catalog {
                    Some(catalog) => {
                        self.access =
                            Access::Private { share, manifest_id: catalog.manifest_id() };
                        self.grants.clear();
                        Ok(())
                    }
                    None => Err(anyhow::anyhow!(
                        "call serve() before restrict_to_invite_holders()"
                    )),
                };
                let _ = reply.send(result);
            }
        }
    }

    /// Decide how to answer an inbound request from `peer`, applying the access
    /// policy and any per-file scope.
    fn answer_for_peer(&mut self, peer: PeerId, request: &Request) -> Response {
        // `Hello` is always processed; on a private share it is how a grant is
        // obtained, on a public share it is a courteous no-op.
        if let Request::Hello(cred) = request {
            return match &self.access {
                Access::Public => Response::Welcome,
                Access::Private { share, manifest_id } => {
                    match cred.verify_for(share, manifest_id, unix_now()) {
                        Ok(cap) => {
                            self.grants.insert(
                                peer,
                                Grant { scope: cap.scope.clone(), expires_at: cap.expires_at },
                            );
                            Response::Welcome
                        }
                        Err(e) => Response::Unauthorized(e.to_string()),
                    }
                }
            };
        }

        let scope = match &self.access {
            Access::Public => None,
            Access::Private { .. } => match self.grants.get(&peer) {
                Some(grant) => {
                    if grant.expires_at.is_some_and(|exp| unix_now() >= exp) {
                        self.grants.remove(&peer);
                        return Response::Unauthorized("capability has expired".into());
                    }
                    Some(grant.scope.clone())
                }
                None => {
                    return Response::Unauthorized("present a valid invite first".into());
                }
            },
        };

        let Some(catalog) = &self.catalog else { return Response::NotFound };
        match (request, &scope) {
            (_, None) | (Request::GetManifest, _) => catalog.answer(request),
            (Request::GetInventory, Some(scope)) => {
                Response::Inventory(catalog.inventory_scoped(scope))
            }
            (Request::GetChunkList(root), Some(scope)) => match catalog.path_for_root(root) {
                Some(path) if scope.allows(path) => catalog.answer(request),
                Some(_) => Response::Unauthorized("this file is outside your invite".into()),
                None => Response::NotFound,
            },
            (Request::GetChunk(hash), Some(scope)) => {
                let paths = catalog.paths_for_chunk(hash);
                if paths.is_empty() {
                    Response::NotFound
                } else if paths.iter().any(|p| scope.allows(p)) {
                    catalog.answer(request)
                } else {
                    Response::Unauthorized("this chunk is outside your invite".into())
                }
            }
            (Request::Hello(_), _) => unreachable!("handled above"),
        }
    }

    fn dispatch_request(&mut self, peer: PeerId, request: Request, reply: ReplyResult<Response>) {
        if self.swarm.is_connected(&peer) {
            let id = self.swarm.behaviour_mut().chunk_exchange.send_request(&peer, request);
            self.pending_requests.insert(id, reply);
            return;
        }

        self.requests_awaiting_conn
            .entry(peer)
            .or_default()
            .push(PendingRequest { request, reply });

        // Already dialing / resolving for this peer? Ride along.
        if self.requests_awaiting_conn.get(&peer).map(Vec::len).unwrap_or(0) > 1
            || self.pending_route.values().any(|p| *p == peer)
        {
            return;
        }

        if let Some(addrs) = self.peer_addrs.get(&peer).filter(|a| !a.is_empty()) {
            let addrs = addrs.iter().cloned().collect();
            if let Err(e) = self.swarm.dial(
                DialOpts::peer_id(peer)
                    .addresses(addrs)
                    .extend_addresses_through_behaviour()
                    .condition(PeerCondition::DisconnectedAndNotDialing)
                    .build(),
            ) {
                self.fail_awaiting(peer, format!("dialing {peer}: {e}"));
            }
        } else {
            // No known route — ask the DHT who is close to this peer id.
            let qid = self.swarm.behaviour_mut().kademlia.get_closest_peers(peer);
            self.pending_route.insert(qid, peer);
        }
    }

    fn fail_awaiting(&mut self, peer: PeerId, msg: String) {
        if let Some(waiters) = self.requests_awaiting_conn.remove(&peer) {
            for w in waiters {
                let _ = w.reply.send(Err(anyhow::anyhow!(msg.clone())));
            }
        }
    }

    fn flush_awaiting(&mut self, peer: PeerId) {
        if let Some(waiters) = self.requests_awaiting_conn.remove(&peer) {
            for w in waiters {
                let id = self.swarm.behaviour_mut().chunk_exchange.send_request(&peer, w.request);
                self.pending_requests.insert(id, w.reply);
            }
        }
    }

    fn on_swarm_event(&mut self, event: SwarmEvent<PeerBehaviourEvent>) {
        match event {
            SwarmEvent::NewListenAddr { address, .. } => {
                // A relay reservation produced a circuit listen addr for us.
                if let Some(Some(relay)) = as_circuit(&address)
                    && let Some((relay_addr, reply)) = self.pending_relay.remove(&relay)
                {
                    let self_id = *self.swarm.local_peer_id();
                    let circuit_addr =
                        relay_addr.with(Protocol::P2pCircuit).with(Protocol::P2p(self_id));
                    let _ = self.events.send(NodeEvent::RelayReservationAccepted {
                        relay,
                        circuit_addr: circuit_addr.clone(),
                    });
                    let _ = reply.send(Ok(circuit_addr));
                }
            }
            SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                if let Some(reply) = self.pending_connect.remove(&peer_id) {
                    let _ = self.swarm.behaviour_mut().kademlia.bootstrap();
                    let _ = reply.send(Ok(peer_id));
                }
                self.flush_awaiting(peer_id);
            }
            SwarmEvent::ConnectionClosed { peer_id, num_established: 0, .. } => {
                // The peer must re-present its capability on a fresh connection.
                self.grants.remove(&peer_id);
            }
            SwarmEvent::OutgoingConnectionError { peer_id: Some(peer), error, .. } => {
                if let Some(reply) = self.pending_connect.remove(&peer) {
                    let _ = reply.send(Err(anyhow::anyhow!("connecting to {peer}: {error}")));
                }
                self.fail_awaiting(peer, format!("could not connect to {peer}: {error}"));
            }
            SwarmEvent::NewExternalAddrOfPeer { peer_id, address } => {
                self.peer_addrs.entry(peer_id).or_default().insert(address);
            }
            SwarmEvent::NewExternalAddrCandidate { address } => {
                let _ = self.events.send(NodeEvent::ExternalAddressCandidate { address });
            }
            SwarmEvent::Behaviour(event) => self.on_behaviour_event(event),
            _ => {}
        }
    }

    fn on_behaviour_event(&mut self, event: PeerBehaviourEvent) {
        match event {
            PeerBehaviourEvent::ChunkExchange(request_response::Event::Message {
                peer,
                message,
                ..
            }) => match message {
                request_response::Message::Request { request, channel, .. } => {
                    let response = self.answer_for_peer(peer, &request);
                    let _ =
                        self.swarm.behaviour_mut().chunk_exchange.send_response(channel, response);
                }
                request_response::Message::Response { request_id, response } => {
                    if let Some(reply) = self.pending_requests.remove(&request_id) {
                        let _ = reply.send(Ok(response));
                    }
                }
            },
            PeerBehaviourEvent::ChunkExchange(request_response::Event::OutboundFailure {
                request_id,
                error,
                ..
            }) => {
                if let Some(reply) = self.pending_requests.remove(&request_id) {
                    let _ = reply.send(Err(anyhow::anyhow!("request failed: {error}")));
                }
            }
            PeerBehaviourEvent::Identify(libp2p::identify::Event::Received {
                peer_id,
                info,
                ..
            }) => {
                for addr in info.listen_addrs {
                    self.learn_addr(peer_id, addr);
                }
                let _ = self.events.send(NodeEvent::Identified { peer: peer_id });
            }
            PeerBehaviourEvent::Identify(_) => {}
            PeerBehaviourEvent::Kademlia(kad_event) => self.on_kad_event(kad_event),
            PeerBehaviourEvent::Dcutr(event) => {
                let direct = event.result.is_ok();
                match &event.result {
                    Ok(_) => tracing::debug!(peer = %event.remote_peer_id, "dcutr established a direct connection"),
                    Err(e) => tracing::debug!(peer = %event.remote_peer_id, error = %e, "dcutr hole-punch failed; staying on relay"),
                }
                let _ = self
                    .events
                    .send(NodeEvent::HolePunch { peer: event.remote_peer_id, direct });
            }
            PeerBehaviourEvent::RelayClient(_) => {}
            _ => {}
        }
    }

    fn on_kad_event(&mut self, event: kad::Event) {
        let kad::Event::OutboundQueryProgressed { id, result, step, .. } = event else {
            if let kad::Event::RoutingUpdated { peer, addresses, .. } = event {
                for addr in addresses.into_vec() {
                    self.peer_addrs.entry(peer).or_default().insert(addr);
                }
            }
            return;
        };

        match result {
            kad::QueryResult::StartProviding(res) => {
                if let Some(reply) = self.pending_provide.remove(&id) {
                    let _ = reply.send(
                        res.map(|_| ()).map_err(|e| anyhow::anyhow!("announce failed: {e:?}")),
                    );
                }
            }
            kad::QueryResult::GetProviders(res) => {
                let Some((acc, _)) = self.pending_find.get_mut(&id) else { return };
                match res {
                    Ok(kad::GetProvidersOk::FoundProviders { providers, .. }) => {
                        acc.extend(providers);
                    }
                    Ok(kad::GetProvidersOk::FinishedWithNoAdditionalRecord { .. }) => {}
                    Err(_) => {}
                }
                if step.last {
                    let (acc, reply) = self.pending_find.remove(&id).unwrap();
                    let _ = reply.send(Ok(acc));
                }
            }
            kad::QueryResult::GetClosestPeers(res) => {
                let Some(target) = self.pending_route.remove(&id) else { return };
                let mut found = false;
                if let Ok(ok) = res {
                    for info in ok.peers {
                        if info.peer_id == target {
                            found = true;
                            for addr in info.addrs {
                                self.peer_addrs.entry(target).or_default().insert(addr.clone());
                                self.swarm.behaviour_mut().kademlia.add_address(&target, addr);
                            }
                        }
                    }
                }
                let has_addr =
                    self.peer_addrs.get(&target).map(|a| !a.is_empty()).unwrap_or(false);
                if found && has_addr {
                    let addrs = self.peer_addrs[&target].iter().cloned().collect();
                    if let Err(e) = self.swarm.dial(
                        DialOpts::peer_id(target)
                            .addresses(addrs)
                            .extend_addresses_through_behaviour()
                            .condition(PeerCondition::DisconnectedAndNotDialing)
                            .build(),
                    ) {
                        self.fail_awaiting(target, format!("dialing {target}: {e}"));
                    }
                } else {
                    self.fail_awaiting(target, format!("no route found to {target} on the DHT"));
                }
            }
            _ => {}
        }
    }
}

fn peer_id_of(addr: &Multiaddr) -> Option<PeerId> {
    addr.iter().find_map(|p| match p {
        Protocol::P2p(id) => Some(id),
        _ => None,
    })
}

fn strip_p2p(addr: Multiaddr) -> Multiaddr {
    addr.into_iter().filter(|p| !matches!(p, Protocol::P2p(_))).collect()
}

/// If `addr` is a circuit address, return `Some(relay_peer_id)` (or `Some(None)`
/// when the relay component carries no peer id).
fn as_circuit(addr: &Multiaddr) -> Option<Option<PeerId>> {
    if !addr.iter().any(|p| matches!(p, Protocol::P2pCircuit)) {
        return None;
    }
    // The peer id immediately before `/p2p-circuit` identifies the relay.
    let mut relay = None;
    for p in addr.iter() {
        match p {
            Protocol::P2p(id) => relay = Some(id),
            Protocol::P2pCircuit => break,
            _ => {}
        }
    }
    Some(relay)
}
