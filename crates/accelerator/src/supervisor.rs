//! The `Supervisor` owns the running accelerator (a multi-share [`RelayNode`],
//! or one serving [`Node`] per replicated share) and applies every mutation
//! that arrives from the admin API, republishing a [`DaemonStatus`] after each
//! and rewriting `config.toml` so the change survives a restart.
//!
//! A NAS share's replication runs in the background (`tokio::spawn`), not
//! inline in the command loop: pulling a 100 GB share must not block adding or
//! removing any other share, or answering `GET /admin/status`, for however
//! long that takes. Progress lands via an internal `events` channel the main
//! loop selects on alongside admin commands, so `GET /admin/status` shows a
//! live [`ShareStatus::replicating`] the whole time. The share's token is
//! persisted to `config.toml` as soon as it's accepted (not once it finishes),
//! so an add that outlives this process still resumes on the next start —
//! matching how a share that fails to start at boot is already retried every
//! restart until an operator explicitly removes it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Context;
use control_plane::admin::{AdminCommand, DaemonStatus, ReplicationProgress, ShareStatus};
use control_plane::{PeerInfo, TrackerRegistry};
use gaggle_core::{AgentKeypair, Hash};
use net::accel::{ShareMeta, nas_add_share_with_progress, relay_add_share};
use net::{Keypair, Multiaddr, Node, RelayConfig, RelayNode, ShareLink, SwarmProgress};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::AbortHandle;
use tokio::time::MissedTickBehavior;

use crate::config::{AcceleratorConfig, Home, Role};

/// How often a replicating share's progress may update `DaemonStatus` — a
/// share with many small chunks must not flood the status watch channel.
const PROGRESS_THROTTLE: Duration = Duration::from_millis(500);

/// How often every ready share is (re-)announced to the seeder tracker so
/// downloaders pointed at this daemon discover it as a source. Comfortably
/// under `control_plane::tracker`'s entry TTL.
const TRACKER_ANNOUNCE_INTERVAL: Duration = Duration::from_secs(30);

/// A share the daemon is accelerating, or in the middle of adding.
enum ShareRecord {
    /// NAS role only: replication under way in the background.
    Replicating {
        token: String,
        name_hint: String,
        progress: Option<ReplicationProgress>,
        abort: AbortHandle,
    },
    Ready {
        token: String,
        meta: ShareMeta,
        /// NAS role only: the node serving this share (drop = stop). `None`
        /// for a relay-cached share.
        node: Option<Node>,
        replica_chunks: Option<usize>,
        listen_addr: Option<String>,
    },
    Failed { token: String, name_hint: String, error: String },
}

impl ShareRecord {
    fn token(&self) -> &str {
        match self {
            ShareRecord::Replicating { token, .. }
            | ShareRecord::Ready { token, .. }
            | ShareRecord::Failed { token, .. } => token,
        }
    }
}

enum Backend {
    Relay { relay: RelayNode, meta_node: Node, listen_addrs: Vec<String> },
    Nas { dir_root: PathBuf },
}

/// A background NAS replication reporting back to the main loop.
enum ShareEvent {
    Progress { manifest_id: Hash, progress: ReplicationProgress },
    Ready {
        manifest_id: Hash,
        meta: ShareMeta,
        node: Node,
        chunks: usize,
        listen_addr: Option<String>,
        ack: oneshot::Sender<Result<(), String>>,
    },
    Failed { manifest_id: Hash, error: String, ack: oneshot::Sender<Result<(), String>> },
}

pub struct Supervisor {
    home: Home,
    config: AcceleratorConfig,
    daemon: AgentKeypair,
    identity: Keypair,
    backend: Backend,
    shares: HashMap<Hash, ShareRecord>,
    status_tx: watch::Sender<DaemonStatus>,
    events_tx: mpsc::Sender<ShareEvent>,
    events_rx: mpsc::Receiver<ShareEvent>,
    tracker: TrackerRegistry,
    /// Cumulative chunk bytes served to downloaders, refreshed by
    /// [`refresh_served`](Self::refresh_served) before every [`publish`](Self::publish)
    /// (the actual counters live on the `net` nodes and are read async).
    last_served: u64,
}

impl Supervisor {
    /// Start the backend for `config.role`, cache/replicate every configured
    /// share, and return the running supervisor plus a `watch` handle the admin
    /// router reads status from.
    pub async fn start(
        home: Home,
        config: AcceleratorConfig,
        daemon: AgentKeypair,
        identity: Keypair,
        tracker: TrackerRegistry,
    ) -> anyhow::Result<(Self, watch::Receiver<DaemonStatus>)> {
        let listen = config.listen_addr()?;
        let backend = match config.role {
            Role::Relay => {
                let relay = RelayNode::spawn_with_opts(
                    RelayConfig { cache_capacity_bytes: config.cache_mib * 1024 * 1024 },
                    Some(identity.clone()),
                    listen,
                )
                .await?;
                let listen_addrs: Vec<String> = relay
                    .listen_addr()
                    .await
                    .map(|a| vec![a.to_string()])
                    .unwrap_or_default();
                for addr in &listen_addrs {
                    tracing::info!(%addr, "relay listening");
                }
                Backend::Relay { relay, meta_node: Node::spawn().await?, listen_addrs }
            }
            Role::Nas => {
                let dir_root = config.resolved_replica_dir(&home);
                std::fs::create_dir_all(&dir_root)
                    .with_context(|| format!("creating {}", dir_root.display()))?;
                Backend::Nas { dir_root }
            }
        };

        let (status_tx, status_rx) = watch::channel(DaemonStatus::default());
        let (events_tx, events_rx) = mpsc::channel(64);
        let mut sup = Self {
            home,
            config,
            daemon,
            identity,
            backend,
            shares: HashMap::new(),
            status_tx,
            events_tx,
            events_rx,
            tracker,
            last_served: 0,
        };

        let tokens = sup.config.shares.clone();
        for token in tokens {
            let (ack, _rx) = oneshot::channel();
            sup.add_share(token, ack).await;
        }
        sup.refresh_served().await;
        sup.publish();
        Ok((sup, status_rx))
    }

    /// Drive the supervisor: apply admin mutations and background replication
    /// events until the admin command channel closes.
    pub async fn run(mut self, mut commands: mpsc::Receiver<AdminCommand>) {
        let mut announce = tokio::time::interval(TRACKER_ANNOUNCE_INTERVAL);
        announce.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                cmd = commands.recv() => {
                    let Some(cmd) = cmd else { break };
                    match cmd {
                        AdminCommand::AddShare { token, ack } => self.add_share(token, ack).await,
                        AdminCommand::RemoveShare { manifest_id, ack } => {
                            let result =
                                self.remove_share(&manifest_id).await.map_err(|e| format!("{e:#}"));
                            let _ = ack.send(result);
                        }
                    }
                    self.refresh_served().await;
                    self.publish();
                    self.announce_to_tracker().await;
                }
                Some(event) = self.events_rx.recv() => {
                    self.handle_event(event);
                    self.refresh_served().await;
                    self.publish();
                    self.announce_to_tracker().await;
                }
                _ = announce.tick() => {
                    self.refresh_served().await;
                    self.publish();
                    self.announce_to_tracker().await;
                }
            }
        }
    }

    /// (Re-)publish every ready share to the seeder tracker so a downloader
    /// pointed at this daemon's control-plane URL discovers it as a source,
    /// not only whatever address is in its share link. Best-effort: the
    /// registry is in-process, so this can't fail, but a share with no
    /// reachable address yet is simply skipped until it has one.
    async fn announce_to_tracker(&self) {
        match &self.backend {
            Backend::Relay { relay, .. } => {
                let addrs = relay.reachable_addrs().await.unwrap_or_default();
                if addrs.is_empty() {
                    return;
                }
                let me = peer_info(relay.peer_id().to_string(), &addrs);
                for (id, record) in &self.shares {
                    if matches!(record, ShareRecord::Ready { .. }) {
                        self.tracker.announce(&id.to_hex(), me.clone());
                    }
                }
            }
            Backend::Nas { .. } => {
                for (id, record) in &self.shares {
                    let ShareRecord::Ready { node: Some(node), .. } = record else { continue };
                    let addrs = node.reachable_addrs().await.unwrap_or_default();
                    if addrs.is_empty() {
                        continue;
                    }
                    self.tracker
                        .announce(&id.to_hex(), peer_info(node.peer_id().to_string(), &addrs));
                }
            }
        }
    }

    /// Accept `token`: for a relay it caches (fast, so this awaits fully and
    /// acks immediately); for a NAS it spawns the replication in the
    /// background and acks once [`ShareEvent::Ready`] / [`ShareEvent::Failed`]
    /// lands, so other admin commands are never blocked behind it.
    async fn add_share(&mut self, token: String, ack: oneshot::Sender<Result<(), String>>) {
        let link = match ShareLink::parse(&token) {
            Ok(l) => l,
            Err(e) => {
                let _ = ack.send(Err(format!("parsing the share link: {e:#}")));
                return;
            }
        };
        if self.shares.contains_key(&link.manifest_id) {
            let _ = ack.send(Err("that share is already being accelerated".into()));
            return;
        }

        match &self.backend {
            Backend::Relay { relay, meta_node, .. } => match relay_add_share(relay, meta_node, &link).await
            {
                Ok(meta) => {
                    tracing::info!(share = %meta.manifest_id, name = %meta.name, files = meta.files, "accelerating share");
                    self.shares.insert(
                        link.manifest_id,
                        ShareRecord::Ready { token, meta, node: None, replica_chunks: None, listen_addr: None },
                    );
                    self.persist();
                    let _ = ack.send(Ok(()));
                }
                Err(e) => {
                    let _ = ack.send(Err(format!("{e:#}")));
                }
            },
            Backend::Nas { dir_root } => {
                let dir_root = dir_root.clone();
                let seed = match share_identity_seed(&self.identity, link.manifest_id) {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = ack.send(Err(format!("{e:#}")));
                        return;
                    }
                };
                let identity = net::keypair_from_seed(seed);
                let manifest_id = link.manifest_id;
                let name_hint = link.name.clone();
                let events_tx = self.events_tx.clone();

                let task = tokio::spawn(async move {
                    let mut last_sent = Instant::now()
                        .checked_sub(PROGRESS_THROTTLE)
                        .unwrap_or_else(Instant::now);
                    let progress_tx = events_tx.clone();
                    let result = nas_add_share_with_progress(&dir_root, identity, &link, |p: SwarmProgress| {
                        let done = p.chunks_done >= p.chunks_total;
                        if done || last_sent.elapsed() >= PROGRESS_THROTTLE {
                            last_sent = Instant::now();
                            let _ = progress_tx.try_send(ShareEvent::Progress {
                                manifest_id,
                                progress: ReplicationProgress {
                                    chunks_done: p.chunks_done,
                                    chunks_total: p.chunks_total,
                                    bytes_done: p.bytes_done,
                                    bytes_total: p.bytes_total,
                                },
                            });
                        }
                    })
                    .await;
                    match result {
                        Ok((node, meta, chunks)) => {
                            let listen_addr = node.listen_addr().await.ok().map(|a| a.to_string());
                            let _ = events_tx
                                .send(ShareEvent::Ready { manifest_id, meta, node, chunks, listen_addr, ack })
                                .await;
                        }
                        Err(e) => {
                            let _ = events_tx
                                .send(ShareEvent::Failed { manifest_id, error: format!("{e:#}"), ack })
                                .await;
                        }
                    }
                });

                self.shares.insert(
                    manifest_id,
                    ShareRecord::Replicating { token, name_hint, progress: None, abort: task.abort_handle() },
                );
                self.persist();
            }
        }
    }

    fn handle_event(&mut self, event: ShareEvent) {
        match event {
            ShareEvent::Progress { manifest_id, progress } => {
                if let Some(ShareRecord::Replicating { progress: p, .. }) = self.shares.get_mut(&manifest_id)
                {
                    *p = Some(progress);
                }
            }
            ShareEvent::Ready { manifest_id, meta, node, chunks, listen_addr, ack } => {
                tracing::info!(share = %manifest_id, name = %meta.name, files = meta.files, "accelerating share");
                let token = self.shares.get(&manifest_id).map(|r| r.token().to_string()).unwrap_or_default();
                self.shares.insert(
                    manifest_id,
                    ShareRecord::Ready {
                        token,
                        meta,
                        node: Some(node),
                        replica_chunks: Some(chunks),
                        listen_addr,
                    },
                );
                let _ = ack.send(Ok(()));
            }
            ShareEvent::Failed { manifest_id, error, ack } => {
                tracing::warn!(share = %manifest_id, %error, "could not replicate share");
                if let Some(token) = self.shares.get(&manifest_id).map(|r| r.token().to_string()) {
                    self.shares.insert(
                        manifest_id,
                        ShareRecord::Failed { token, name_hint: String::new(), error: error.clone() },
                    );
                } else {
                    self.shares.remove(&manifest_id);
                }
                let _ = ack.send(Err(error));
            }
        }
    }

    /// Stop accelerating a share (aborting an in-flight replication, or
    /// shutting down its serving node) — the on-disk replica bytes are kept,
    /// so re-adding the same share resumes rather than starting over.
    async fn remove_share(&mut self, manifest_id: &str) -> anyhow::Result<()> {
        let id = Hash::from_hex(manifest_id.trim())
            .map_err(|_| anyhow::anyhow!("not a manifest id: {manifest_id:?}"))?;
        let record = self.shares.remove(&id).context("no such accelerated share")?;

        match record {
            ShareRecord::Replicating { abort, .. } => abort.abort(),
            ShareRecord::Ready { node: Some(node), .. } => {
                // Leave the tracker's seeder list right away rather than
                // lingering as a dead address until the TTL expires.
                self.tracker.withdraw(&id.to_hex(), &node.peer_id().to_string());
                node.shutdown().await;
            }
            ShareRecord::Ready { node: None, .. } => {
                if let Backend::Relay { relay, .. } = &self.backend {
                    self.tracker.withdraw(&id.to_hex(), &relay.peer_id().to_string());
                    relay.remove_share(id).await.ok();
                }
            }
            ShareRecord::Failed { .. } => {}
        }
        tracing::info!(share = %id, "stopped accelerating share");
        self.persist();
        Ok(())
    }

    /// Read the cumulative served-bytes counters off the running `net` nodes
    /// (a relay's own forwarded-bytes total, or the sum across every NAS
    /// share's serving node) into `self.last_served`, so the next sync
    /// [`publish`](Self::publish) can report it in [`DaemonStatus`].
    async fn refresh_served(&mut self) {
        self.last_served = match &self.backend {
            Backend::Relay { relay, .. } => {
                relay.cache_stats().await.map(|s| s.bytes_served).unwrap_or(self.last_served)
            }
            Backend::Nas { .. } => {
                let mut total = 0u64;
                for record in self.shares.values() {
                    if let ShareRecord::Ready { node: Some(node), .. } = record
                        && let Ok(s) = node.serve_stats().await
                    {
                        total += s.bytes_served;
                    }
                }
                total
            }
        };
    }

    fn persist(&mut self) {
        self.config.shares = self.shares.values().map(|r| r.token().to_string()).collect();
        self.config.shares.sort();
        if let Err(e) = self.config.save(&self.home.config_path()) {
            tracing::warn!(error = %format!("{e:#}"), "could not rewrite config.toml");
        }
    }

    fn publish(&self) {
        let listen_addrs = match &self.backend {
            Backend::Relay { listen_addrs, .. } => listen_addrs.clone(),
            Backend::Nas { .. } => Vec::new(),
        };

        let mut shares: Vec<ShareStatus> = self
            .shares
            .iter()
            .map(|(id, r)| match r {
                ShareRecord::Replicating { name_hint, progress, .. } => ShareStatus {
                    manifest_id: id.to_hex(),
                    name: name_hint.clone(),
                    files: 0,
                    total_bytes: 0,
                    version: 0,
                    private: false,
                    cached_chunks: None,
                    replica_chunks: None,
                    listen_addr: None,
                    replicating: progress.clone(),
                    error: None,
                },
                ShareRecord::Ready { meta, replica_chunks, listen_addr, .. } => ShareStatus {
                    manifest_id: meta.manifest_id.to_hex(),
                    name: meta.name.clone(),
                    files: meta.files,
                    total_bytes: meta.total_bytes,
                    version: meta.version,
                    private: meta.private,
                    cached_chunks: None,
                    replica_chunks: replica_chunks.map(|n| n as u64),
                    listen_addr: listen_addr.clone(),
                    replicating: None,
                    error: None,
                },
                ShareRecord::Failed { name_hint, error, .. } => ShareStatus {
                    manifest_id: id.to_hex(),
                    name: name_hint.clone(),
                    files: 0,
                    total_bytes: 0,
                    version: 0,
                    private: false,
                    cached_chunks: None,
                    replica_chunks: None,
                    listen_addr: None,
                    replicating: None,
                    error: Some(error.clone()),
                },
            })
            .collect();
        shares.sort_by(|a, b| a.name.cmp(&b.name));

        let status = DaemonStatus {
            agent_id: self.daemon.public().to_hex(),
            peer_id: self.identity.public().to_peer_id().to_string(),
            role: self.config.role.as_str().to_string(),
            listen_addrs,
            shares,
            bytes_served_total: Some(self.last_served),
        };
        let _ = self.status_tx.send(status);
    }
}

/// A `PeerInfo` for the seeder tracker from a peer id and its dialable
/// addresses.
fn peer_info(peer_id: String, addrs: &[Multiaddr]) -> PeerInfo {
    PeerInfo { peer_id, addrs: addrs.iter().map(Multiaddr::to_string).collect() }
}

/// Deterministic per-share identity seed: `blake3(daemon-seed ++ manifest-id)`.
fn share_identity_seed(identity: &Keypair, manifest_id: Hash) -> anyhow::Result<[u8; 32]> {
    let daemon_seed = net::identity_seed(identity)?;
    let mut buf = Vec::with_capacity(64);
    buf.extend_from_slice(&daemon_seed);
    buf.extend_from_slice(manifest_id.as_bytes());
    Ok(*Hash::of(&buf).as_bytes())
}
