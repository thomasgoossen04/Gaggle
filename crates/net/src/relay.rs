//! [`RelayNode`] — an accelerator in its relay role.
//!
//! It is three things at once:
//!
//! - a **libp2p relay server** that lets NAT'd peers accept inbound circuits
//!   (the TURN-style fallback path),
//! - a **Kademlia server** that acts as the swarm's bootstrap / rendezvous
//!   point, and
//! - a **hot-chunk cache**: a byte-budgeted
//!   [`LruChunkCache`](gaggle_core::LruChunkCache) in front of the chunk
//!   protocol. Once a share is registered with [`cache_share`](RelayNode::cache_share)
//!   the relay answers `GetChunk` from cache; on a miss it fetches the chunk
//!   from an upstream seed, verifies it, caches it (evicting the coldest chunk
//!   if full), and forwards it — so a swarm of peers hammering the relay costs
//!   the origin only one fetch per hot chunk.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use gaggle_core::{
    CacheStats, ChunkList, ChunkStore, Hash, LruChunkCache, Manifest, Scope, SharePublicKey,
};
use libp2p::futures::StreamExt;
use libp2p::multiaddr::Protocol;
use libp2p::request_response::{self, OutboundRequestId, ResponseChannel};
use libp2p::swarm::SwarmEvent;
use libp2p::swarm::dial_opts::{DialOpts, PeerCondition};
use libp2p::{Multiaddr, PeerId, Swarm};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::behaviour::{RelayBehaviour, RelayBehaviourEvent};
use crate::proto::{Request, Response};
use crate::{LISTEN_QUIC, build_relay_swarm};

/// How big the relay's hot-chunk cache may grow.
#[derive(Debug, Clone, Copy)]
pub struct RelayConfig {
    pub cache_capacity_bytes: u64,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self { cache_capacity_bytes: 256 * 1024 * 1024 }
    }
}

enum Command {
    ListenAddrs(oneshot::Sender<Vec<Multiaddr>>),
    AddUpstream { peer: PeerId, addr: Multiaddr },
    CacheShare { manifest: Box<Manifest>, chunk_lists: Vec<ChunkList>, upstreams: Vec<PeerId> },
    CacheStats(oneshot::Sender<CacheStats>),
    Restrict(SharePublicKey),
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

/// A connection's capability grant on the relay.
struct RelayGrant {
    scope: Scope,
    expires_at: Option<u64>,
}

/// Handle to a running relay/bootstrap/cache node. Drop stops it.
pub struct RelayNode {
    commands: Option<mpsc::Sender<Command>>,
    peer_id: PeerId,
    task: Option<JoinHandle<()>>,
}

impl RelayNode {
    /// Start the relay with the default cache budget, listening on an ephemeral
    /// loopback QUIC port.
    pub async fn spawn() -> anyhow::Result<Self> {
        Self::spawn_with(RelayConfig::default()).await
    }

    /// Start the relay with an explicit [`RelayConfig`].
    pub async fn spawn_with(config: RelayConfig) -> anyhow::Result<Self> {
        let mut swarm = build_relay_swarm()?;
        let peer_id = *swarm.local_peer_id();
        swarm.listen_on(LISTEN_QUIC.parse()?)?;

        loop {
            match swarm.select_next_some().await {
                SwarmEvent::NewListenAddr { address, .. } => {
                    swarm.add_external_address(address.clone());
                    break;
                }
                SwarmEvent::ListenerError { error, .. } => {
                    anyhow::bail!("relay listener failed before start-up: {error}");
                }
                _ => {}
            }
        }

        let (commands_tx, commands_rx) = mpsc::channel::<Command>(32);
        let task = tokio::spawn(async move {
            EventLoop::new(swarm, config, commands_rx).run().await;
        });

        Ok(Self { commands: Some(commands_tx), peer_id, task: Some(task) })
    }

    pub fn peer_id(&self) -> PeerId {
        self.peer_id
    }

    pub async fn listen_addrs(&self) -> anyhow::Result<Vec<Multiaddr>> {
        let (tx, rx) = oneshot::channel();
        self.send(Command::ListenAddrs(tx)).await?;
        Ok(rx.await?)
    }

    /// The first dialable `/quic-v1/p2p/<id>` listen address, if any.
    pub async fn listen_addr(&self) -> anyhow::Result<Multiaddr> {
        let p2p = Protocol::P2p(self.peer_id);
        self.listen_addrs()
            .await?
            .into_iter()
            .next()
            .map(|a| a.with(p2p))
            .ok_or_else(|| anyhow::anyhow!("relay has no listen address yet"))
    }

    /// Teach the relay a seed it can pull cache misses from. `addr` must carry
    /// `/p2p/<id>`; the relay dials it now so fills are fast later. Returns the
    /// upstream's peer id.
    pub async fn add_upstream(&self, addr: Multiaddr) -> anyhow::Result<PeerId> {
        let peer = peer_id_of(&addr)
            .ok_or_else(|| anyhow::anyhow!("upstream address {addr} has no /p2p/<peer-id>"))?;
        self.send(Command::AddUpstream { peer, addr }).await?;
        Ok(peer)
    }

    /// Register a share the relay should cache: its manifest and chunk lists (so
    /// the relay can answer `GetManifest` / `GetChunkList` and knows each
    /// chunk's expected length), and the `upstreams` to fetch misses from.
    pub async fn cache_share(
        &self,
        manifest: Manifest,
        chunk_lists: impl IntoIterator<Item = ChunkList>,
        upstreams: Vec<PeerId>,
    ) -> anyhow::Result<()> {
        self.send(Command::CacheShare {
            manifest: Box::new(manifest),
            chunk_lists: chunk_lists.into_iter().collect(),
            upstreams,
        })
        .await
    }

    /// Current hot-chunk cache occupancy and hit/miss counts.
    pub async fn cache_stats(&self) -> anyhow::Result<CacheStats> {
        let (tx, rx) = oneshot::channel();
        self.send(Command::CacheStats(tx)).await?;
        Ok(rx.await?)
    }

    /// Make the relay private: it will serve — from cache or via
    /// an upstream fill — only to a connection that has presented a valid
    /// [`SignedCapability`](gaggle_core::SignedCapability) for `share` and one
    /// of the cached manifests, and only within that capability's
    /// [`Scope`](gaggle_core::Scope). Call after [`cache_share`](Self::cache_share).
    pub async fn restrict_to_invite_holders(&self, share: SharePublicKey) -> anyhow::Result<()> {
        self.send(Command::Restrict(share)).await
    }

    /// Run until the process is stopped.
    pub async fn run_until_ctrl_c(self) -> anyhow::Result<()> {
        tokio::signal::ctrl_c().await?;
        self.shutdown().await;
        Ok(())
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
            .ok_or_else(|| anyhow::anyhow!("relay has been shut down"))?
            .send(command)
            .await
            .map_err(|_| anyhow::anyhow!("relay task has stopped"))
    }
}

impl Drop for RelayNode {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// A cache miss waiting on an upstream fetch.
struct PendingFill {
    hash: Hash,
    expected_len: u32,
    reply_to: ResponseChannel<Response>,
}

struct EventLoop {
    swarm: Swarm<RelayBehaviour>,
    commands: mpsc::Receiver<Command>,
    cache: LruChunkCache,

    /// One manifest per registered share, newest last.
    manifests: Vec<Manifest>,
    lists_by_root: HashMap<Hash, ChunkList>,
    /// Expected byte length of every chunk in a registered share.
    chunk_len: HashMap<Hash, u32>,
    /// Upstream seeds to try, per chunk.
    chunk_upstreams: HashMap<Hash, Vec<PeerId>>,
    /// Manifest path per chunk-list root, and per chunk — for per-file scope
    /// checks on a private relay.
    path_by_root: HashMap<Hash, String>,
    paths_by_chunk: HashMap<Hash, Vec<String>>,

    /// `Some` once [`RelayNode::restrict_to_invite_holders`] was called.
    restrict: Option<SharePublicKey>,
    grants: HashMap<PeerId, RelayGrant>,

    pending_fills: HashMap<OutboundRequestId, PendingFill>,
}

impl EventLoop {
    fn new(
        swarm: Swarm<RelayBehaviour>,
        config: RelayConfig,
        commands: mpsc::Receiver<Command>,
    ) -> Self {
        Self {
            swarm,
            commands,
            cache: LruChunkCache::new(config.cache_capacity_bytes),
            manifests: Vec::new(),
            lists_by_root: HashMap::new(),
            chunk_len: HashMap::new(),
            chunk_upstreams: HashMap::new(),
            path_by_root: HashMap::new(),
            paths_by_chunk: HashMap::new(),
            restrict: None,
            grants: HashMap::new(),
            pending_fills: HashMap::new(),
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

    fn on_command(&mut self, command: Command) {
        match command {
            Command::ListenAddrs(reply) => {
                let _ = reply.send(self.swarm.listeners().cloned().collect());
            }
            Command::AddUpstream { peer, addr } => {
                let dial_addr = strip_p2p(addr);
                self.swarm.behaviour_mut().kademlia.add_address(&peer, dial_addr.clone());
                let _ = self.swarm.dial(
                    DialOpts::peer_id(peer)
                        .addresses(vec![dial_addr])
                        .condition(PeerCondition::DisconnectedAndNotDialing)
                        .build(),
                );
            }
            Command::CacheShare { manifest, chunk_lists, upstreams } => {
                let path_of_root: HashMap<Hash, &str> =
                    manifest.files.iter().map(|f| (f.root, f.path.as_str())).collect();
                for list in &chunk_lists {
                    let root = list.root();
                    let path = path_of_root.get(&root).map(|p| p.to_string());
                    if let Some(path) = &path {
                        self.path_by_root.insert(root, path.clone());
                    }
                    for chunk in &list.chunks {
                        self.chunk_len.insert(chunk.hash, chunk.len);
                        self.chunk_upstreams.entry(chunk.hash).or_default().extend(&upstreams);
                        if let Some(path) = &path {
                            let entry = self.paths_by_chunk.entry(chunk.hash).or_default();
                            if !entry.contains(path) {
                                entry.push(path.clone());
                            }
                        }
                    }
                    self.lists_by_root.insert(root, list.clone());
                }
                tracing::info!(
                    share = %manifest.id(),
                    files = manifest.files.len(),
                    chunks = self.chunk_len.len(),
                    upstreams = upstreams.len(),
                    "caching share"
                );
                self.manifests.push(*manifest);
            }
            Command::CacheStats(reply) => {
                let _ = reply.send(self.cache.stats());
            }
            Command::Restrict(share) => {
                self.restrict = Some(share);
                self.grants.clear();
                tracing::info!(%share, "relay is now invite-only");
            }
        }
    }

    fn on_swarm_event(&mut self, event: SwarmEvent<RelayBehaviourEvent>) {
        match event {
            SwarmEvent::NewListenAddr { address, .. } => {
                self.swarm.add_external_address(address.clone());
                tracing::info!(%address, "relay listening");
            }
            SwarmEvent::Behaviour(RelayBehaviourEvent::Relay(e)) => {
                tracing::debug!(?e, "relay event");
            }
            SwarmEvent::Behaviour(RelayBehaviourEvent::Identify(
                libp2p::identify::Event::Received { peer_id, info, .. },
            )) => {
                for addr in info.listen_addrs {
                    self.swarm.behaviour_mut().kademlia.add_address(&peer_id, addr);
                }
            }
            SwarmEvent::ConnectionClosed { peer_id, num_established: 0, .. } => {
                self.grants.remove(&peer_id);
            }
            SwarmEvent::Behaviour(RelayBehaviourEvent::ChunkExchange(
                request_response::Event::Message { peer, message, .. },
            )) => self.on_chunk_message(peer, message),
            SwarmEvent::Behaviour(RelayBehaviourEvent::ChunkExchange(
                request_response::Event::OutboundFailure { request_id, error, .. },
            )) => {
                if let Some(fill) = self.pending_fills.remove(&request_id) {
                    tracing::debug!(hash = %fill.hash, %error, "upstream fill failed");
                    let _ = self
                        .swarm
                        .behaviour_mut()
                        .chunk_exchange
                        .send_response(fill.reply_to, Response::NotFound);
                }
            }
            _ => {}
        }
    }

    fn on_chunk_message(
        &mut self,
        peer: PeerId,
        message: request_response::Message<Request, Response>,
    ) {
        match message {
            request_response::Message::Request { request, channel, .. } => {
                self.answer_inbound(peer, request, channel);
            }
            request_response::Message::Response { request_id, response } => {
                let Some(fill) = self.pending_fills.remove(&request_id) else { return };
                let forwarded = match response {
                    Response::Chunk(data)
                        if Hash::of(&data) == fill.hash
                            && data.len() as u64 == u64::from(fill.expected_len) =>
                    {
                        self.cache.put(fill.hash, data.clone());
                        tracing::debug!(hash = %fill.hash, "cached chunk from upstream");
                        Response::Chunk(data)
                    }
                    other => {
                        tracing::debug!(hash = %fill.hash, got = other.kind(), "upstream fill unusable");
                        Response::NotFound
                    }
                };
                let _ = self
                    .swarm
                    .behaviour_mut()
                    .chunk_exchange
                    .send_response(fill.reply_to, forwarded);
            }
        }
    }

    fn answer_inbound(
        &mut self,
        peer: PeerId,
        request: Request,
        channel: ResponseChannel<Response>,
    ) {
        // Credential presentation.
        if let Request::Hello(cred) = &request {
            let resp = match &self.restrict {
                None => Response::Welcome,
                Some(share) => match self.admit(peer, cred, *share) {
                    Ok(()) => Response::Welcome,
                    Err(why) => Response::Unauthorized(why),
                },
            };
            let _ = self.swarm.behaviour_mut().chunk_exchange.send_response(channel, resp);
            return;
        }

        // Access + per-file scope for a private relay.
        let scope = if self.restrict.is_some() {
            match self.grants.get(&peer) {
                Some(g) if g.expires_at.is_some_and(|e| unix_now() >= e) => {
                    self.grants.remove(&peer);
                    let _ = self.swarm.behaviour_mut().chunk_exchange.send_response(
                        channel,
                        Response::Unauthorized("capability has expired".into()),
                    );
                    return;
                }
                Some(g) => Some(g.scope.clone()),
                None => {
                    let _ = self.swarm.behaviour_mut().chunk_exchange.send_response(
                        channel,
                        Response::Unauthorized("present a valid invite first".into()),
                    );
                    return;
                }
            }
        } else {
            None
        };
        let allows = |path: &str| scope.as_ref().is_none_or(|s| s.allows(path));

        let response = match request {
            Request::Hello(_) => unreachable!("handled above"),
            Request::GetManifest => {
                self.manifests.last().cloned().map_or(Response::NotFound, Response::Manifest)
            }
            Request::GetChunkList(root) => match self.path_by_root.get(&root) {
                Some(path) if !allows(path) => {
                    Response::Unauthorized("this file is outside your invite".into())
                }
                _ => self
                    .lists_by_root
                    .get(&root)
                    .cloned()
                    .map_or(Response::NotFound, Response::ChunkList),
            },
            Request::GetInventory => {
                let held: Vec<Hash> = self
                    .chunk_len
                    .keys()
                    .copied()
                    .filter(|h| self.cache.contains(h) && self.chunk_allowed(h, &scope))
                    .collect();
                Response::Inventory(held)
            }
            Request::GetChunk(hash) => {
                if !self.chunk_allowed(&hash, &scope) {
                    Response::Unauthorized("this chunk is outside your invite".into())
                } else if let Some(bytes) = self.cache.get_refreshing(&hash) {
                    Response::Chunk(bytes)
                } else if let Some(upstream) = self.pick_upstream(&hash) {
                    let id = self
                        .swarm
                        .behaviour_mut()
                        .chunk_exchange
                        .send_request(&upstream, Request::GetChunk(hash));
                    self.pending_fills.insert(
                        id,
                        PendingFill {
                            hash,
                            expected_len: self.chunk_len.get(&hash).copied().unwrap_or(0),
                            reply_to: channel,
                        },
                    );
                    return; // response goes out when the upstream answers
                } else {
                    Response::NotFound
                }
            }
        };
        let _ = self.swarm.behaviour_mut().chunk_exchange.send_response(channel, response);
    }

    /// Verify a presented capability and record its grant. `Err` carries a
    /// human-readable reason.
    fn admit(
        &mut self,
        peer: PeerId,
        cred: &gaggle_core::SignedCapability,
        share: SharePublicKey,
    ) -> Result<(), String> {
        let cap = cred.verify(unix_now()).map_err(|e| e.to_string())?;
        if cap.share != share {
            return Err("capability is for a different share".into());
        }
        if !self.manifests.iter().any(|m| m.id() == cap.manifest_id) {
            return Err("capability is for a manifest this relay does not cache".into());
        }
        self.grants
            .insert(peer, RelayGrant { scope: cap.scope.clone(), expires_at: cap.expires_at });
        Ok(())
    }

    /// Is `hash` inside `scope` (given what files the relay knows the chunk to
    /// be part of)? A `None` scope allows everything.
    fn chunk_allowed(&self, hash: &Hash, scope: &Option<Scope>) -> bool {
        match scope {
            None | Some(Scope::All) => true,
            Some(s) => {
                let paths = self.paths_by_chunk.get(hash);
                paths.is_some_and(|ps| ps.iter().any(|p| s.allows(p)))
            }
        }
    }

    /// An upstream for `hash`, preferring one we already have a connection to.
    fn pick_upstream(&self, hash: &Hash) -> Option<PeerId> {
        let candidates = self.chunk_upstreams.get(hash)?;
        candidates
            .iter()
            .find(|p| self.swarm.is_connected(p))
            .or_else(|| candidates.first())
            .copied()
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
