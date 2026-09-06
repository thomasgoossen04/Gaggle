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

use std::collections::{HashMap, HashSet};
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
use crate::LISTEN_QUIC;

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
    Restrict { share: SharePublicKey, manifest_id: Hash },
    Unrestrict(SharePublicKey),
    RemoveShare(Hash),
    Shares(oneshot::Sender<Vec<Hash>>),
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

/// A connection's capability grant on the relay — one per private share the
/// connection has authenticated for.
struct RelayGrant {
    share: SharePublicKey,
    scope: Scope,
    expires_at: Option<u64>,
}

impl RelayGrant {
    fn live(&self, now: u64) -> bool {
        self.expires_at.is_none_or(|e| now < e)
    }
}

/// One share the relay caches: its metadata, the seeds to fill misses from, and
/// (for a private share) the key an invite must be signed under.
struct ShareEntry {
    manifest: Manifest,
    lists: Vec<ChunkList>,
    upstreams: Vec<PeerId>,
    /// `Some` once [`RelayNode::restrict_to_invite_holders`] gated this share.
    restrict: Option<SharePublicKey>,
}

/// Handle to a running relay/bootstrap/cache node. Drop stops it.
pub struct RelayNode {
    commands: Option<mpsc::Sender<Command>>,
    peer_id: PeerId,
    /// The 32-byte Ed25519 seed of this relay's libp2p identity, so it can sign
    /// a seeder-tracker announce for the shares it caches without reaching into
    /// the swarm task.
    identity_seed: [u8; 32],
    task: Option<JoinHandle<()>>,
}

impl RelayNode {
    /// Start the relay with the default cache budget, listening on an ephemeral
    /// QUIC port on every local interface.
    pub async fn spawn() -> anyhow::Result<Self> {
        Self::spawn_with(RelayConfig::default()).await
    }

    /// Start the relay with an explicit [`RelayConfig`].
    pub async fn spawn_with(config: RelayConfig) -> anyhow::Result<Self> {
        Self::spawn_inner(config, None, None).await
    }

    /// [`spawn_with`](Self::spawn_with) plus a persistent libp2p identity, so the
    /// relay keeps the same [`PeerId`] across restarts.
    pub async fn spawn_with_identity(
        config: RelayConfig,
        keypair: crate::Keypair,
    ) -> anyhow::Result<Self> {
        Self::spawn_inner(config, Some(keypair), None).await
    }

    /// [`spawn_with`](Self::spawn_with) with an optional persistent identity and
    /// an optional explicit listen [`Multiaddr`] (e.g.
    /// `/ip4/0.0.0.0/udp/4001/quic-v1` for a public daemon).
    pub async fn spawn_with_opts(
        config: RelayConfig,
        keypair: Option<crate::Keypair>,
        listen: Option<Multiaddr>,
    ) -> anyhow::Result<Self> {
        Self::spawn_inner(config, keypair, listen).await
    }

    async fn spawn_inner(
        config: RelayConfig,
        keypair: Option<crate::Keypair>,
        listen: Option<Multiaddr>,
    ) -> anyhow::Result<Self> {
        let keypair = keypair.unwrap_or_else(crate::Keypair::generate_ed25519);
        let identity_seed = crate::identity_seed(&keypair)
            .map_err(|_| anyhow::anyhow!("a Gaggle relay needs an Ed25519 identity"))?;
        let mut swarm = crate::build_relay_swarm_with(keypair)?;
        let peer_id = *swarm.local_peer_id();
        match listen {
            Some(addr) => swarm.listen_on(addr)?,
            None => swarm.listen_on(LISTEN_QUIC.parse()?)?,
        };

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

        Ok(Self { commands: Some(commands_tx), peer_id, identity_seed, task: Some(task) })
    }

    /// Sign `msg` with this relay's libp2p Ed25519 identity key — see
    /// [`Node::sign_identity`](crate::Node::sign_identity).
    pub fn sign_identity(&self, msg: &[u8]) -> [u8; 64] {
        crate::keypair_from_seed(self.identity_seed)
            .sign(msg)
            .expect("Ed25519 signing is infallible")
            .try_into()
            .expect("an Ed25519 signature is 64 bytes")
    }

    pub fn peer_id(&self) -> PeerId {
        self.peer_id
    }

    pub async fn listen_addrs(&self) -> anyhow::Result<Vec<Multiaddr>> {
        let (tx, rx) = oneshot::channel();
        self.send(Command::ListenAddrs(tx)).await?;
        Ok(rx.await?)
    }

    /// Every dialable `/quic-v1/p2p/<id>` listen address, ranked best-first
    /// (LAN, then any other reachable address, then loopback last).
    pub async fn reachable_addrs(&self) -> anyhow::Result<Vec<Multiaddr>> {
        let mut addrs = self.listen_addrs().await?;
        // Drop a wildcard `0.0.0.0`/`::` entry — undialable, and libp2p-quic
        // rejects it with `MultiaddrNotSupported` (see `Node::reachable_addrs`).
        addrs.retain(|a| !crate::addr_is_unspecified(a));
        crate::prefer_reachable(&mut addrs);
        Ok(addrs.into_iter().map(|a| a.with(Protocol::P2p(self.peer_id))).collect())
    }

    /// The best dialable `/quic-v1/p2p/<id>` listen address (LAN/WAN-reachable
    /// if the relay has one, else loopback), if any.
    pub async fn listen_addr(&self) -> anyhow::Result<Multiaddr> {
        self.reachable_addrs()
            .await?
            .into_iter()
            .next()
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

    /// Register (or refresh) a share the relay should cache: its manifest and
    /// chunk lists (so the relay can answer `GetManifest` / `GetChunkList` and
    /// knows each chunk's expected length), and the `upstreams` to fetch misses
    /// from. A relay may cache any number of shares at once; re-calling for a
    /// share already cached replaces its metadata and merges the upstreams.
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

    /// Stop caching the share with this manifest id and forget its metadata.
    /// Chunks of it already in the hot cache age out normally.
    pub async fn remove_share(&self, manifest_id: Hash) -> anyhow::Result<()> {
        self.send(Command::RemoveShare(manifest_id)).await
    }

    /// The manifest ids of every share the relay currently caches.
    pub async fn shares(&self) -> anyhow::Result<Vec<Hash>> {
        let (tx, rx) = oneshot::channel();
        self.send(Command::Shares(tx)).await?;
        Ok(rx.await?)
    }

    /// Current hot-chunk cache occupancy and hit/miss counts.
    pub async fn cache_stats(&self) -> anyhow::Result<CacheStats> {
        let (tx, rx) = oneshot::channel();
        self.send(Command::CacheStats(tx)).await?;
        Ok(rx.await?)
    }

    /// Gate the cached share `manifest_id`: the relay will serve its manifest,
    /// chunk lists and chunks — from cache or via an upstream fill — only to a
    /// connection that has presented a valid
    /// [`SignedCapability`](gaggle_core::SignedCapability) for `share` and that
    /// manifest, and only within the capability's
    /// [`Scope`](gaggle_core::Scope). Other cached shares are unaffected, so one
    /// relay can carry a mix of public and private shares. Call after
    /// [`cache_share`](Self::cache_share) for that share.
    pub async fn restrict_to_invite_holders(
        &self,
        share: SharePublicKey,
        manifest_id: Hash,
    ) -> anyhow::Result<()> {
        self.send(Command::Restrict { share, manifest_id }).await
    }

    /// Lift the gate that [`restrict_to_invite_holders`](Self::restrict_to_invite_holders)
    /// put on every share keyed to `share`; those shares become public again and
    /// existing grants for `share` are dropped.
    pub async fn unrestrict(&self, share: SharePublicKey) -> anyhow::Result<()> {
        self.send(Command::Unrestrict(share)).await
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

    /// Every share the relay caches, keyed by manifest id.
    shares: HashMap<Hash, ShareEntry>,

    // --- indexes derived from `shares`, rebuilt on every mutation ---
    lists_by_root: HashMap<Hash, ChunkList>,
    /// Manifest id that owns each chunk-list root.
    manifest_by_root: HashMap<Hash, Hash>,
    /// Expected byte length of every chunk in a cached share.
    chunk_len: HashMap<Hash, u32>,
    /// Upstream seeds to try, per chunk.
    chunk_upstreams: HashMap<Hash, Vec<PeerId>>,
    /// Manifest path per chunk-list root, and per chunk — for per-file scope
    /// checks on a private share.
    path_by_root: HashMap<Hash, String>,
    paths_by_chunk: HashMap<Hash, Vec<String>>,
    /// Manifest ids each chunk appears in — a chunk in any public share is
    /// always servable, one only in private shares needs a matching grant.
    manifests_by_chunk: HashMap<Hash, Vec<Hash>>,

    /// Per-connection capability grants, one entry per private share admitted.
    grants: HashMap<PeerId, Vec<RelayGrant>>,

    pending_fills: HashMap<OutboundRequestId, PendingFill>,

    /// Cumulative chunk bytes forwarded to downloaders (cache hit *or*
    /// miss-then-fill), across every cached share. Surfaced on
    /// [`CacheStats::bytes_served`] — the relay's "upload throughput" signal.
    bytes_served: u64,
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
            shares: HashMap::new(),
            lists_by_root: HashMap::new(),
            manifest_by_root: HashMap::new(),
            chunk_len: HashMap::new(),
            chunk_upstreams: HashMap::new(),
            path_by_root: HashMap::new(),
            paths_by_chunk: HashMap::new(),
            manifests_by_chunk: HashMap::new(),
            grants: HashMap::new(),
            pending_fills: HashMap::new(),
            bytes_served: 0,
        }
    }

    /// Recompute every derived index from `self.shares`.
    fn rebuild_indexes(&mut self) {
        self.lists_by_root.clear();
        self.manifest_by_root.clear();
        self.chunk_len.clear();
        self.chunk_upstreams.clear();
        self.path_by_root.clear();
        self.paths_by_chunk.clear();
        self.manifests_by_chunk.clear();

        for (mid, entry) in &self.shares {
            let path_of_root: HashMap<Hash, &str> =
                entry.manifest.files.iter().map(|f| (f.root, f.path.as_str())).collect();
            for list in &entry.lists {
                let root = list.root();
                self.manifest_by_root.insert(root, *mid);
                let path = path_of_root.get(&root).map(|p| p.to_string());
                if let Some(path) = &path {
                    self.path_by_root.insert(root, path.clone());
                }
                for chunk in &list.chunks {
                    self.chunk_len.insert(chunk.hash, chunk.len);
                    self.chunk_upstreams
                        .entry(chunk.hash)
                        .or_default()
                        .extend(entry.upstreams.iter().copied());
                    let mids = self.manifests_by_chunk.entry(chunk.hash).or_default();
                    if !mids.contains(mid) {
                        mids.push(*mid);
                    }
                    if let Some(path) = &path {
                        let paths = self.paths_by_chunk.entry(chunk.hash).or_default();
                        if !paths.contains(path) {
                            paths.push(path.clone());
                        }
                    }
                }
                self.lists_by_root.insert(root, list.clone());
            }
        }
        // De-duplicate upstreams per chunk.
        for ups in self.chunk_upstreams.values_mut() {
            let mut seen = HashSet::new();
            ups.retain(|p| seen.insert(*p));
        }
    }

    /// Does this relay gate at least one cached share?
    fn any_private(&self) -> bool {
        self.shares.values().any(|s| s.restrict.is_some())
    }

    /// Live (non-expired) grants `peer` holds.
    fn live_grants(&self, peer: &PeerId, now: u64) -> impl Iterator<Item = &RelayGrant> {
        self.grants.get(peer).into_iter().flatten().filter(move |g| g.live(now))
    }

    /// May `peer` see the manifest / chunk lists of the cached share `mid`?
    fn manifest_access(&self, mid: &Hash, peer: &PeerId) -> bool {
        match self.shares.get(mid) {
            None => false,
            Some(entry) => match entry.restrict {
                None => true,
                Some(pk) => {
                    let now = unix_now();
                    self.live_grants(peer, now).any(|g| g.share == pk)
                }
            },
        }
    }

    /// May `peer` fetch `path` within the cached share `mid`?
    fn path_allowed_in(&self, mid: &Hash, path: &str, peer: &PeerId) -> bool {
        match self.shares.get(mid).and_then(|e| e.restrict) {
            None => true,
            Some(pk) => {
                let now = unix_now();
                self.live_grants(peer, now).any(|g| g.share == pk && g.scope.allows(path))
            }
        }
    }

    /// May `peer` fetch this chunk? Allowed if the chunk belongs to any public
    /// cached share, or to a private one `peer` holds a scope-matching grant for.
    fn chunk_allowed(&self, hash: &Hash, peer: &PeerId) -> bool {
        let Some(mids) = self.manifests_by_chunk.get(hash) else { return false };
        let now = unix_now();
        for mid in mids {
            let Some(entry) = self.shares.get(mid) else { continue };
            match entry.restrict {
                None => return true,
                Some(pk) => {
                    let chunk_paths = self.paths_by_chunk.get(hash);
                    for g in self.live_grants(peer, now) {
                        if g.share != pk {
                            continue;
                        }
                        match &g.scope {
                            Scope::All => return true,
                            Scope::Files(_) => {
                                if chunk_paths
                                    .is_some_and(|ps| ps.iter().any(|p| g.scope.allows(p)))
                                {
                                    return true;
                                }
                            }
                        }
                    }
                }
            }
        }
        false
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
                let mid = manifest.id();
                tracing::info!(
                    share = %mid,
                    files = manifest.files.len(),
                    chunk_lists = chunk_lists.len(),
                    upstreams = upstreams.len(),
                    "caching share"
                );
                match self.shares.get_mut(&mid) {
                    Some(entry) => {
                        entry.manifest = *manifest;
                        entry.lists = chunk_lists;
                        for up in upstreams {
                            if !entry.upstreams.contains(&up) {
                                entry.upstreams.push(up);
                            }
                        }
                    }
                    None => {
                        self.shares.insert(
                            mid,
                            ShareEntry {
                                manifest: *manifest,
                                lists: chunk_lists,
                                upstreams,
                                restrict: None,
                            },
                        );
                    }
                }
                self.rebuild_indexes();
            }
            Command::RemoveShare(mid) => {
                if self.shares.remove(&mid).is_some() {
                    tracing::info!(share = %mid, "dropped cached share");
                    self.rebuild_indexes();
                }
            }
            Command::Shares(reply) => {
                let _ = reply.send(self.shares.keys().copied().collect());
            }
            Command::CacheStats(reply) => {
                let mut stats = self.cache.stats();
                stats.bytes_served = self.bytes_served;
                let _ = reply.send(stats);
            }
            Command::Restrict { share, manifest_id } => {
                match self.shares.get_mut(&manifest_id) {
                    Some(entry) => {
                        entry.restrict = Some(share);
                        // Drop any stale grants for this key so the gate is clean.
                        for grants in self.grants.values_mut() {
                            grants.retain(|g| g.share != share);
                        }
                        tracing::info!(%share, share_id = %manifest_id, "share is now invite-only");
                    }
                    None => tracing::warn!(
                        share_id = %manifest_id,
                        "restrict_to_invite_holders for a share this relay does not cache"
                    ),
                }
            }
            Command::Unrestrict(share) => {
                for entry in self.shares.values_mut() {
                    if entry.restrict == Some(share) {
                        entry.restrict = None;
                    }
                }
                for grants in self.grants.values_mut() {
                    grants.retain(|g| g.share != share);
                }
                tracing::info!(%share, "share restriction lifted");
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
                if let Response::Chunk(data) = &forwarded {
                    self.bytes_served += data.len() as u64;
                }
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
            let resp = if self.any_private() {
                match self.admit(peer, cred) {
                    Ok(()) => Response::Welcome,
                    Err(why) => Response::Unauthorized(why),
                }
            } else {
                Response::Welcome
            };
            let _ = self.swarm.behaviour_mut().chunk_exchange.send_response(channel, resp);
            return;
        }

        let response = match request {
            Request::Hello(_) => unreachable!("handled above"),
            Request::GetManifest(sel) => {
                match self.resolve_share(sel) {
                    None => Response::NotFound,
                    Some(mid) if !self.manifest_access(&mid, &peer) => {
                        Response::Unauthorized("present a valid invite first".into())
                    }
                    Some(mid) => Response::Manifest(self.shares[&mid].manifest.clone()),
                }
            }
            Request::GetChunkList(root) => match self.manifest_by_root.get(&root).copied() {
                None => Response::NotFound,
                Some(mid) if !self.manifest_access(&mid, &peer) => {
                    Response::Unauthorized("present a valid invite first".into())
                }
                Some(mid) => match self.path_by_root.get(&root) {
                    Some(path) if !self.path_allowed_in(&mid, path, &peer) => {
                        Response::Unauthorized("this file is outside your invite".into())
                    }
                    _ => self
                        .lists_by_root
                        .get(&root)
                        .cloned()
                        .map_or(Response::NotFound, Response::ChunkList),
                },
            },
            Request::GetInventory => {
                let held: Vec<Hash> = self
                    .chunk_len
                    .keys()
                    .copied()
                    .filter(|h| self.cache.contains(h) && self.chunk_allowed(h, &peer))
                    .collect();
                Response::Inventory(held)
            }
            Request::GetChunk(hash) => {
                if !self.chunk_allowed(&hash, &peer) {
                    Response::Unauthorized("this chunk is outside your invite".into())
                } else if let Some(bytes) = self.cache.get_refreshing(&hash) {
                    self.bytes_served += bytes.len() as u64;
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

    /// Which cached share a `GetManifest(sel)` targets: the selected id if it is
    /// cached, or the sole cached share when `sel` is `None`.
    fn resolve_share(&self, sel: Option<Hash>) -> Option<Hash> {
        match sel {
            Some(id) => self.shares.contains_key(&id).then_some(id),
            None => (self.shares.len() == 1).then(|| *self.shares.keys().next().unwrap()),
        }
    }

    /// Verify a presented capability and record a grant for the private share it
    /// unlocks. `Err` carries a human-readable reason.
    fn admit(&mut self, peer: PeerId, cred: &gaggle_core::SignedCapability) -> Result<(), String> {
        let cap = cred.verify(unix_now()).map_err(|e| e.to_string())?;
        let gated = self
            .shares
            .get(&cap.manifest_id)
            .filter(|e| e.restrict == Some(cap.share))
            .is_some();
        if !gated {
            return Err(
                "capability is for a share/manifest this relay does not gate for invites".into(),
            );
        }
        let grants = self.grants.entry(peer).or_default();
        grants.retain(|g| g.share != cap.share);
        grants.push(RelayGrant {
            share: cap.share,
            scope: cap.scope.clone(),
            expires_at: cap.expires_at,
        });
        Ok(())
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
