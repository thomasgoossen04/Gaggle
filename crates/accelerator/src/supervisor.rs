//! The `Supervisor` owns the running accelerator (a multi-share [`RelayNode`],
//! or one serving [`Node`] per replicated share) and serialises every mutation
//! that arrives from the admin API, republishing a [`DaemonStatus`] after each
//! and rewriting `config.toml` so the change survives a restart.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Context;
use control_plane::admin::{AdminCommand, DaemonStatus, ShareStatus};
use gaggle_core::{AgentKeypair, Hash};
use net::accel::{ShareMeta, nas_add_share, relay_add_share};
use net::{Keypair, Node, RelayConfig, RelayNode, ShareLink};
use tokio::sync::{mpsc, watch};

use crate::config::{AcceleratorConfig, Home, Role};

/// A share the daemon is accelerating.
struct ShareRecord {
    token: String,
    meta: ShareMeta,
    /// NAS role only: the node serving this share (drop = stop), and its chunk
    /// count / address.
    node: Option<Node>,
    replica_chunks: Option<usize>,
    listen_addr: Option<String>,
}

enum Backend {
    Relay { relay: RelayNode, meta_node: Node, listen_addrs: Vec<String> },
    Nas { dir_root: PathBuf },
}

pub struct Supervisor {
    home: Home,
    config: AcceleratorConfig,
    daemon: AgentKeypair,
    identity: Keypair,
    backend: Backend,
    shares: HashMap<Hash, ShareRecord>,
    status_tx: watch::Sender<DaemonStatus>,
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
        let mut sup = Self {
            home,
            config,
            daemon,
            identity,
            backend,
            shares: HashMap::new(),
            status_tx,
        };

        let tokens = sup.config.shares.clone();
        for token in tokens {
            if let Err(e) = sup.add_share_inner(&token).await {
                tracing::warn!(
                    error = %format!("{e:#}"),
                    token = %elide(&token),
                    "could not start share from config"
                );
            }
        }
        sup.publish();
        Ok((sup, status_rx))
    }

    /// Drive the supervisor: apply admin mutations until the channel closes.
    pub async fn run(mut self, mut commands: mpsc::Receiver<AdminCommand>) {
        while let Some(cmd) = commands.recv().await {
            match cmd {
                AdminCommand::AddShare { token, ack } => {
                    let result = self.add_share(&token).await.map_err(|e| format!("{e:#}"));
                    let _ = ack.send(result);
                }
                AdminCommand::RemoveShare { manifest_id, ack } => {
                    let result =
                        self.remove_share(&manifest_id).await.map_err(|e| format!("{e:#}"));
                    let _ = ack.send(result);
                }
            }
            self.publish();
        }
    }

    async fn add_share(&mut self, token: &str) -> anyhow::Result<()> {
        self.add_share_inner(token).await?;
        self.persist();
        Ok(())
    }

    async fn add_share_inner(&mut self, token: &str) -> anyhow::Result<()> {
        let link = ShareLink::parse(token).context("parsing the share link")?;
        if self.shares.contains_key(&link.manifest_id) {
            anyhow::bail!("that share is already being accelerated");
        }

        let record = match &self.backend {
            Backend::Relay { relay, meta_node, .. } => {
                let meta = relay_add_share(relay, meta_node, &link).await?;
                ShareRecord {
                    token: token.to_string(),
                    meta,
                    node: None,
                    replica_chunks: None,
                    listen_addr: None,
                }
            }
            Backend::Nas { dir_root } => {
                let seed = share_identity_seed(&self.identity, link.manifest_id)?;
                let (node, meta, chunks) =
                    nas_add_share(dir_root, net::keypair_from_seed(seed), &link).await?;
                let listen_addr = node.listen_addr().await.ok().map(|a| a.to_string());
                ShareRecord {
                    token: token.to_string(),
                    meta,
                    node: Some(node),
                    replica_chunks: Some(chunks),
                    listen_addr,
                }
            }
        };

        tracing::info!(
            share = %record.meta.manifest_id,
            name = %record.meta.name,
            files = record.meta.files,
            "accelerating share"
        );
        self.shares.insert(link.manifest_id, record);
        Ok(())
    }

    async fn remove_share(&mut self, manifest_id: &str) -> anyhow::Result<()> {
        let id = Hash::from_hex(manifest_id.trim())
            .map_err(|_| anyhow::anyhow!("not a manifest id: {manifest_id:?}"))?;
        let record = self.shares.remove(&id).context("no such accelerated share")?;

        match &self.backend {
            Backend::Relay { relay, .. } => {
                relay.remove_share(id).await.ok();
            }
            Backend::Nas { .. } => {
                if let Some(node) = record.node {
                    node.shutdown().await; // the on-disk replica is kept
                }
            }
        }
        tracing::info!(share = %id, "stopped accelerating share");
        self.persist();
        Ok(())
    }

    fn persist(&mut self) {
        self.config.shares = self.shares.values().map(|r| r.token.clone()).collect();
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
            .values()
            .map(|r| ShareStatus {
                manifest_id: r.meta.manifest_id.to_hex(),
                name: r.meta.name.clone(),
                files: r.meta.files,
                total_bytes: r.meta.total_bytes,
                version: r.meta.version,
                private: r.meta.private,
                cached_chunks: None,
                replica_chunks: r.replica_chunks.map(|n| n as u64),
                listen_addr: r.listen_addr.clone(),
                error: None,
            })
            .collect();
        shares.sort_by(|a, b| a.name.cmp(&b.name));

        let status = DaemonStatus {
            agent_id: self.daemon.public().to_hex(),
            peer_id: self.identity.public().to_peer_id().to_string(),
            role: self.config.role.as_str().to_string(),
            listen_addrs,
            shares,
        };
        let _ = self.status_tx.send(status);
    }
}

/// Deterministic per-share identity seed: `blake3(daemon-seed ++ manifest-id)`.
fn share_identity_seed(identity: &Keypair, manifest_id: Hash) -> anyhow::Result<[u8; 32]> {
    let daemon_seed = net::identity_seed(identity)?;
    let mut buf = Vec::with_capacity(64);
    buf.extend_from_slice(&daemon_seed);
    buf.extend_from_slice(manifest_id.as_bytes());
    Ok(*Hash::of(&buf).as_bytes())
}

fn elide(s: &str) -> String {
    if s.len() > 24 { format!("{}…", &s[..24]) } else { s.to_string() }
}
