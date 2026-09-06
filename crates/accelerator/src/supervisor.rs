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
use control_plane::{PeerInfo, RendezvousClient, TrackerClient, TrackerRegistry};
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

/// How often the daemon checks the external rendezvous point (when
/// `config.rendezvous_url` is set) for subscribers waiting to NAT-punch
/// through to one of its served shares. Short — a subscriber only waits a
/// few seconds for the answer before falling back to the share link.
const RENDEZVOUS_ANSWER_INTERVAL: Duration = Duration::from_secs(2);

/// Upper bound on one rendezvous-answer sweep (and on each per-share request
/// within it), so a slow/hung rendezvous server can never wedge the command
/// loop — status still flows over its own `watch` channel regardless, and
/// the next sweep retries in [`RENDEZVOUS_ANSWER_INTERVAL`].
const RENDEZVOUS_SWEEP_TIMEOUT: Duration = Duration::from_secs(3);
const RENDEZVOUS_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

/// Cap on one punch-dial. Its job is just to get the outbound packet out
/// (which happens the moment the dial is issued) — a dead subscriber address
/// must not stall the sweep waiting for a connection that will never form.
const PUNCH_DIAL_TIMEOUT: Duration = Duration::from_millis(600);

/// Cap on reserving a circuit on `config.public_relay` when a NAS replica
/// comes up — an unreachable relay must not hold the share short of `Ready`
/// (it still serves on its own listen addresses without a circuit).
const RELAY_RESERVE_TIMEOUT: Duration = Duration::from_secs(20);

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
        /// NAS role: a `/p2p-circuit/…` address reserved on `config.public_relay`,
        /// advertised alongside the node's own listen addresses so a NAT'd
        /// replica is still reachable. `None` when no relay is configured or
        /// the reservation failed.
        circuit_addr: Option<String>,
    },
    /// An operator paused this share: the token stays in config and (NAS) the
    /// replica stays on disk, but nothing serves it until it is resumed.
    Paused { token: String, name_hint: String, meta: Option<ShareMeta> },
    Failed { token: String, name_hint: String, error: String },
}

impl ShareRecord {
    fn token(&self) -> &str {
        match self {
            ShareRecord::Replicating { token, .. }
            | ShareRecord::Ready { token, .. }
            | ShareRecord::Paused { token, .. }
            | ShareRecord::Failed { token, .. } => token,
        }
    }
}

enum Backend {
    Relay { relay: RelayNode, meta_node: Node, listen_addrs: Vec<String> },
    Nas { dir_root: PathBuf, compress: bool },
}

/// A background NAS replication reporting back to the main loop.
enum ShareEvent {
    Progress { manifest_id: Hash, progress: ReplicationProgress },
    Ready {
        manifest_id: Hash,
        meta: ShareMeta,
        // Boxed: much larger than the other variants, and it only moves
        // through the channel once per share.
        node: Box<Node>,
        chunks: usize,
        listen_addr: Option<String>,
        circuit_addr: Option<String>,
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
    /// NAS role: per-share on-disk replica size, refreshed alongside
    /// `last_served`.
    disk_bytes: HashMap<Hash, u64>,
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
                Backend::Nas { dir_root, compress: config.compress_replica }
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
            disk_bytes: HashMap::new(),
        };

        let tokens = sup.config.shares.clone();
        let paused = sup.config.paused_shares.clone();
        for token in tokens {
            let link = ShareLink::parse(&token).ok();
            let is_paused = link
                .as_ref()
                .is_some_and(|l| paused.iter().any(|p| p == &l.manifest_id.to_hex()));
            if is_paused && let Some(l) = link {
                // Don't serve (or, for NAS, re-replicate) a paused share on
                // boot — just record it so status shows it and it can resume.
                sup.shares.insert(
                    l.manifest_id,
                    ShareRecord::Paused { token, name_hint: l.name.clone(), meta: None },
                );
                continue;
            }
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
        let mut punch = tokio::time::interval(RENDEZVOUS_ANSWER_INTERVAL);
        punch.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                cmd = commands.recv() => {
                    let Some(cmd) = cmd else { break };
                    match cmd {
                        AdminCommand::AddShare { token, ack } => self.add_share(token, ack).await,
                        AdminCommand::RemoveShare { manifest_id, keep_data, ack } => {
                            let result = self
                                .remove_share(&manifest_id, keep_data)
                                .await
                                .map_err(|e| format!("{e:#}"));
                            let _ = ack.send(result);
                        }
                        AdminCommand::SetSeeding { manifest_id, seeding, ack } => {
                            let result = self
                                .set_seeding(&manifest_id, seeding)
                                .await
                                .map_err(|e| format!("{e:#}"));
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
                _ = punch.tick() => {
                    let _ = tokio::time::timeout(
                        RENDEZVOUS_SWEEP_TIMEOUT,
                        self.answer_rendezvous(),
                    )
                    .await;
                }
            }
        }
    }

    /// Answer NAT-rendezvous punch requests aimed at any share this daemon
    /// serves, via the external accelerator in `config.rendezvous_url`. NAS
    /// role only — a relay is a libp2p relay server, expected to be publicly
    /// reachable already. Silent on error: this is a best-effort optimization
    /// on top of the addresses already in the share link / tracker.
    async fn answer_rendezvous(&self) {
        let Some(url) = self.config.rendezvous_url.as_deref() else { return };
        if !matches!(self.backend, Backend::Nas { .. }) {
            return;
        }
        let ready = self
            .shares
            .values()
            .any(|r| matches!(r, ShareRecord::Ready { node: Some(_), .. }));
        if !ready {
            return;
        }
        let client = RendezvousClient::new(url);
        for record in self.shares.values() {
            let ShareRecord::Ready { node: Some(node), circuit_addr, .. } = record else { continue };
            match tokio::time::timeout(
                RENDEZVOUS_REQUEST_TIMEOUT,
                answer_punch_requests(&client, node, circuit_addr.as_deref()),
            )
            .await
            {
                Ok(Err(e)) => tracing::debug!(error = %format!("{e:#}"), "rendezvous answer failed"),
                Err(_) => tracing::debug!("rendezvous answer timed out"),
                Ok(Ok(())) => {}
            }
        }
    }

    /// (Re-)publish every ready share to the seeder tracker so a downloader
    /// discovers it as a source, not only whatever address is in its share
    /// link. Always announces to this daemon's own in-process tracker (a
    /// downloader pointed straight at this daemon); when `config.rendezvous_url`
    /// is set, also announces over HTTP to that *external* accelerator's
    /// tracker, so a downloader pointed at a public relay finds this
    /// (possibly private / NAT'd) daemon too. Best-effort — a share with no
    /// reachable address yet is skipped until it has one.
    async fn announce_to_tracker(&self) {
        let remote = self.config.rendezvous_url.as_deref().map(TrackerClient::new);
        match &self.backend {
            Backend::Relay { relay, .. } => {
                let addrs = relay.reachable_addrs().await.unwrap_or_default();
                if addrs.is_empty() {
                    return;
                }
                let me = peer_info(relay.peer_id().to_string(), &addrs);
                for (id, record) in &self.shares {
                    if matches!(record, ShareRecord::Ready { .. }) {
                        let (name, private) = link_meta(record.token());
                        self.announce_one(remote.as_ref(), id, &me, name.as_deref(), private).await;
                    }
                }
            }
            Backend::Nas { .. } => {
                for (id, record) in &self.shares {
                    let ShareRecord::Ready { node: Some(node), circuit_addr, .. } = record else {
                        continue;
                    };
                    let mut addrs = node.reachable_addrs().await.unwrap_or_default();
                    if let Some(circuit) =
                        circuit_addr.as_deref().and_then(|s| s.parse::<Multiaddr>().ok())
                        && !addrs.contains(&circuit)
                    {
                        addrs.push(circuit);
                    }
                    if addrs.is_empty() {
                        continue;
                    }
                    let (name, private) = link_meta(record.token());
                    self.announce_one(
                        remote.as_ref(),
                        id,
                        &peer_info(node.peer_id().to_string(), &addrs),
                        name.as_deref(),
                        private,
                    )
                    .await;
                }
            }
        }
    }

    /// Announce one share/`PeerInfo` pair to the in-process tracker (a
    /// same-machine downloader may use a loopback address) and, when
    /// configured, the external one — the latter with loopback addresses
    /// stripped, since a remote peer can only waste a dial on them.
    async fn announce_one(
        &self,
        remote: Option<&TrackerClient>,
        id: &Hash,
        me: &PeerInfo,
        name: Option<&str>,
        private: bool,
    ) {
        self.tracker.announce_with_meta(
            &id.to_hex(),
            me.clone(),
            name.map(str::to_string),
            private,
        );
        if let Some(client) = remote {
            let far = PeerInfo {
                peer_id: me.peer_id.clone(),
                addrs: me
                    .addrs
                    .iter()
                    .filter(|a| {
                        a.parse::<Multiaddr>().map(|m| !net::addr_is_loopback(&m)).unwrap_or(false)
                    })
                    .cloned()
                    .collect(),
            };
            if !far.addrs.is_empty() {
                let _ = client.announce_share(&id.to_hex(), &far, name, private).await;
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
                        ShareRecord::Ready {
                            token,
                            meta,
                            node: None,
                            replica_chunks: None,
                            listen_addr: None,
                            circuit_addr: None,
                        },
                    );
                    self.persist();
                    let _ = ack.send(Ok(()));
                }
                Err(e) => {
                    let _ = ack.send(Err(format!("{e:#}")));
                }
            },
            Backend::Nas { dir_root, compress } => {
                let dir_root = dir_root.clone();
                let compress = *compress;
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
                let public_relay = self.config.public_relay.clone();

                let task = tokio::spawn(async move {
                    let mut last_sent = Instant::now()
                        .checked_sub(PROGRESS_THROTTLE)
                        .unwrap_or_else(Instant::now);
                    let progress_tx = events_tx.clone();
                    let result = nas_add_share_with_progress(&dir_root, identity, &link, compress, |p: SwarmProgress| {
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
                            // Reserve a circuit on the configured relay so a
                            // NAT'd replica is dialable through it (dcutr then
                            // upgrades to direct). Best-effort — a share still
                            // serves on its listen addresses without it.
                            let circuit_addr = match &public_relay {
                                Some(relay_addr) => match tokio::time::timeout(
                                    RELAY_RESERVE_TIMEOUT,
                                    reserve_relay_circuit(&node, relay_addr),
                                )
                                .await
                                {
                                    Ok(Ok(c)) => Some(c.to_string()),
                                    Ok(Err(e)) => {
                                        tracing::warn!(
                                            share = %manifest_id, error = %format!("{e:#}"),
                                            "NAS replica could not reserve a relay circuit"
                                        );
                                        None
                                    }
                                    Err(_) => {
                                        tracing::warn!(
                                            share = %manifest_id,
                                            "NAS replica relay-circuit reservation timed out"
                                        );
                                        None
                                    }
                                },
                                None => None,
                            };
                            let _ = events_tx
                                .send(ShareEvent::Ready {
                                    manifest_id,
                                    meta,
                                    node: Box::new(node),
                                    chunks,
                                    listen_addr,
                                    circuit_addr,
                                    ack,
                                })
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
            ShareEvent::Ready { manifest_id, meta, node, chunks, listen_addr, circuit_addr, ack } => {
                // Paused while its replication was still running: keep the
                // fresh replica on disk but don't start serving it.
                if let Some(ShareRecord::Paused { .. }) = self.shares.get(&manifest_id) {
                    tracing::info!(share = %manifest_id, "replication finished for a paused share — not serving");
                    tokio::spawn(async move { node.shutdown().await });
                    let _ = ack.send(Ok(()));
                    return;
                }
                tracing::info!(share = %manifest_id, name = %meta.name, files = meta.files, "accelerating share");
                let token = self.shares.get(&manifest_id).map(|r| r.token().to_string()).unwrap_or_default();
                self.shares.insert(
                    manifest_id,
                    ShareRecord::Ready {
                        token,
                        meta,
                        node: Some(*node),
                        replica_chunks: Some(chunks),
                        listen_addr,
                        circuit_addr,
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
    /// shutting down its serving node). Unless `keep_data` is set, a NAS
    /// replica's on-disk chunk directory is deleted too — `keep_data` (the
    /// admin API's `?keep_data=1`) preserves it so a later re-add resumes
    /// rather than starting over.
    async fn remove_share(&mut self, manifest_id: &str, keep_data: bool) -> anyhow::Result<()> {
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
            // Paused: nothing is serving it, so there's nothing to stop — the
            // on-disk replica is dealt with below like any other NAS share.
            ShareRecord::Paused { .. } | ShareRecord::Failed { .. } => {}
        }

        if !keep_data
            && let Backend::Nas { dir_root, .. } = &self.backend
        {
            let replica = dir_root.join(id.to_hex());
            match tokio::fs::remove_dir_all(&replica).await {
                Ok(()) => tracing::info!(share = %id, path = %replica.display(), "deleted on-disk replica"),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => tracing::warn!(share = %id, path = %replica.display(), error = %e, "could not delete replica"),
            }
        }

        tracing::info!(share = %id, "stopped accelerating share");
        self.persist();
        Ok(())
    }

    /// Pause (`seeding = false`) or resume (`seeding = true`) serving one share
    /// without forgetting it. Pause shuts the share's serving node down (NAS) or
    /// drops it from the relay cache, keeping the token and the on-disk replica;
    /// resume re-runs the add path (a cheap NAS top-up over the existing
    /// replica, or a relay re-cache).
    async fn set_seeding(&mut self, manifest_id: &str, seeding: bool) -> anyhow::Result<()> {
        let id = Hash::from_hex(manifest_id.trim())
            .map_err(|_| anyhow::anyhow!("not a manifest id: {manifest_id:?}"))?;

        if seeding {
            let token = match self.shares.get(&id) {
                Some(ShareRecord::Paused { token, .. }) => token.clone(),
                Some(_) => return Ok(()), // already serving / replicating
                None => anyhow::bail!("no such accelerated share"),
            };
            self.shares.remove(&id);
            let (ack, mut rx) = oneshot::channel();
            self.add_share(token, ack).await;
            // A relay acks synchronously; a NAS re-replication acks later from
            // its background task — don't block the command loop on it, just
            // report acceptance and let status show the progress.
            match rx.try_recv() {
                Ok(r) => r.map_err(|e| anyhow::anyhow!(e)),
                Err(_) => Ok(()),
            }
        } else {
            let record = self.shares.remove(&id).context("no such accelerated share")?;
            let (token, name_hint, meta) = match record {
                ShareRecord::Ready { token, meta, node, .. } => {
                    match node {
                        Some(node) => {
                            self.tracker.withdraw(&id.to_hex(), &node.peer_id().to_string());
                            node.shutdown().await;
                        }
                        None => {
                            if let Backend::Relay { relay, .. } = &self.backend {
                                self.tracker.withdraw(&id.to_hex(), &relay.peer_id().to_string());
                                relay.remove_share(id).await.ok();
                            }
                        }
                    }
                    (token, meta.name.clone(), Some(meta))
                }
                ShareRecord::Replicating { token, name_hint, abort, .. } => {
                    abort.abort();
                    (token, name_hint, None)
                }
                ShareRecord::Failed { token, name_hint, .. } => (token, name_hint, None),
                ShareRecord::Paused { token, name_hint, meta } => (token, name_hint, meta),
            };
            tracing::info!(share = %id, "paused serving share (replica kept)");
            self.shares.insert(id, ShareRecord::Paused { token, name_hint, meta });
            self.persist();
            Ok(())
        }
    }

    /// Read the cumulative served-bytes counters off the running `net` nodes
    /// (a relay's own forwarded-bytes total, or the sum across every NAS
    /// share's serving node) into `self.last_served`, so the next sync
    /// [`publish`](Self::publish) can report it in [`DaemonStatus`]. Also
    /// refreshes each NAS replica's on-disk size for the same status.
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
        if let Backend::Nas { dir_root, .. } = &self.backend {
            let ids: Vec<Hash> = self
                .shares
                .iter()
                .filter(|(_, r)| {
                    matches!(r, ShareRecord::Ready { .. } | ShareRecord::Paused { .. })
                })
                .map(|(id, _)| *id)
                .collect();
            let dir_root = dir_root.clone();
            let sizes = tokio::task::spawn_blocking(move || {
                ids.into_iter()
                    .map(|id| (id, dir_size(&dir_root.join(id.to_hex())).unwrap_or(0)))
                    .collect::<HashMap<Hash, u64>>()
            })
            .await
            .unwrap_or_default();
            self.disk_bytes = sizes;
        }
    }

    fn persist(&mut self) {
        self.config.shares = self.shares.values().map(|r| r.token().to_string()).collect();
        self.config.shares.sort();
        self.config.paused_shares = self
            .shares
            .iter()
            .filter(|(_, r)| matches!(r, ShareRecord::Paused { .. }))
            .map(|(id, _)| id.to_hex())
            .collect();
        self.config.paused_shares.sort();
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
                    disk_bytes: self.disk_bytes.get(id).copied(),
                    listen_addr: None,
                    seeding: true,
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
                    disk_bytes: self.disk_bytes.get(&meta.manifest_id).copied(),
                    listen_addr: listen_addr.clone(),
                    seeding: true,
                    replicating: None,
                    error: None,
                },
                ShareRecord::Paused { name_hint, meta, .. } => ShareStatus {
                    manifest_id: id.to_hex(),
                    name: meta.as_ref().map_or_else(|| name_hint.clone(), |m| m.name.clone()),
                    files: meta.as_ref().map_or(0, |m| m.files),
                    total_bytes: meta.as_ref().map_or(0, |m| m.total_bytes),
                    version: meta.as_ref().map_or(0, |m| m.version),
                    private: meta.as_ref().is_some_and(|m| m.private),
                    cached_chunks: None,
                    replica_chunks: None,
                    disk_bytes: self.disk_bytes.get(id).copied(),
                    listen_addr: None,
                    seeding: false,
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
                    disk_bytes: None,
                    listen_addr: None,
                    seeding: true,
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

/// Sum the sizes of every file directly inside `dir`'s shard subdirectories —
/// the [`DiskChunkStore`](gaggle_core::DiskChunkStore) layout (256 prefix
/// shards, one file per chunk). Missing dir → `Ok(0)`.
fn dir_size(dir: &std::path::Path) -> std::io::Result<u64> {
    let mut total = 0u64;
    let shards = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    for shard in shards {
        let shard = shard?;
        if !shard.file_type()?.is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(shard.path())? {
            let entry = entry?;
            if let Ok(meta) = entry.metadata()
                && meta.is_file()
            {
                total += meta.len();
            }
        }
    }
    Ok(total)
}

/// A `PeerInfo` for the seeder tracker from a peer id and its dialable
/// addresses.
fn peer_info(peer_id: String, addrs: &[Multiaddr]) -> PeerInfo {
    PeerInfo { peer_id, addrs: addrs.iter().map(Multiaddr::to_string).collect() }
}

/// A share link's display name and whether it is invite-only — the metadata
/// the seeder tracker's open directory needs. A token that no longer parses
/// yields `(None, false)`: it will just show nameless, not leak as private.
fn link_meta(token: &str) -> (Option<String>, bool) {
    match ShareLink::parse(token) {
        Ok(link) => (Some(link.name).filter(|n| !n.is_empty()), link.invite.is_some()),
        Err(_) => (None, false),
    }
}

/// Dial the relay at `relay_addr` (a `…/p2p/<id>` multiaddr), reserve a
/// circuit slot, and return the resulting `/p2p-circuit/…/p2p/<self>`
/// address — dialable by a downloader even when `node` sits behind a NAT
/// with no shared network path, with dcutr opportunistically upgrading to a
/// direct connection once both ends have connected through the relay.
async fn reserve_relay_circuit(node: &Node, relay_addr: &str) -> anyhow::Result<Multiaddr> {
    let addr: Multiaddr = relay_addr.trim().parse()?;
    // `bootstrap` dials the relay (blocking until the connection is live — the
    // reservation needs one) and joins its DHT, so the replica also becomes
    // discoverable through the relay's bootstrap role.
    let relay = node.bootstrap(addr.clone()).await?;
    let circuit = node.reserve_relay_slot(relay, addr).await?;
    tracing::info!(%relay, %circuit, "NAS replica reserved a relay circuit");
    Ok(circuit)
}

/// Origin side of the NAT-rendezvous handshake for one served share: check
/// whether any subscriber is waiting for `node` to show up, publish `node`'s
/// current addresses (plus its relay circuit, if any) as the answer, then
/// dial each subscriber's candidates — that outbound dial is this side's
/// half of the punch, opening the pinhole for their inbound one. Answering
/// happens *before* the dials so a slow/dead punch dial never delays the
/// subscriber seeing the answer.
async fn answer_punch_requests(
    client: &RendezvousClient,
    node: &Node,
    circuit_addr: Option<&str>,
) -> anyhow::Result<()> {
    let my_id = node.peer_id().to_string();
    let pending = client.pending(&my_id).await?;
    if pending.is_empty() {
        return Ok(());
    }
    let mut addrs: Vec<String> = node
        .reachable_addrs()
        .await?
        .into_iter()
        .filter(|a| !net::addr_is_loopback(a))
        .map(|a| a.to_string())
        .collect();
    if let Some(circuit) = circuit_addr
        && !addrs.iter().any(|a| a == circuit)
    {
        addrs.push(circuit.to_string());
    }
    if addrs.is_empty() {
        return Ok(());
    }
    let me = PeerInfo { peer_id: my_id.clone(), addrs };

    for req in pending {
        if let Err(e) = client.answer(&my_id, &req.request_id, &me).await {
            tracing::debug!(error = %format!("{e:#}"), "rendezvous answer failed");
            continue;
        }
        for addr in &req.subscriber.addrs {
            if let Ok(addr) = addr.parse::<Multiaddr>() {
                let _ = tokio::time::timeout(PUNCH_DIAL_TIMEOUT, node.bootstrap(addr)).await;
            }
        }
    }
    Ok(())
}

/// Deterministic per-share identity seed: `blake3(daemon-seed ++ manifest-id)`.
fn share_identity_seed(identity: &Keypair, manifest_id: Hash) -> anyhow::Result<[u8; 32]> {
    let daemon_seed = net::identity_seed(identity)?;
    let mut buf = Vec::with_capacity(64);
    buf.extend_from_slice(&daemon_seed);
    buf.extend_from_slice(manifest_id.as_bytes());
    Ok(*Hash::of(&buf).as_bytes())
}
