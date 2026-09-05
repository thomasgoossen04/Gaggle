//! The transfer manager: a background task that owns the `net` nodes and turns
//! high-level commands ("share this folder", "subscribe to that one", "pause",
//! "rescan", "run an accelerator") into swarm activity, publishing an
//! [`AppState`] snapshot after every change.
//!
//! The GUI never touches `net`. It calls the sync methods on [`App`], reads
//! [`App::snapshot`], and optionally listens on [`App::events`]. All the async
//! lives here.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use control_plane::{AdminClient, PeerInfo, RendezvousClient, TrackerClient};
use gaggle_core::{
    AgentId, AgentKeypair, ChunkList, DiskChunkStore, Hash, Manifest, MemoryChunkStore,
    ScanProgress, SignedCapability, SourceChunkStore, SyncOutcome, index_dir_with_progress,
    snapshot_dir, sync_share, write_share,
};
use net::accel::{nas_pull_with_progress, nas_serve, relay_add_share};
use net::{
    CacheStats, Capability, Catalog, Invite, Keypair, Multiaddr, Node, PeerId, RelayConfig,
    RelayNode, Scope, ShareKeypair, ShareLink, SwarmConfig, SwarmProgress, peer_id_of,
};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

use crate::persist::{PersistedSeed, PersistedState};
use crate::settings::{PersistedAccelRole, PersistedAccelerator, Settings};
use crate::state::{
    AccelShareRow, AcceleratorRole, AcceleratorState, AppState, BenchmarkResult, MintedInvite,
    RemoteAccelState, ReplicaProgress, SourceStats, SwarmStatus, TransferId, TransferKind,
    TransferRow, TransferStatus,
};

/// A discrete thing that happened, for callers that would rather react than poll.
#[derive(Debug, Clone)]
pub enum AppEvent {
    /// The [`AppState`] snapshot changed — re-read [`App::snapshot`].
    Changed,
    TransferAdded(TransferId),
    TransferProgress(TransferId),
    TransferCompleted(TransferId),
    TransferFailed(TransferId, String),
    /// An update check found a newer manifest version for a subscription.
    UpdateAvailable(TransferId, u64),
    /// [`App::mint_invite`] produced a token — read `snapshot().minted_invite`.
    InviteMinted(TransferId),
    /// [`App::benchmark`] finished — read `snapshot().benchmark`.
    BenchmarkReady,
    /// The in-process accelerator started, stopped, or refreshed its stats.
    AcceleratorChanged,
    /// The in-process accelerator could not start / crashed.
    AcceleratorFailed(String),
}

/// Everything needed to start pulling a remote share. `Serialize`/`Deserialize`
/// so it round-trips through the persisted share list ([`crate::persist`]).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SubscribeRequest {
    /// Display name (also the download sub-folder).
    pub name: String,
    pub manifest_id: Hash,
    /// Dialable peer addresses (`…/p2p/<id>`).
    pub sources: Vec<Multiaddr>,
    /// Capability token for a private share.
    pub credential: Option<SignedCapability>,
}

impl SubscribeRequest {
    /// Build from a parsed [`Invite`](gaggle_core::Invite) plus the addresses to
    /// reach a seed at (an invite carries no network location of its own).
    pub fn from_invite(invite: &gaggle_core::Invite, sources: Vec<Multiaddr>) -> Self {
        Self {
            name: invite.name.clone(),
            manifest_id: invite.manifest_id,
            sources,
            credential: Some(invite.credential.clone()),
        }
    }
}

impl From<ShareLink> for SubscribeRequest {
    fn from(link: ShareLink) -> Self {
        Self {
            name: link.name,
            manifest_id: link.manifest_id,
            sources: link.sources,
            credential: link.invite.map(|i| i.credential),
        }
    }
}

/// Ask the node to opt this machine in as an accelerator. Each role takes a
/// *list* of shares to carry — an accelerator is not bound to one.
#[derive(Debug, Clone)]
pub enum AcceleratorRequest {
    /// High-bandwidth relay + Kademlia bootstrap, read-through caching every
    /// share in `shares`.
    Relay { cache_bytes: u64, shares: Vec<ShareLink> },
    /// Durable on-disk replica of every share in `shares`, kept under `dir`
    /// (one serving node per share).
    Nas { dir: PathBuf, shares: Vec<ShareLink> },
}

enum Command {
    AddLocalShare { dir: PathBuf, private: bool },
    Subscribe(SubscribeRequest),
    Pause(TransferId),
    Resume(TransferId),
    Retry(TransferId),
    Remove { id: TransferId, delete_files: bool },
    RescanShare(TransferId),
    CheckUpdates(TransferId),
    Resync(TransferId),
    MintInvite { id: TransferId, scope: Scope, expires_at: Option<u64> },
    Benchmark,
    StartAccelerator(Box<AcceleratorRequest>),
    StopAccelerator,
    AccelAddShare(String),
    AccelRemoveShare(String),
    AddRemoteAccelerator { label: String, admin_url: String },
    RemoveRemoteAccelerator(String),
    RemoteAddShare { label: String, token: String },
    RemoteRemoveShare { label: String, manifest_id: String },
    UpdateSettings(Box<Settings>),
    Shutdown,

    // Internal, from worker tasks.
    LocalShareReady {
        id: TransferId,
        node: Arc<Node>,
        addrs: Vec<Multiaddr>,
        /// Set when `Settings::public_relay` was configured but the reservation
        /// failed — the share still works, but the link is local-addresses-only.
        relay_warning: Option<String>,
        info: ShareInfo,
    },
    RescanDone {
        id: TransferId,
        manifest_id: Hash,
        version: u64,
        files: usize,
        bytes: u64,
        file_paths: Vec<String>,
    },
    WorkerFailed { id: TransferId, error: String },
    /// A local folder scan ([`add_share`](Manager::add_share) /
    /// [`rescan_share`](Manager::rescan_share)) made progress.
    ScanProgress { id: TransferId, files_total: usize, bytes_done: u64, bytes_total: u64 },
    DownloadProgress { id: TransferId, p: SwarmProgress, base_bytes: u64 },
    /// A download/resync worker reached a new pre-transfer phase (resolving
    /// sources, authenticating, fetching metadata) — feeds `TransferRow::detail`.
    DownloadStage { id: TransferId, message: String },
    DownloadDone { id: TransferId, outcome: Box<DownloadOutcome> },
    UpdateSeen { id: TransferId, version: u64 },
    ResyncProgress { id: TransferId, p: SwarmProgress },
    ResyncDone { id: TransferId, outcome: Box<ResyncOutcome> },
    BenchmarkDone(BenchmarkResult),
    AcceleratorReady {
        handle: Box<AccelHandle>,
        state: Box<AcceleratorState>,
        request: Box<AcceleratorRequest>,
    },
    AcceleratorStartFailed(String),
    AcceleratorStatsRefresh(CacheStats),
    AccelSharesRefresh(Vec<AccelShareRow>),
    AccelShareAdded { node: Option<Box<Node>>, row: Box<AccelShareRow>, token: String },
    /// A NAS share added to an already-running local accelerator made progress.
    AccelShareProgress { manifest_id: String, progress: ReplicaProgress },
    RemoteStatusRefresh { label: String, state: Box<RemoteAccelState> },
    RepollRemote(String),
}

struct ShareInfo {
    name: String,
    manifest_id: Hash,
    files: usize,
    bytes: u64,
    version: u64,
    dir: PathBuf,
    /// Manifest file paths, sorted.
    file_paths: Vec<String>,
    /// `Some` for a private (invite-only) share — the per-share signing seed.
    share_seed: Option<[u8; 32]>,
}

struct DownloadOutcome {
    files: usize,
    total_bytes: u64,
    output_dir: PathBuf,
    /// Authoritative chunk-count-per-source from the finished swarm download.
    sources: HashMap<PeerId, usize>,
    manifest: Manifest,
    chunk_lists: BTreeMap<String, ChunkList>,
    version: u64,
}

struct ResyncOutcome {
    manifest: Manifest,
    chunk_lists: BTreeMap<String, ChunkList>,
    synced: SyncOutcome,
    files: usize,
    total_bytes: u64,
}

/// Handle to the running transfer manager. Sync, safe to call from any thread
/// (e.g. the GUI thread with no tokio runtime). Drop stops the manager.
pub struct App {
    commands: mpsc::Sender<Command>,
    state_rx: watch::Receiver<AppState>,
    events: broadcast::Sender<AppEvent>,
}

impl App {
    /// Start the manager on the current tokio runtime. `config_path`, if given,
    /// is where settings are loaded from and saved to.
    pub async fn new(config_path: Option<PathBuf>) -> anyhow::Result<Self> {
        let download_node = Node::spawn().await?;

        let settings = config_path
            .as_deref()
            .map(Settings::load)
            .transpose()
            .unwrap_or(None)
            .unwrap_or_default();

        // A persistent operator identity next to the settings file, so it is
        // stable across restarts (the key an accelerator daemon authorises).
        let operator = Arc::new(load_or_create_operator(config_path.as_deref()));

        let mut remotes = HashMap::new();
        let mut remote_states = Vec::new();
        for r in &settings.remote_accelerators {
            let pinned = r.daemon_key.as_deref().and_then(|k| AgentId::from_hex(k).ok());
            remotes.insert(r.label.clone(), pinned);
            remote_states.push(RemoteAccelState {
                label: r.label.clone(),
                admin_url: r.admin_url.clone(),
                reachable: false,
                peer_id: None,
                daemon_key: r.daemon_key.clone(),
                role: None,
                shares: Vec::new(),
                error: None,
            });
        }

        let state = AppState {
            settings,
            transfers: Default::default(),
            swarm: SwarmStatus {
                download_peer_id: Some(download_node.peer_id()),
                seeding: 0,
                downloading: 0,
            },
            accelerator: None,
            remote_accelerators: remote_states,
            benchmark: None,
            minted_invite: None,
            operator_key: operator.public().to_hex(),
        };

        let (commands_tx, commands_rx) = mpsc::channel(128);
        let (state_tx, state_rx) = watch::channel(state.clone());
        let (events_tx, _) = broadcast::channel(256);

        let manager = Manager {
            self_tx: commands_tx.clone(),
            rx: commands_rx,
            state_tx,
            events: events_tx.clone(),
            config_path,
            state,
            next_id: 1,
            download_node: Arc::new(download_node),
            operator,
            seeds: HashMap::new(),
            subs: HashMap::new(),
            downloads: HashMap::new(),
            resync_samples: HashMap::new(),
            accel: None,
            remotes,
            last_resync_poll: Instant::now(),
            last_remote_poll: Instant::now()
                .checked_sub(Duration::from_secs(60))
                .unwrap_or_else(Instant::now),
            // "Long ago", so the first tick with a share + tracker configured
            // announces immediately rather than after a full interval.
            last_tracker_announce: Instant::now()
                .checked_sub(TRACKER_ANNOUNCE_INTERVAL)
                .unwrap_or_else(Instant::now),
        };
        tokio::spawn(manager.run());

        Ok(Self { commands: commands_tx, state_rx, events: events_tx })
    }

    /// The current state snapshot.
    pub fn snapshot(&self) -> AppState {
        self.state_rx.borrow().clone()
    }

    /// A receiver that fires whenever the snapshot changes.
    pub fn state_watch(&self) -> watch::Receiver<AppState> {
        self.state_rx.clone()
    }

    /// A stream of discrete [`AppEvent`]s. Late subscribers miss earlier ones.
    pub fn events(&self) -> broadcast::Receiver<AppEvent> {
        self.events.subscribe()
    }

    /// Snapshot `dir` and start seeding it publicly.
    pub fn add_local_share(&self, dir: impl Into<PathBuf>) {
        self.send(Command::AddLocalShare { dir: dir.into(), private: false });
    }

    /// Snapshot `dir` and start seeding it as an invite-only share. Hand out
    /// access with [`mint_invite`](Self::mint_invite).
    pub fn add_private_share(&self, dir: impl Into<PathBuf>) {
        self.send(Command::AddLocalShare { dir: dir.into(), private: true });
    }

    /// Re-read a seeded folder from disk, bump its manifest version, and re-serve
    /// it. Subscribers pick up the delta via [`resync`](Self::resync).
    pub fn rescan_share(&self, id: TransferId) {
        self.send(Command::RescanShare(id));
    }

    /// Mint a `gaggleshare1…` token granting `scope` (optionally expiring at a
    /// unix timestamp) for the private seed `id`. The token lands in
    /// `snapshot().minted_invite`.
    pub fn mint_invite(&self, id: TransferId, scope: Scope, expires_at: Option<u64>) {
        self.send(Command::MintInvite { id, scope, expires_at });
    }

    /// Start pulling a remote share.
    pub fn subscribe(&self, request: SubscribeRequest) {
        self.send(Command::Subscribe(request));
    }

    /// Check a completed subscription for a newer manifest version. A newer
    /// version is only flagged (`row.update_available`), never applied.
    pub fn check_updates(&self, id: TransferId) {
        self.send(Command::CheckUpdates(id));
    }

    /// Pull the delta for a subscription whose share has a newer version and
    /// apply it to the already-materialized output tree.
    pub fn resync(&self, id: TransferId) {
        self.send(Command::Resync(id));
    }

    pub fn pause(&self, id: TransferId) {
        self.send(Command::Pause(id));
    }

    pub fn resume(&self, id: TransferId) {
        self.send(Command::Resume(id));
    }

    /// Re-attempt a failed download from scratch (existing partial chunks are
    /// kept and topped up, same as [`resume`](Self::resume)).
    pub fn retry(&self, id: TransferId) {
        self.send(Command::Retry(id));
    }

    /// Stop and forget a transfer (a seed stops serving; a download's partial
    /// chunks are discarded). Any materialized output folder is left on disk.
    pub fn remove(&self, id: TransferId) {
        self.send(Command::Remove { id, delete_files: false });
    }

    /// Like [`remove`](Self::remove) but also deletes a completed download's
    /// output folder. Never touches a seed's source folder.
    pub fn remove_and_delete(&self, id: TransferId) {
        self.send(Command::Remove { id, delete_files: true });
    }

    /// Measure disk write throughput + free space on the download volume and
    /// suggest an accelerator role. Result lands in `snapshot().benchmark`.
    pub fn benchmark(&self) {
        self.send(Command::Benchmark);
    }

    /// Opt this machine in as an accelerator. Only one runs at a time.
    pub fn start_accelerator(&self, request: AcceleratorRequest) {
        self.send(Command::StartAccelerator(Box::new(request)));
    }

    pub fn stop_accelerator(&self) {
        self.send(Command::StopAccelerator);
    }

    /// Add a share (a `gaggleshare1…` token) to the running local accelerator.
    pub fn accel_add_share(&self, token: impl Into<String>) {
        self.send(Command::AccelAddShare(token.into()));
    }

    /// Drop a share (by manifest-id hex) from the running local accelerator.
    pub fn accel_remove_share(&self, manifest_id: impl Into<String>) {
        self.send(Command::AccelRemoveShare(manifest_id.into()));
    }

    /// This node's operator public key (hex). Authorise it on a daemon with
    /// `accelerator authorize <key>`.
    pub fn operator_public_key(&self) -> String {
        self.snapshot().operator_key
    }

    /// Register a remote accelerator daemon by its admin URL. Its identity is
    /// pinned on the first successful status call.
    pub fn add_remote_accelerator(&self, label: impl Into<String>, admin_url: impl Into<String>) {
        self.send(Command::AddRemoteAccelerator {
            label: label.into(),
            admin_url: admin_url.into(),
        });
    }

    pub fn remove_remote_accelerator(&self, label: impl Into<String>) {
        self.send(Command::RemoveRemoteAccelerator(label.into()));
    }

    /// Tell a registered remote accelerator to start carrying a share.
    pub fn remote_add_share(&self, label: impl Into<String>, token: impl Into<String>) {
        self.send(Command::RemoteAddShare { label: label.into(), token: token.into() });
    }

    /// Tell a registered remote accelerator to stop carrying a share.
    pub fn remote_remove_share(&self, label: impl Into<String>, manifest_id: impl Into<String>) {
        self.send(Command::RemoteRemoveShare {
            label: label.into(),
            manifest_id: manifest_id.into(),
        });
    }

    pub fn update_settings(&self, settings: Settings) {
        self.send(Command::UpdateSettings(Box::new(settings)));
    }

    fn send(&self, command: Command) {
        if self.commands.try_send(command).is_err() {
            tracing::warn!("transfer-manager command channel is full or closed");
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        let _ = self.commands.try_send(Command::Shutdown);
    }
}

/// A seeded local folder: the serving node plus what a rescan / invite needs.
struct SeedEntry {
    node: Arc<Node>,
    dir: PathBuf,
    version: u64,
    name: String,
    manifest_id: Hash,
    /// Every dialable address for this share's node, ranked best-first — a
    /// share link embeds them all so a subscriber on any network (LAN, a VPN
    /// overlay, same machine) can connect.
    addrs: Vec<Multiaddr>,
    /// `Some` for a private share — the per-share Ed25519 seed.
    share_seed: Option<[u8; 32]>,
}

/// A completed subscription, retained so it can be re-synced later.
struct SubEntry {
    request: SubscribeRequest,
    output_dir: PathBuf,
    manifest: Manifest,
    chunk_lists: BTreeMap<String, ChunkList>,
    version: u64,
}

struct DownloadJob {
    task: JoinHandle<()>,
    request: SubscribeRequest,
    chunk_dir: PathBuf,
    /// For the rolling speed estimate.
    last_sample: Option<(Instant, u64)>,
}

/// Keeps an in-process accelerator alive; drop stops it. Carries any number of
/// shares — relay caches them all through one node, NAS serves one node each.
struct AccelHandle {
    role: AcceleratorRole,
    relay: Option<Arc<RelayNode>>,
    /// Relay role: a long-lived downloading node used to learn share metadata.
    meta: Option<Arc<Node>>,
    /// NAS role: replica root, and one serving node per share (drop = stop it).
    /// `Arc` so [`Manager::tick`] can hand a clone to a background task that
    /// announces the replica to the seeder tracker.
    nas_dir: Option<PathBuf>,
    nas_nodes: Vec<(Hash, Arc<Node>)>,
    /// Per-share status rows and the `gaggleshare1…` token behind each.
    rows: Vec<AccelShareRow>,
    tokens: Vec<String>,
}

struct Manager {
    self_tx: mpsc::Sender<Command>,
    rx: mpsc::Receiver<Command>,
    state_tx: watch::Sender<AppState>,
    events: broadcast::Sender<AppEvent>,
    config_path: Option<PathBuf>,

    state: AppState,
    next_id: TransferId,
    download_node: Arc<Node>,
    operator: Arc<AgentKeypair>,
    seeds: HashMap<TransferId, SeedEntry>,
    subs: HashMap<TransferId, SubEntry>,
    downloads: HashMap<TransferId, DownloadJob>,
    resync_samples: HashMap<TransferId, Option<(Instant, u64)>>,
    accel: Option<AccelHandle>,
    /// Registered remote accelerators: label → pinned daemon identity (if known).
    remotes: HashMap<String, Option<AgentId>>,
    last_resync_poll: Instant,
    last_remote_poll: Instant,
    last_tracker_announce: Instant,
}

/// Load a persistent operator [`AgentKeypair`] from `operator.key` next to the
/// settings file, creating it on first run. Ephemeral if there is no config dir.
///
/// The seed derives every accelerator/NAS share identity, so the file is written
/// `0600` and a present-but-unparseable file is **not** overwritten (that would
/// silently rotate every derived identity) — the process runs ephemerally
/// instead so a transient read error can't destroy the identity.
fn load_or_create_operator(config_path: Option<&std::path::Path>) -> AgentKeypair {
    let Some(dir) = config_path.and_then(|p| p.parent()) else {
        return AgentKeypair::generate();
    };
    let path = dir.join("operator.key");
    match std::fs::read_to_string(&path) {
        Ok(hex) => {
            if let Ok(bytes) = <[u8; 32]>::try_from(hex_decode(hex.trim()).unwrap_or_default()) {
                return AgentKeypair::from_seed(bytes);
            }
            tracing::warn!(path = %path.display(), "operator.key is unreadable; using an ephemeral identity (not overwriting it)");
            return AgentKeypair::generate();
        }
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            tracing::warn!(path = %path.display(), error = %e, "cannot read operator.key; using an ephemeral identity");
            return AgentKeypair::generate();
        }
        Err(_) => {} // absent — create it below
    }
    let kp = AgentKeypair::generate();
    let _ = std::fs::create_dir_all(dir);
    if let Err(e) = write_secret_file(&path, hex_encode(&kp.to_seed()).as_bytes()) {
        tracing::warn!(path = %path.display(), error = %e, "could not persist operator.key");
    }
    kp
}

/// Write secret bytes to a freshly created file, `0600` on unix.
fn write_secret_file(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
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
    {
        std::fs::write(path, bytes)
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

/// Deterministic per-share NAS identity seed: `blake3(operator-seed ++ id)`.
fn derive_share_seed(operator_seed: &[u8; 32], manifest_id: Hash) -> [u8; 32] {
    let mut buf = Vec::with_capacity(64);
    buf.extend_from_slice(operator_seed);
    buf.extend_from_slice(manifest_id.as_bytes());
    *Hash::of(&buf).as_bytes()
}

impl Manager {
    async fn run(mut self) {
        self.restore_persisted();
        let mut ticker = tokio::time::interval(Duration::from_secs(2));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                command = self.rx.recv() => match command {
                    None | Some(Command::Shutdown) => break,
                    Some(command) => self.handle(command),
                },
                _ = ticker.tick() => self.tick(),
            }
        }
        // Dropping `self` drops every `Node`, which aborts its swarm task.
        for job in self.downloads.values() {
            job.task.abort();
        }
    }

    fn tick(&mut self) {
        // Relay hot-cache stats refresh.
        if let Some(relay) = self.accel.as_ref().and_then(|a| a.relay.clone()) {
            let tx = self.self_tx.clone();
            tokio::spawn(async move {
                if let Ok(s) = relay.cache_stats().await {
                    let _ = tx.send(Command::AcceleratorStatsRefresh(s)).await;
                }
            });
        }
        // Background update poll for subscriptions.
        if let Some(secs) = self.state.settings.auto_resync_secs
            && self.last_resync_poll.elapsed() >= Duration::from_secs(secs.max(5))
        {
            self.last_resync_poll = Instant::now();
            for id in self.subs.keys().copied().collect::<Vec<_>>() {
                let _ = self.self_tx.try_send(Command::CheckUpdates(id));
            }
        }
        // Poll every registered remote accelerator for its live status.
        if !self.remotes.is_empty()
            && self.last_remote_poll.elapsed() >= Duration::from_secs(10)
        {
            self.last_remote_poll = Instant::now();
            for label in self.remotes.keys().cloned().collect::<Vec<_>>() {
                self.spawn_remote_status(label);
            }
        }
        // NAT rendezvous: check whether some subscriber is waiting to punch
        // through to one of our served shares, and answer if so.
        if let Some(url) = self.state.settings.rendezvous_url.clone() {
            for seed in self.seeds.values() {
                let node = Arc::clone(&seed.node);
                let url = url.clone();
                tokio::spawn(async move { answer_rendezvous_requests(&node, &url).await });
            }
        }
        // Seeder tracker: publish which shares this node serves — every
        // origin seed plus any local NAS replica / relay cache — so a
        // downloader pointed at the same accelerator discovers them all as
        // sources, not just the one address baked into its share link. The
        // timer is only advanced when there is actually something to
        // announce, so a share that appears shortly after startup is
        // published on the next tick rather than waiting out a full interval.
        if let Some(url) = self.state.settings.rendezvous_url.clone() {
            let mut nodes: Vec<(Hash, Arc<Node>)> = self
                .seeds
                .values()
                .map(|s| (s.manifest_id, Arc::clone(&s.node)))
                .collect();
            let relay = self.accel.as_ref().and_then(|a| a.relay.clone());
            if let Some(accel) = &self.accel {
                nodes.extend(accel.nas_nodes.iter().map(|(id, n)| (*id, Arc::clone(n))));
            }
            if (!nodes.is_empty() || relay.is_some())
                && self.last_tracker_announce.elapsed() >= TRACKER_ANNOUNCE_INTERVAL
            {
                self.last_tracker_announce = Instant::now();
                tokio::spawn(async move { announce_to_tracker(&url, nodes, relay).await });
            }
        }
    }

    /// Fetch one remote daemon's status off-thread and feed it back as a
    /// [`Command::RemoteStatusRefresh`].
    fn spawn_remote_status(&mut self, label: String) {
        let Some(pinned) = self.remotes.get(&label).copied() else { return };
        let base = self
            .state
            .settings
            .remote_accelerators
            .iter()
            .find(|r| r.label == label)
            .map(|r| r.admin_url.clone())
            .unwrap_or_default();
        let operator = AgentKeypair::from_seed(self.operator.to_seed());
        let tx = self.self_tx.clone();
        tokio::spawn(async move {
            let state = match AdminClient::new(base.clone(), operator, pinned) {
                Ok(mut client) => match client.status().await {
                    Ok(s) => {
                        let role = match s.role.as_str() {
                            "nas" => Some(AcceleratorRole::Nas),
                            _ => Some(AcceleratorRole::Relay),
                        };
                        RemoteAccelState {
                            label: label.clone(),
                            admin_url: base,
                            reachable: true,
                            peer_id: Some(s.peer_id),
                            daemon_key: client.pinned().map(|k| k.to_hex()),
                            role,
                            shares: s.shares.iter().map(share_status_row).collect(),
                            error: None,
                        }
                    }
                    Err(e) => RemoteAccelState {
                        label: label.clone(),
                        admin_url: base,
                        reachable: false,
                        peer_id: None,
                        daemon_key: pinned.map(|k| k.to_hex()),
                        role: None,
                        shares: Vec::new(),
                        error: Some(format!("{e:#}")),
                    },
                },
                Err(e) => RemoteAccelState {
                    label: label.clone(),
                    admin_url: base,
                    reachable: false,
                    peer_id: None,
                    daemon_key: pinned.map(|k| k.to_hex()),
                    role: None,
                    shares: Vec::new(),
                    error: Some(format!("{e:#}")),
                },
            };
            let _ = tx
                .send(Command::RemoteStatusRefresh { label, state: Box::new(state) })
                .await;
        });
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::AddLocalShare { dir, private } => self.add_share(dir, private),
            Command::Subscribe(req) => self.subscribe(req),
            Command::Pause(id) => self.pause(id),
            Command::Resume(id) => self.resume(id),
            Command::Retry(id) => self.retry(id),
            Command::Remove { id, delete_files } => self.remove(id, delete_files),
            Command::RescanShare(id) => self.rescan_share(id),
            Command::CheckUpdates(id) => self.check_updates(id),
            Command::Resync(id) => self.resync(id),
            Command::MintInvite { id, scope, expires_at } => self.mint_invite(id, scope, expires_at),
            Command::Benchmark => self.benchmark(),
            Command::StartAccelerator(req) => self.start_accelerator(*req),
            Command::StopAccelerator => {
                self.accel = None;
                self.state.accelerator = None;
                self.save_accelerator_settings(None);
                self.publish();
                let _ = self.events.send(AppEvent::AcceleratorChanged);
            }
            Command::AccelAddShare(token) => self.accel_add_share(token),
            Command::AccelRemoveShare(id) => self.accel_remove_share(id),
            Command::AddRemoteAccelerator { label, admin_url } => {
                self.add_remote_accelerator(label, admin_url)
            }
            Command::RemoveRemoteAccelerator(label) => self.remove_remote_accelerator(label),
            Command::RemoteAddShare { label, token } => self.remote_share_op(label, Some(token), None),
            Command::RemoteRemoveShare { label, manifest_id } => {
                self.remote_share_op(label, None, Some(manifest_id))
            }
            Command::RemoteStatusRefresh { label, state } => {
                if let Some(k) = &state.daemon_key
                    && let Ok(id) = AgentId::from_hex(k)
                {
                    self.remotes.insert(label.clone(), Some(id));
                    if let Some(r) = self
                        .state
                        .settings
                        .remote_accelerators
                        .iter_mut()
                        .find(|r| r.label == label)
                        && r.daemon_key.as_deref() != Some(k.as_str())
                    {
                        r.daemon_key = Some(k.clone());
                        if let Some(path) = &self.config_path {
                            let _ = self.state.settings.save(path);
                        }
                    }
                }
                if let Some(slot) =
                    self.state.remote_accelerators.iter_mut().find(|r| r.label == label)
                {
                    *slot = *state;
                    self.publish();
                }
            }
            Command::UpdateSettings(s) => {
                self.state.settings = *s;
                if let Some(path) = &self.config_path
                    && let Err(e) = self.state.settings.save(path)
                {
                    tracing::warn!(error = %e, "could not save settings");
                }
                // Persistence may have just been turned on (or off) — either
                // way, make sure the file matches reality going forward.
                self.persist_shares();
                self.publish();
            }
            Command::Shutdown => {}

            Command::LocalShareReady { id, node, addrs, relay_warning, info } => {
                self.seeds.insert(
                    id,
                    SeedEntry {
                        node,
                        dir: info.dir.clone(),
                        version: info.version,
                        name: info.name.clone(),
                        manifest_id: info.manifest_id,
                        addrs: addrs.clone(),
                        share_seed: info.share_seed,
                    },
                );
                if let Some(row) = self.state.transfers.get_mut(&id) {
                    row.name = info.name;
                    row.manifest_id = info.manifest_id;
                    row.files = info.files;
                    row.total_bytes = info.bytes;
                    row.done_bytes = info.bytes;
                    row.version = info.version;
                    row.private = info.share_seed.is_some();
                    row.source_dir = Some(info.dir);
                    row.file_paths = Arc::new(info.file_paths);
                    row.status = TransferStatus::Complete;
                    row.share_addr = addrs.first().cloned();
                    row.share_addrs = addrs;
                    row.error = relay_warning;
                }
                self.recount();
                self.persist_shares();
                self.publish();
                let _ = self.events.send(AppEvent::TransferCompleted(id));
            }
            Command::RescanDone { id, manifest_id, version, files, bytes, file_paths } => {
                if let Some(seed) = self.seeds.get_mut(&id) {
                    seed.version = version;
                    seed.manifest_id = manifest_id;
                }
                if let Some(row) = self.state.transfers.get_mut(&id) {
                    row.manifest_id = manifest_id;
                    row.version = version;
                    row.files = files;
                    row.total_bytes = bytes;
                    row.done_bytes = bytes;
                    row.file_paths = Arc::new(file_paths);
                    row.status = TransferStatus::Complete;
                    row.error = None;
                }
                self.persist_shares();
                self.publish();
                let _ = self.events.send(AppEvent::TransferCompleted(id));
            }
            Command::WorkerFailed { id, error } => {
                // Leave a download job's `request`/`chunk_dir` in place (task is
                // already finished) so `retry` can respawn it, same as a paused one.
                if let Some(job) = self.downloads.get_mut(&id) {
                    job.last_sample = None;
                }
                self.resync_samples.remove(&id);
                if let Some(row) = self.state.transfers.get_mut(&id) {
                    row.status = TransferStatus::Failed;
                    row.error = Some(error.clone());
                    row.speed_bps = 0;
                    row.detail = None;
                }
                self.recount();
                self.publish();
                let _ = self.events.send(AppEvent::TransferFailed(id, error));
            }
            Command::DownloadStage { id, message } => {
                if let Some(row) = self.state.transfers.get_mut(&id) {
                    row.detail = Some(message);
                    self.publish();
                }
            }
            Command::ScanProgress { id, files_total, bytes_done, bytes_total } => {
                if let Some(row) = self.state.transfers.get_mut(&id) {
                    row.files = files_total;
                    row.total_bytes = bytes_total;
                    row.done_bytes = bytes_done;
                }
                self.publish();
                let _ = self.events.send(AppEvent::TransferProgress(id));
            }
            Command::DownloadProgress { id, p, base_bytes } => {
                let now = Instant::now();
                let done = base_bytes + p.bytes_done;
                if let Some(job) = self.downloads.get_mut(&id) {
                    let speed = sample_speed(&mut job.last_sample, now, done);
                    if let Some(row) = self.state.transfers.get_mut(&id) {
                        row.status = TransferStatus::Active;
                        row.detail = None;
                        row.total_bytes = base_bytes + p.bytes_total;
                        row.done_bytes = done;
                        if let Some(speed) = speed {
                            row.speed_bps = if row.speed_bps == 0 {
                                speed
                            } else {
                                (row.speed_bps * 3 + speed) / 4
                            };
                        }
                        bump_source(&mut row.sources, p.from, p.chunk_len);
                    }
                    self.publish();
                    let _ = self.events.send(AppEvent::TransferProgress(id));
                }
            }
            Command::DownloadDone { id, outcome } => {
                let request = self.downloads.remove(&id).map(|job| {
                    clear_partial(job.chunk_dir);
                    job.request
                });
                if let Some(request) = request {
                    self.subs.insert(
                        id,
                        SubEntry {
                            request,
                            output_dir: outcome.output_dir.clone(),
                            manifest: outcome.manifest.clone(),
                            chunk_lists: outcome.chunk_lists.clone(),
                            version: outcome.version,
                        },
                    );
                }
                if let Some(row) = self.state.transfers.get_mut(&id) {
                    row.status = TransferStatus::Complete;
                    row.files = outcome.files;
                    row.total_bytes = outcome.total_bytes;
                    row.done_bytes = outcome.total_bytes;
                    row.version = outcome.version;
                    row.speed_bps = 0;
                    row.output_dir = Some(outcome.output_dir);
                    for (peer, chunks) in &outcome.sources {
                        match row.sources.iter_mut().find(|s| s.peer == *peer) {
                            Some(s) => s.chunks = s.chunks.max(*chunks),
                            None => row.sources.push(SourceStats {
                                peer: *peer,
                                chunks: *chunks,
                                bytes: 0,
                            }),
                        }
                    }
                    row.sources.sort_by_key(|s| s.peer.to_bytes());
                }
                self.recount();
                self.publish();
                let _ = self.events.send(AppEvent::TransferCompleted(id));
            }
            Command::UpdateSeen { id, version } => {
                if let Some(row) = self.state.transfers.get_mut(&id) {
                    if version == 0 {
                        row.update_available = None;
                    } else {
                        row.update_available = Some(version);
                        let _ = self.events.send(AppEvent::UpdateAvailable(id, version));
                    }
                    self.publish();
                }
            }
            Command::ResyncProgress { id, p } => {
                let now = Instant::now();
                if let Some(slot) = self.resync_samples.get_mut(&id) {
                    let speed = sample_speed(slot, now, p.bytes_done);
                    if let Some(row) = self.state.transfers.get_mut(&id) {
                        row.status = TransferStatus::Active;
                        row.detail = None;
                        row.total_bytes = p.bytes_total.max(1);
                        row.done_bytes = p.bytes_done;
                        if let Some(speed) = speed {
                            row.speed_bps = if row.speed_bps == 0 {
                                speed
                            } else {
                                (row.speed_bps * 3 + speed) / 4
                            };
                        }
                        bump_source(&mut row.sources, p.from, p.chunk_len);
                    }
                    self.publish();
                    let _ = self.events.send(AppEvent::TransferProgress(id));
                }
            }
            Command::ResyncDone { id, outcome } => {
                self.resync_samples.remove(&id);
                if let Some(sub) = self.subs.get_mut(&id) {
                    sub.manifest = outcome.manifest.clone();
                    sub.chunk_lists = outcome.chunk_lists.clone();
                    sub.version = outcome.manifest.version;
                }
                tracing::info!(
                    id,
                    written = outcome.synced.written.len(),
                    removed = outcome.synced.removed.len(),
                    "resync applied"
                );
                if let Some(row) = self.state.transfers.get_mut(&id) {
                    row.status = TransferStatus::Complete;
                    row.version = outcome.manifest.version;
                    row.manifest_id = outcome.manifest.id();
                    row.files = outcome.files;
                    row.total_bytes = outcome.total_bytes;
                    row.done_bytes = outcome.total_bytes;
                    row.speed_bps = 0;
                    row.update_available = None;
                    row.error = None;
                }
                self.recount();
                self.publish();
                let _ = self.events.send(AppEvent::TransferCompleted(id));
            }
            Command::BenchmarkDone(result) => {
                self.state.benchmark = Some(result);
                self.publish();
                let _ = self.events.send(AppEvent::BenchmarkReady);
            }
            Command::AcceleratorReady { handle, state, request } => {
                self.accel = Some(*handle);
                self.state.accelerator = Some(*state);
                self.save_accelerator_settings(Some(&request));
                self.publish();
                let _ = self.events.send(AppEvent::AcceleratorChanged);
            }
            Command::AcceleratorStartFailed(error) => {
                self.accel = None;
                self.state.accelerator = None;
                self.publish();
                let _ = self.events.send(AppEvent::AcceleratorFailed(error));
            }
            Command::AcceleratorStatsRefresh(stats) => {
                if let Some(a) = &mut self.state.accelerator {
                    a.cache = Some(stats);
                    self.publish();
                }
            }
            Command::AccelSharesRefresh(rows) => {
                if let Some(h) = &mut self.accel {
                    h.rows = rows.clone();
                }
                if let Some(a) = &mut self.state.accelerator {
                    a.shares = rows;
                    a.detail = accel_detail(a.role, &a.shares);
                    self.publish();
                    let _ = self.events.send(AppEvent::AcceleratorChanged);
                }
            }
            Command::AccelShareAdded { node, row, token } => {
                if let Some(h) = &mut self.accel {
                    if let (Some(node), Ok(mid)) =
                        (node, Hash::from_hex(&row.manifest_id))
                    {
                        h.nas_nodes.push((mid, Arc::from(node)));
                    }
                    h.rows.retain(|r| r.manifest_id != row.manifest_id);
                    h.rows.push(*row.clone());
                    h.tokens.push(token);
                    let rows = h.rows.clone();
                    if let Some(a) = &mut self.state.accelerator {
                        a.shares = rows.clone();
                        a.detail = accel_detail(a.role, &rows);
                    }
                    self.sync_accelerator_shares();
                    self.publish();
                    let _ = self.events.send(AppEvent::AcceleratorChanged);
                }
            }
            Command::AccelShareProgress { manifest_id, progress } => {
                if let Some(h) = &mut self.accel
                    && let Some(row) = h.rows.iter_mut().find(|r| r.manifest_id == manifest_id)
                {
                    row.replicating = Some(progress);
                    if let Some(a) = &mut self.state.accelerator {
                        a.shares = h.rows.clone();
                    }
                    self.publish();
                }
            }
            Command::RepollRemote(label) => self.spawn_remote_status(label),
        }
    }

    fn accel_add_share(&mut self, token: String) {
        let Some(h) = &self.accel else {
            let _ = self.events.send(AppEvent::AcceleratorFailed(
                "no accelerator is running".into(),
            ));
            return;
        };
        let link = match ShareLink::parse(token.trim()) {
            Ok(l) => l,
            Err(e) => {
                let _ = self
                    .events
                    .send(AppEvent::AcceleratorFailed(format!("bad share link: {e:#}")));
                return;
            }
        };
        if h.rows.iter().any(|r| r.manifest_id == link.manifest_id.to_hex()) {
            return; // already carried
        }
        let role = h.role;
        let relay = h.relay.clone();
        let meta = h.meta.clone();
        let dir_root = h.nas_dir.clone();
        let operator_seed = self.operator.to_seed();
        let rendezvous_url = self.state.settings.rendezvous_url.clone();
        let tx = self.self_tx.clone();
        let existing = h.rows.clone();
        let token_owned = token.trim().to_string();

        tokio::spawn(async move {
            let added = match role {
                AcceleratorRole::Relay => match (relay, meta) {
                    (Some(relay), Some(meta)) => {
                        relay_add_share(&relay, &meta, &link).await.map(|m| (None, row_from_meta(&m, None, None)))
                    }
                    _ => Err(anyhow::anyhow!("relay accelerator is not available")),
                },
                AcceleratorRole::Nas => match dir_root {
                    Some(dir) => {
                        let seed = derive_share_seed(&operator_seed, link.manifest_id);
                        let manifest_id = link.manifest_id.to_hex();
                        let progress_tx = tx.clone();
                        let mut last_sent = Instant::now()
                            .checked_sub(Duration::from_millis(500))
                            .unwrap_or_else(Instant::now);
                        let on_progress = move |p: SwarmProgress| {
                            let done = p.chunks_done >= p.chunks_total;
                            if done || last_sent.elapsed() >= Duration::from_millis(500) {
                                last_sent = Instant::now();
                                let _ = progress_tx.try_send(Command::AccelShareProgress {
                                    manifest_id: manifest_id.clone(),
                                    progress: ReplicaProgress {
                                        chunks_done: p.chunks_done,
                                        chunks_total: p.chunks_total,
                                        bytes_done: p.bytes_done,
                                        bytes_total: p.bytes_total,
                                    },
                                });
                            }
                        };
                        match nas_replicate(
                            &dir,
                            net::keypair_from_seed(seed),
                            &link,
                            rendezvous_url.as_deref(),
                            on_progress,
                        )
                        .await
                        {
                            Ok((node, m, chunks)) => {
                                let addr = node.listen_addr().await.ok().map(|a| a.to_string());
                                Ok((
                                    Some(Box::new(node)),
                                    row_from_meta(&m, Some(chunks as u64), addr),
                                ))
                            }
                            Err(e) => Err(e),
                        }
                    }
                    None => Err(anyhow::anyhow!("nas accelerator is not available")),
                },
            };
            match added {
                Ok((node, row)) => {
                    let _ = tx
                        .send(Command::AccelShareAdded {
                            node,
                            row: Box::new(row),
                            token: token_owned,
                        })
                        .await;
                }
                Err(e) => {
                    let mut rows = existing;
                    rows.push(err_row(&link.name, &link.manifest_id.to_hex(), format!("{e:#}")));
                    let _ = tx.send(Command::AccelSharesRefresh(rows)).await;
                }
            }
        });
    }

    fn accel_remove_share(&mut self, manifest_id: String) {
        let Some(h) = &mut self.accel else { return };
        let id = manifest_id.trim().to_string();
        if let Some(pos) = h.rows.iter().position(|r| r.manifest_id == id) {
            h.rows.remove(pos);
        }
        h.tokens.retain(|t| {
            ShareLink::parse(t).map(|l| l.manifest_id.to_hex() != id).unwrap_or(true)
        });
        if let Some(pos) = h.nas_nodes.iter().position(|(mid, _)| mid.to_hex() == id) {
            let (_, node) = h.nas_nodes.remove(pos);
            // Gracefully shut down if this was the last reference; a brief
            // overlapping tracker-announce task just drops its clone and the
            // node's own `Drop` aborts the swarm task.
            tokio::spawn(async move {
                if let Some(node) = Arc::into_inner(node) {
                    node.shutdown().await;
                }
            });
        }
        if let (AcceleratorRole::Relay, Some(relay)) = (h.role, h.relay.clone())
            && let Ok(mid) = Hash::from_hex(&id)
        {
            tokio::spawn(async move { let _ = relay.remove_share(mid).await; });
        }
        let rows = h.rows.clone();
        if let Some(a) = &mut self.state.accelerator {
            a.shares = rows.clone();
            a.detail = accel_detail(a.role, &rows);
        }
        self.sync_accelerator_shares();
        self.publish();
        let _ = self.events.send(AppEvent::AcceleratorChanged);
    }

    /// Persist what a running local accelerator is carrying, so it can be
    /// restarted with the same role/dir/cache and share set on the next
    /// launch. `request` is `None` to clear it ([`StopAccelerator`](Command::StopAccelerator)).
    fn save_accelerator_settings(&mut self, request: Option<&AcceleratorRequest>) {
        self.state.settings.accelerator = request.map(|r| match r {
            AcceleratorRequest::Relay { cache_bytes, shares } => PersistedAccelerator {
                role: PersistedAccelRole::Relay,
                cache_bytes: *cache_bytes,
                dir: None,
                shares: shares.iter().map(|l| l.clone().encode()).collect(),
            },
            AcceleratorRequest::Nas { dir, shares } => PersistedAccelerator {
                role: PersistedAccelRole::Nas,
                cache_bytes: 0,
                dir: Some(dir.clone()),
                shares: shares.iter().map(|l| l.clone().encode()).collect(),
            },
        });
        if let Some(path) = &self.config_path {
            let _ = self.state.settings.save(path);
        }
    }

    /// Refresh the persisted share-token list for a running local accelerator
    /// to match what it's actually carrying (after `accel_add_share` /
    /// `accel_remove_share`), so a restart resumes the current set rather
    /// than only the one it was started with.
    fn sync_accelerator_shares(&mut self) {
        let Some(h) = &self.accel else { return };
        let tokens = h.tokens.clone();
        if let Some(acc) = &mut self.state.settings.accelerator {
            acc.shares = tokens;
            if let Some(path) = &self.config_path {
                let _ = self.state.settings.save(path);
            }
        }
    }

    fn add_remote_accelerator(&mut self, label: String, admin_url: String) {
        if label.trim().is_empty() || admin_url.trim().is_empty() {
            let _ = self
                .events
                .send(AppEvent::AcceleratorFailed("label and admin URL are required".into()));
            return;
        }
        let admin_url = control_plane::admin::normalize_base(&admin_url);
        self.remotes.entry(label.clone()).or_insert(None);
        let settings = &mut self.state.settings;
        if let Some(r) = settings.remote_accelerators.iter_mut().find(|r| r.label == label) {
            r.admin_url = admin_url.clone();
        } else {
            settings.remote_accelerators.push(crate::settings::RemoteAccelerator {
                label: label.clone(),
                admin_url: admin_url.clone(),
                daemon_key: None,
            });
        }
        if let Some(path) = &self.config_path {
            let _ = self.state.settings.save(path);
        }
        if !self.state.remote_accelerators.iter().any(|r| r.label == label) {
            self.state.remote_accelerators.push(RemoteAccelState {
                label: label.clone(),
                admin_url,
                reachable: false,
                peer_id: None,
                daemon_key: None,
                role: None,
                shares: Vec::new(),
                error: None,
            });
        }
        self.publish();
        self.spawn_remote_status(label);
    }

    fn remove_remote_accelerator(&mut self, label: String) {
        self.remotes.remove(&label);
        self.state.settings.remote_accelerators.retain(|r| r.label != label);
        self.state.remote_accelerators.retain(|r| r.label != label);
        if let Some(path) = &self.config_path {
            let _ = self.state.settings.save(path);
        }
        self.publish();
    }

    fn remote_share_op(&mut self, label: String, add: Option<String>, remove: Option<String>) {
        let Some(pinned) = self.remotes.get(&label).copied() else { return };
        let Some(base) = self
            .state
            .settings
            .remote_accelerators
            .iter()
            .find(|r| r.label == label)
            .map(|r| r.admin_url.clone())
        else {
            return;
        };
        let operator = AgentKeypair::from_seed(self.operator.to_seed());
        let tx = self.self_tx.clone();
        tokio::spawn(async move {
            let result = match AdminClient::new(base, operator, pinned) {
                Ok(mut client) => {
                    if let Some(token) = add {
                        client.add_share(token.trim()).await
                    } else if let Some(mid) = remove {
                        client.remove_share(mid.trim()).await
                    } else {
                        Ok(())
                    }
                }
                Err(e) => Err(e),
            };
            if let Err(e) = result {
                tracing::warn!(%label, error = %format!("{e:#}"), "remote share op failed");
            }
            // Re-poll so the UI reflects the change quickly.
            let _ = tx.send(Command::RepollRemote(label)).await;
        });
    }

    fn add_share(&mut self, dir: PathBuf, private: bool) {
        let share_seed = private.then(|| ShareKeypair::generate().to_seed());
        self.start_seed(dir, share_seed, 1);
    }

    /// Re-scan `dir` and start serving it again exactly as it was before a
    /// restart — same version, and (for a private share) the same signing
    /// key, so already-minted invites and any manifest ids a peer has pinned
    /// still line up. Shared by [`add_share`](Self::add_share) (a fresh
    /// share: `version: 1`, a freshly generated `share_seed`) and
    /// [`restore_persisted`](Self::restore_persisted).
    fn start_seed(&mut self, dir: PathBuf, share_seed: Option<[u8; 32]>, version: u64) {
        let id = self.alloc_id();
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| dir.display().to_string());
        let mut row = new_row(id, name.clone(), TransferKind::Seeding);
        row.status = TransferStatus::Scanning;
        self.insert_row(row);

        let tx = self.self_tx.clone();
        let cache_bytes = self.state.settings.seed_cache_bytes;
        let public_relay = self.state.settings.public_relay.clone();
        tokio::spawn(async move {
            let scan_dir = dir.clone();
            let scan_name = name.clone();
            let progress = scan_progress_sink(tx.clone(), id);
            let built = tokio::task::spawn_blocking(move || {
                // Stream chunks from the source folder on demand, holding only a
                // bounded hot-chunk cache in RAM — no whole-folder buffer, no
                // second copy on disk.
                let idx = index_dir_with_progress(&scan_dir, scan_name, version, progress)?;
                let store =
                    SourceChunkStore::new(&scan_dir, idx.locations.clone(), cache_bytes);
                anyhow::Ok((idx, store))
            })
            .await;

            let (snap, store) = match built {
                Ok(Ok(v)) => v,
                Ok(Err(e)) => return fail(&tx, id, format!("snapshot failed: {e:#}")).await,
                Err(e) => return fail(&tx, id, format!("snapshot task panicked: {e}")).await,
            };

            let info = ShareInfo {
                name: snap.manifest.name.clone(),
                manifest_id: snap.manifest.id(),
                files: snap.manifest.files.len(),
                bytes: snap.manifest.total_size(),
                version: snap.manifest.version,
                dir,
                file_paths: snap.manifest.files.iter().map(|f| f.path.clone()).collect(),
                share_seed,
            };
            let catalog = Catalog::new(snap.manifest, snap.chunk_lists, store);
            let node = match Node::spawn_serving(catalog).await {
                Ok(n) => n,
                Err(e) => return fail(&tx, id, format!("could not start serving: {e:#}")).await,
            };
            if let Some(seed) = share_seed {
                let pubkey = ShareKeypair::from_seed(seed).public();
                if let Err(e) = node.restrict_to_invite_holders(pubkey).await {
                    return fail(&tx, id, format!("could not make the share private: {e:#}")).await;
                }
            }
            let mut addrs = match node.reachable_addrs().await {
                Ok(a) if !a.is_empty() => a,
                Ok(_) => return fail(&tx, id, "no listen address".to_string()).await,
                Err(e) => return fail(&tx, id, format!("no listen address: {e:#}")).await,
            };
            let mut relay_warning = None;
            if let Some(relay_addr) = &public_relay {
                match reserve_relay(&node, relay_addr).await {
                    Ok(circuit) => addrs.push(circuit),
                    Err(e) => {
                        let msg = format!(
                            "Public relay unreachable ({e:#}) — link only has local addresses"
                        );
                        tracing::warn!(id, error = %format!("{e:#}"), "relay reservation failed");
                        relay_warning = Some(msg);
                    }
                }
            }
            let _ = tx
                .send(Command::LocalShareReady {
                    id,
                    node: Arc::new(node),
                    addrs,
                    relay_warning,
                    info,
                })
                .await;
        });
    }

    fn rescan_share(&mut self, id: TransferId) {
        let Some(seed) = self.seeds.get(&id) else {
            tracing::warn!(id, "rescan: no such seed");
            return;
        };
        let node = Arc::clone(&seed.node);
        let dir = seed.dir.clone();
        let name = seed.name.clone();
        let next_version = seed.version + 1;
        let share_seed = seed.share_seed;
        let cache_bytes = self.state.settings.seed_cache_bytes;

        if let Some(row) = self.state.transfers.get_mut(&id) {
            row.status = TransferStatus::Scanning;
            row.error = None;
        }
        self.publish();

        let tx = self.self_tx.clone();
        tokio::spawn(async move {
            let progress = scan_progress_sink(tx.clone(), id);
            let built = tokio::task::spawn_blocking(move || {
                let idx = index_dir_with_progress(&dir, name, next_version, progress)?;
                let store = SourceChunkStore::new(&dir, idx.locations.clone(), cache_bytes);
                anyhow::Ok((idx, store))
            })
            .await;
            let (snap, store) = match built {
                Ok(Ok(v)) => v,
                Ok(Err(e)) => return fail(&tx, id, format!("rescan snapshot failed: {e:#}")).await,
                Err(e) => return fail(&tx, id, format!("rescan task panicked: {e}")).await,
            };
            let manifest_id = snap.manifest.id();
            let files = snap.manifest.files.len();
            let bytes = snap.manifest.total_size();
            let file_paths: Vec<String> =
                snap.manifest.files.iter().map(|f| f.path.clone()).collect();
            let catalog = Catalog::new(snap.manifest, snap.chunk_lists, store);
            if let Err(e) = node.serve(catalog).await {
                return fail(&tx, id, format!("re-serve failed: {e:#}")).await;
            }
            if let Some(seed) = share_seed {
                let pubkey = ShareKeypair::from_seed(seed).public();
                if let Err(e) = node.restrict_to_invite_holders(pubkey).await {
                    return fail(&tx, id, format!("re-restrict failed: {e:#}")).await;
                }
            }
            let _ = tx
                .send(Command::RescanDone {
                    id,
                    manifest_id,
                    version: next_version,
                    files,
                    bytes,
                    file_paths,
                })
                .await;
        });
    }

    fn mint_invite(&mut self, id: TransferId, scope: Scope, expires_at: Option<u64>) {
        let Some(seed) = self.seeds.get(&id) else {
            tracing::warn!(id, "mint_invite: no such seed");
            return;
        };
        let Some(seed_bytes) = seed.share_seed else {
            tracing::warn!(id, "mint_invite: share is not private");
            return;
        };
        let kp = ShareKeypair::from_seed(seed_bytes);
        let mut cap = Capability::new(kp.public(), seed.manifest_id).with_scope(scope);
        if let Some(exp) = expires_at {
            cap = cap.expiring_at(exp);
        }
        let invite = Invite::new(kp.public(), seed.manifest_id, seed.name.clone(), kp.issue(cap));
        let token = ShareLink::new(seed.name.clone(), seed.manifest_id, seed.addrs.clone())
            .with_invite(invite)
            .encode();
        self.state.minted_invite = Some(MintedInvite { transfer: id, token });
        self.publish();
        let _ = self.events.send(AppEvent::InviteMinted(id));
    }

    fn check_updates(&mut self, id: TransferId) {
        let Some(sub) = self.subs.get(&id) else { return };
        let node = Arc::clone(&self.download_node);
        let req = sub.request.clone();
        let have_version = sub.version;
        let tx = self.self_tx.clone();
        let tracker_url = self.state.settings.rendezvous_url.clone();
        tokio::spawn(async move {
            match check_remote_version(&node, &req, tracker_url.as_deref()).await {
                Ok(version) => {
                    // Compare by manifest version only — a scoped download stores
                    // a narrowed manifest whose id never equals the seed's.
                    let flag = if version > have_version { version } else { 0 };
                    let _ = tx.send(Command::UpdateSeen { id, version: flag }).await;
                }
                Err(e) => tracing::warn!(id, error = %e, "update check failed"),
            }
        });
    }

    fn resync(&mut self, id: TransferId) {
        if self.downloads.contains_key(&id) || self.resync_samples.contains_key(&id) {
            return; // already busy
        }
        let Some(sub) = self.subs.get(&id) else {
            tracing::warn!(id, "resync: no such subscription");
            return;
        };
        let node = Arc::clone(&self.download_node);
        let req = sub.request.clone();
        let output_dir = sub.output_dir.clone();
        let old_manifest = sub.manifest.clone();

        if let Some(row) = self.state.transfers.get_mut(&id) {
            row.status = TransferStatus::Connecting;
            row.error = None;
            row.speed_bps = 0;
            row.sources.clear();
        }
        self.resync_samples.insert(id, None);
        self.publish();

        let tx = self.self_tx.clone();
        let rendezvous_url = self.state.settings.rendezvous_url.clone();
        tokio::spawn(async move {
            if let Err(e) = run_resync(
                node.as_ref(),
                id,
                req,
                output_dir,
                old_manifest,
                tx.clone(),
                rendezvous_url,
            )
            .await
            {
                let _ = tx.send(Command::WorkerFailed { id, error: format!("{e:#}") }).await;
            }
        });
    }

    fn benchmark(&mut self) {
        let dir = self.state.settings.download_dir.clone();
        let tx = self.self_tx.clone();
        tokio::spawn(async move {
            match tokio::task::spawn_blocking(move || run_benchmark(&dir)).await {
                Ok(Ok(result)) => {
                    let _ = tx.send(Command::BenchmarkDone(result)).await;
                }
                Ok(Err(e)) => tracing::warn!(error = %e, "benchmark failed"),
                Err(e) => tracing::warn!(error = %e, "benchmark task panicked"),
            }
        });
    }

    fn start_accelerator(&mut self, request: AcceleratorRequest) {
        if self.accel.is_some() {
            let _ = self
                .events
                .send(AppEvent::AcceleratorFailed("an accelerator is already running".into()));
            return;
        }
        let tx = self.self_tx.clone();
        let operator_seed = self.operator.to_seed();
        let rendezvous_url = self.state.settings.rendezvous_url.clone();
        let request_saved = request.clone();
        tokio::spawn(async move {
            let result = match request {
                AcceleratorRequest::Relay { cache_bytes, shares } => {
                    start_relay_accel(cache_bytes, shares).await
                }
                AcceleratorRequest::Nas { dir, shares } => {
                    start_nas_accel(dir, shares, operator_seed, rendezvous_url).await
                }
            };
            match result {
                Ok((handle, state)) => {
                    let _ = tx
                        .send(Command::AcceleratorReady {
                            handle: Box::new(handle),
                            state: Box::new(state),
                            request: Box::new(request_saved),
                        })
                        .await;
                }
                Err(e) => {
                    let _ = tx.send(Command::AcceleratorStartFailed(format!("{e:#}"))).await;
                }
            }
        });
    }

    fn subscribe(&mut self, request: SubscribeRequest) {
        let id = self.alloc_id();
        let mut row = new_row(id, request.name.clone(), TransferKind::Downloading);
        row.manifest_id = request.manifest_id;
        self.insert_row(row);
        let chunk_dir = self.partial_dir(request.manifest_id);
        self.spawn_download(id, request, chunk_dir);
        self.recount();
        self.persist_shares();
        self.publish();
        let _ = self.events.send(AppEvent::TransferAdded(id));
    }

    fn spawn_download(&mut self, id: TransferId, request: SubscribeRequest, chunk_dir: PathBuf) {
        let tx = self.self_tx.clone();
        let node = Arc::clone(&self.download_node);
        let out_root = self.state.settings.download_dir.clone();
        let name = sanitize(&request.name).unwrap_or_else(|| hex(&request.manifest_id));
        let req = request.clone();
        let dir = chunk_dir.clone();
        let rendezvous_url = self.state.settings.rendezvous_url.clone();

        let task = tokio::spawn(async move {
            let out = out_root.join(name);
            if let Err(e) =
                run_download(node.as_ref(), id, req, dir, out, tx.clone(), rendezvous_url).await
            {
                let _ = tx.send(Command::WorkerFailed { id, error: format!("{e:#}") }).await;
            }
        });
        self.downloads
            .insert(id, DownloadJob { task, request, chunk_dir, last_sample: None });
    }

    fn pause(&mut self, id: TransferId) {
        let Some(job) = self.downloads.get_mut(&id) else { return };
        job.task.abort();
        job.last_sample = None;
        if let Some(row) = self.state.transfers.get_mut(&id)
            && !row.status.is_terminal()
        {
            row.status = TransferStatus::Paused;
            row.speed_bps = 0;
        }
        self.publish();
    }

    fn resume(&mut self, id: TransferId) {
        let Some(job) = self.downloads.remove(&id) else { return };
        let is_paused = self
            .state
            .transfers
            .get(&id)
            .map(|r| r.status == TransferStatus::Paused)
            .unwrap_or(false);
        if !is_paused {
            self.downloads.insert(id, job);
            return;
        }
        if let Some(row) = self.state.transfers.get_mut(&id) {
            row.status = TransferStatus::Connecting;
            row.error = None;
        }
        self.spawn_download(id, job.request, job.chunk_dir);
        self.publish();
    }

    fn retry(&mut self, id: TransferId) {
        let Some(job) = self.downloads.remove(&id) else { return };
        let is_failed = self
            .state
            .transfers
            .get(&id)
            .map(|r| r.status == TransferStatus::Failed)
            .unwrap_or(false);
        if !is_failed {
            self.downloads.insert(id, job);
            return;
        }
        if let Some(row) = self.state.transfers.get_mut(&id) {
            row.status = TransferStatus::Connecting;
            row.error = None;
        }
        self.spawn_download(id, job.request, job.chunk_dir);
        self.publish();
    }

    fn remove(&mut self, id: TransferId, delete_files: bool) {
        self.seeds.remove(&id);
        self.subs.remove(&id);
        self.resync_samples.remove(&id);
        if let Some(job) = self.downloads.remove(&id) {
            job.task.abort();
            clear_partial(job.chunk_dir);
        } else if let Some(mid) = self
            .state
            .transfers
            .get(&id)
            .filter(|r| r.kind == TransferKind::Downloading)
            .map(|r| r.manifest_id)
        {
            clear_partial(self.partial_dir(mid));
        }

        // Only a completed *download* has an output folder we may delete — a
        // seed's `source_dir` is the user's own folder and is never touched.
        if delete_files
            && let Some(dir) = self
                .state
                .transfers
                .get(&id)
                .filter(|r| r.kind == TransferKind::Downloading)
                .and_then(|r| r.output_dir.clone())
        {
            tokio::task::spawn_blocking(move || {
                if let Err(e) = std::fs::remove_dir_all(&dir) {
                    tracing::warn!(dir = %dir.display(), error = %e, "could not delete output folder");
                }
            });
        }

        self.state.transfers.remove(&id);
        self.recount();
        self.persist_shares();
        self.publish();
    }

    fn alloc_id(&mut self) -> TransferId {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn insert_row(&mut self, row: TransferRow) {
        let id = row.id;
        self.state.transfers.insert(id, row);
        self.recount();
        self.publish();
        let _ = self.events.send(AppEvent::TransferAdded(id));
    }

    fn recount(&mut self) {
        let mut seeding = 0;
        let mut downloading = 0;
        for row in self.state.transfers.values() {
            match row.kind {
                TransferKind::Seeding => seeding += 1,
                TransferKind::Downloading => downloading += 1,
            }
        }
        self.state.swarm = SwarmStatus {
            download_peer_id: Some(self.download_node.peer_id()),
            seeding,
            downloading,
        };
    }

    fn publish(&self) {
        let _ = self.state_tx.send(self.state.clone());
        let _ = self.events.send(AppEvent::Changed);
    }

    fn partial_dir(&self, manifest_id: Hash) -> PathBuf {
        self.state.settings.download_dir.join(".gaggle-partial").join(hex(&manifest_id))
    }

    /// `shares.json`, next to the settings file. `None` when there is no
    /// config path (e.g. a headless/test `App`) — persistence is simply
    /// unavailable then, same as `Settings` itself.
    fn shares_path(&self) -> Option<PathBuf> {
        self.config_path.as_deref()?.parent().map(|dir| dir.join("shares.json"))
    }

    /// Re-run every persisted seed/subscription from the last session. Called
    /// once, before the manager's command loop starts. A malformed or
    /// unreadable file is silently ignored — it just means an empty start,
    /// same as if persistence had never run.
    fn restore_persisted(&mut self) {
        if !self.state.settings.persist_shares {
            return;
        }
        if let Some(saved) = self.state.settings.accelerator.clone() {
            self.restore_accelerator(saved);
        }
        let Some(path) = self.shares_path() else { return };
        let Ok(bytes) = std::fs::read(&path) else { return };
        let persisted: PersistedState = match serde_json::from_slice(&bytes) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "could not parse persisted shares");
                return;
            }
        };
        for seed in persisted.seeds {
            self.start_seed(seed.dir, seed.share_seed, seed.version.max(1));
        }
        for request in persisted.subscriptions {
            self.subscribe(request);
        }
    }

    /// Restart the local accelerator [`Settings::accelerator`] describes —
    /// the "always-on" NAS/relay this node was running before it last
    /// restarted. An unparseable saved share token is dropped with a warning
    /// rather than failing the whole restore.
    fn restore_accelerator(&mut self, saved: PersistedAccelerator) {
        let shares: Vec<ShareLink> = saved
            .shares
            .iter()
            .filter_map(|t| match ShareLink::parse(t) {
                Ok(l) => Some(l),
                Err(e) => {
                    tracing::warn!(error = %e, "dropping an unparseable saved accelerator share");
                    None
                }
            })
            .collect();
        let request = match saved.role {
            PersistedAccelRole::Relay => {
                AcceleratorRequest::Relay { cache_bytes: saved.cache_bytes, shares }
            }
            PersistedAccelRole::Nas => AcceleratorRequest::Nas {
                dir: saved.dir.unwrap_or_else(|| self.state.settings.download_dir.join(".gaggle-nas")),
                shares,
            },
        };
        self.start_accelerator(request);
    }

    /// Rewrite `shares.json` from the live seed/download/subscription maps.
    /// A no-op when persistence is off or there is no config path. Best
    /// -effort, like `Settings::save` — a write failure is logged, not fatal.
    fn persist_shares(&self) {
        if !self.state.settings.persist_shares {
            return;
        }
        let Some(path) = self.shares_path() else { return };
        let persisted = PersistedState {
            seeds: self
                .seeds
                .values()
                .map(|s| PersistedSeed { dir: s.dir.clone(), share_seed: s.share_seed, version: s.version })
                .collect(),
            subscriptions: self
                .downloads
                .values()
                .map(|j| j.request.clone())
                .chain(self.subs.values().map(|s| s.request.clone()))
                .collect(),
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_vec_pretty(&persisted) {
            Ok(bytes) => {
                if let Err(e) = std::fs::write(&path, bytes) {
                    tracing::warn!(path = %path.display(), error = %e, "could not save persisted shares");
                }
            }
            Err(e) => tracing::warn!(error = %e, "could not serialize persisted shares"),
        }
    }
}

fn new_row(id: TransferId, name: String, kind: TransferKind) -> TransferRow {
    TransferRow {
        id,
        name,
        kind,
        status: TransferStatus::Connecting,
        manifest_id: Hash::of(b""),
        files: 0,
        total_bytes: 0,
        done_bytes: 0,
        speed_bps: 0,
        sources: Vec::new(),
        share_addr: None,
        share_addrs: Vec::new(),
        output_dir: None,
        error: None,
        version: 0,
        private: false,
        source_dir: None,
        file_paths: Arc::new(Vec::new()),
        update_available: None,
        detail: None,
    }
}

async fn fail(tx: &mpsc::Sender<Command>, id: TransferId, error: String) {
    let _ = tx.send(Command::WorkerFailed { id, error }).await;
}

/// Reserve a circuit slot on the relay at `relay_addr` (a `…/p2p/<id>`
/// multiaddr, [`Settings::public_relay`](crate::Settings::public_relay)) and
/// return the resulting `/p2p-circuit/…/p2p/<self>` address — dialable even
/// when `node` sits behind a NAT with no other path reachable from a
/// subscriber, with dcutr opportunistically upgrading to a direct connection
/// once both sides have connected through the relay.
async fn reserve_relay(node: &Node, relay_addr: &str) -> anyhow::Result<Multiaddr> {
    let addr: Multiaddr = relay_addr.trim().parse()?;
    // `bootstrap` both dials the relay (blocking until the connection is
    // live — the reservation needs one) and joins its DHT, so the origin
    // also becomes discoverable through the relay's bootstrap/rendezvous role.
    let relay = node.bootstrap(addr.clone()).await?;
    let circuit = node.reserve_relay_slot(relay, addr).await?;
    tracing::info!(%relay, %circuit, "relay reservation established");
    Ok(circuit)
}

/// How long a subscriber waits for the origin to answer a rendezvous request
/// before giving up and falling back to whatever's already in the share link.
const RENDEZVOUS_TIMEOUT: Duration = Duration::from_secs(8);
const RENDEZVOUS_POLL_INTERVAL: Duration = Duration::from_millis(700);

/// How often [`Manager::tick`] re-announces every locally-served share to the
/// seeder tracker. Comfortably under `control_plane::tracker`'s entry TTL, so
/// one missed announce doesn't drop a live seed from the directory.
const TRACKER_ANNOUNCE_INTERVAL: Duration = Duration::from_secs(30);
/// How long a download waits on the seeder tracker before proceeding with
/// just the share-link sources. Short — a slow tracker must not delay a
/// transfer whose link already names a working source.
const TRACKER_QUERY_TIMEOUT: Duration = Duration::from_secs(4);

/// Publish every locally-served share to the seeder tracker at `url`
/// (best-effort — a missing or unreachable tracker is a silent no-op).
/// `nodes` are per-share serving nodes (origin seeds and NAS replicas);
/// `relay`, if present, is announced for every share it currently caches.
async fn announce_to_tracker(
    url: &str,
    nodes: Vec<(Hash, Arc<Node>)>,
    relay: Option<Arc<RelayNode>>,
) {
    let client = TrackerClient::new(url);
    for (id, node) in nodes {
        let Ok(addrs) = node.reachable_addrs().await else { continue };
        if addrs.is_empty() {
            continue;
        }
        let me = PeerInfo {
            peer_id: node.peer_id().to_string(),
            addrs: addrs.iter().map(Multiaddr::to_string).collect(),
        };
        let _ = client.announce(&id.to_hex(), &me).await;
    }
    if let Some(relay) = relay {
        let (Ok(addrs), Ok(shares)) = (relay.reachable_addrs().await, relay.shares().await) else {
            return;
        };
        if addrs.is_empty() {
            return;
        }
        let me = PeerInfo {
            peer_id: relay.peer_id().to_string(),
            addrs: addrs.iter().map(Multiaddr::to_string).collect(),
        };
        for id in shares {
            let _ = client.announce(&id.to_hex(), &me).await;
        }
    }
}

/// Ask the seeder tracker at `url` which peers currently serve `manifest_id`
/// and merge their addresses into `sources`, de-duplicated. Best-effort and
/// time-bounded: a missing, slow, or empty tracker just means "no extra
/// sources this time", never a hard failure. Every chunk a discovered source
/// serves is still verified against the manifest root, exactly like one from
/// the share link.
async fn merge_tracked_sources(url: &str, manifest_id: Hash, sources: &mut Vec<Multiaddr>) {
    let client = TrackerClient::new(url);
    let found = match tokio::time::timeout(
        TRACKER_QUERY_TIMEOUT,
        client.seeders(&manifest_id.to_hex()),
    )
    .await
    {
        Ok(Ok(list)) => list,
        _ => return,
    };
    let mut added = 0usize;
    for info in found {
        for addr in info.addrs {
            if let Ok(addr) = addr.parse::<Multiaddr>()
                && !sources.contains(&addr)
            {
                sources.push(addr);
                added += 1;
            }
        }
    }
    if added > 0 {
        tracing::info!(added, share = %manifest_id.to_hex(), "seeder tracker added extra source address(es)");
    }
}

/// Subscriber side of the NAT-rendezvous handshake: register with `origin` at
/// the accelerator hosting `rendezvous_url`, wait for it to answer with its
/// own current candidate addresses, and dial them right away — that dial is
/// this side's half of the punch (the origin does its own half when it
/// answers, in [`answer_rendezvous_requests`]). Returns the origin's
/// candidate addresses so the caller can add them to its normal dial list;
/// a timeout or any transport error is just "rendezvous didn't help this
/// time", never a hard failure for the download as a whole.
async fn punch_via_rendezvous(
    node: &Node,
    rendezvous_url: &str,
    origin: PeerId,
) -> anyhow::Result<Vec<Multiaddr>> {
    let client = RendezvousClient::new(rendezvous_url);
    let origin_id = origin.to_string();
    let me = PeerInfo {
        peer_id: node.peer_id().to_string(),
        addrs: node.reachable_addrs().await?.iter().map(Multiaddr::to_string).collect(),
    };
    let request_id = client.register(&origin_id, &me).await?;

    let deadline = Instant::now() + RENDEZVOUS_TIMEOUT;
    loop {
        if let Some(answer) = client.poll_answer(&origin_id, &request_id).await? {
            let addrs: Vec<Multiaddr> =
                answer.addrs.iter().filter_map(|s| s.parse().ok()).collect();
            for addr in &addrs {
                // Best-effort, bounded — this is the punch itself, not a
                // required step (`connect_all` retries these addresses right
                // after anyway), and a dead/slow address must not stall this
                // return past the caller's own timeout budget.
                let _ = tokio::time::timeout(PUNCH_DIAL_TIMEOUT, node.bootstrap(addr.clone())).await;
            }
            return Ok(addrs);
        }
        if Instant::now() >= deadline {
            anyhow::bail!("{origin_id} did not answer the rendezvous request in time");
        }
        tokio::time::sleep(RENDEZVOUS_POLL_INTERVAL).await;
    }
}

/// Cap on one punch-dial attempt. Its job is just to get the outbound packet
/// out (which happens as soon as the dial is issued, well before this
/// resolves) — a NAT'd/dead address must not stall the whole rendezvous
/// exchange while this awaits a connection that will never complete.
const PUNCH_DIAL_TIMEOUT: Duration = Duration::from_millis(800);

/// Origin side of the NAT-rendezvous handshake, polled once per share per
/// [`Manager::tick`]: check whether any subscriber is waiting for `node` to
/// show up, publish `node`'s own addresses as the answer, then dial each
/// subscriber's candidate addresses (this side's half of the punch — done
/// *after* answering, so a slow/dead punch dial can never delay the
/// subscriber seeing the answer). Silent on any error — this is a
/// best-effort optimization on top of whatever addresses are already in the
/// share link, not something a share depends on.
async fn answer_rendezvous_requests(node: &Node, rendezvous_url: &str) {
    let client = RendezvousClient::new(rendezvous_url);
    let my_id = node.peer_id().to_string();
    let pending = match client.pending(&my_id).await {
        Ok(p) if !p.is_empty() => p,
        _ => return,
    };
    let Ok(addrs) = node.reachable_addrs().await else { return };
    let me = PeerInfo { peer_id: my_id.clone(), addrs: addrs.iter().map(Multiaddr::to_string).collect() };

    for req in pending {
        if let Err(e) = client.answer(&my_id, &req.request_id, &me).await {
            tracing::debug!(error = %e, "rendezvous answer failed");
            continue;
        }
        for addr_str in &req.subscriber.addrs {
            if let Ok(addr) = addr_str.parse::<Multiaddr>() {
                let _ = tokio::time::timeout(PUNCH_DIAL_TIMEOUT, node.bootstrap(addr)).await;
            }
        }
    }
}

/// Replicate `link` onto `dir_root` under a NAS replica's persistent
/// `identity`, trying a NAT-rendezvous punch first when `rendezvous_url` is
/// set and the link names a peer id (same idea as [`run_download`]'s: the
/// punch has to happen on the very node that then does the real connect/pull,
/// since that's the node whose NAT mapping it opens a hole in — so this
/// spawns its own scratch node rather than reusing [`nas_add_share`]'s).
/// Reports [`SwarmProgress`] once per chunk via `on_progress`.
async fn nas_replicate(
    dir_root: &std::path::Path,
    identity: Keypair,
    link: &ShareLink,
    rendezvous_url: Option<&str>,
    on_progress: impl FnMut(SwarmProgress),
) -> anyhow::Result<(Node, net::accel::ShareMeta, usize)> {
    let scratch = Node::spawn().await?;
    let mut link = link.clone();
    if let Some(url) = rendezvous_url
        && let Some(origin) = link.sources.iter().find_map(peer_id_of)
        && let Ok(extra) = punch_via_rendezvous(&scratch, url, origin).await
    {
        for addr in extra {
            if !link.sources.contains(&addr) {
                link.sources.push(addr);
            }
        }
    }
    let pulled = nas_pull_with_progress(&scratch, dir_root, &link, on_progress).await;
    scratch.shutdown().await;
    let (manifest, chunk_lists, disk, chunks) = pulled?;
    nas_serve(manifest, chunk_lists, disk, chunks, identity, &link).await
}

/// A throttled [`index_dir_with_progress`] callback that forwards
/// `Command::ScanProgress` to the manager — at most a few times a second
/// (always including the very first and last update), so a fast local scan
/// doesn't flood the command channel. Called from inside `spawn_blocking`, so
/// it sends with `blocking_send`.
fn scan_progress_sink(tx: mpsc::Sender<Command>, id: TransferId) -> impl FnMut(ScanProgress) {
    let mut last_sent = Instant::now()
        .checked_sub(Duration::from_secs(1))
        .unwrap_or_else(Instant::now);
    move |p: ScanProgress| {
        let done = p.files_total == 0 || p.files_done >= p.files_total;
        if done || last_sent.elapsed() >= Duration::from_millis(150) {
            last_sent = Instant::now();
            let _ = tx.blocking_send(Command::ScanProgress {
                id,
                files_total: p.files_total,
                bytes_done: p.bytes_done,
                bytes_total: p.bytes_total,
            });
        }
    }
}

/// Delete a download's scratch chunk store off-thread, and the shared
/// `.gaggle-partial` parent once it's left empty. Best-effort.
fn clear_partial(dir: PathBuf) {
    tokio::task::spawn_blocking(move || {
        let _ = std::fs::remove_dir_all(&dir);
        if let Some(parent) = dir.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    });
}

/// How long a download/resync worker may go with no observable progress — no
/// phase change, no chunk verified — before it is treated as stuck and failed
/// with a clear error. Without this, a dial that neither connects nor errors
/// (seen on some networks/firewalls) leaves a transfer sitting on
/// "Connecting" forever with nothing in the UI to say anything is wrong.
/// Generous on purpose: fetching metadata for a share with many thousands of
/// files is legitimately slow, not stuck, and a relay reservation + dcutr
/// hole-punch through a slow/flaky NAT can genuinely take minutes — 90s was
/// tripping on connections that would have come through fine given more time.
const STALL_TIMEOUT: Duration = Duration::from_secs(300);

/// Update a transfer's `detail` text and mark `activity` so the stall
/// watchdog racing this worker knows it is still making progress, even
/// before any chunk has actually moved. Also logged, so a stuck run shows up
/// in the Logs tab.
fn report_stage(
    tx: &mpsc::Sender<Command>,
    activity: &Mutex<Instant>,
    id: TransferId,
    message: impl Into<String>,
) {
    let message = message.into();
    *activity.lock().unwrap() = Instant::now();
    tracing::info!(id, "{message}");
    let _ = tx.try_send(Command::DownloadStage { id, message });
}

/// Race `work` against a watchdog that fails it once `activity` — touched by
/// `work` on every phase change and every chunk received — has been stale for
/// longer than [`STALL_TIMEOUT`].
async fn with_stall_watchdog<T>(
    activity: Arc<Mutex<Instant>>,
    work: impl Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    tokio::pin!(work);
    loop {
        tokio::select! {
            result = &mut work => return result,
            _ = tokio::time::sleep(Duration::from_secs(5)) => {
                let stale = activity.lock().unwrap().elapsed();
                if stale > STALL_TIMEOUT {
                    anyhow::bail!(
                        "no response from any source for {}s — the source may be \
                         unreachable (check your network connection and firewall), \
                         or offline",
                        STALL_TIMEOUT.as_secs()
                    );
                }
            }
        }
    }
}

async fn run_download(
    node: &Node,
    id: TransferId,
    req: SubscribeRequest,
    chunk_dir: PathBuf,
    output_dir: PathBuf,
    tx: mpsc::Sender<Command>,
    rendezvous_url: Option<String>,
) -> anyhow::Result<()> {
    anyhow::ensure!(!req.sources.is_empty(), "no sources given for the subscription");
    let activity = Arc::new(Mutex::new(Instant::now()));
    let work_activity = activity.clone();

    let work = async move {
        let mut req = req;
        if let Some(url) = rendezvous_url.as_deref()
            && let Some(origin) = req.sources.iter().find_map(peer_id_of)
        {
            report_stage(&tx, &work_activity, id, "trying a direct NAT punch…");
            if let Ok(extra) = punch_via_rendezvous(node, url, origin).await {
                for addr in extra {
                    if !req.sources.contains(&addr) {
                        req.sources.push(addr);
                    }
                }
            }
        }
        // Ask the accelerator's seeder tracker for any *other* peers serving
        // this share (a NAS replica, a second origin) and swarm across them
        // all, not just the address in the link.
        if let Some(url) = rendezvous_url.as_deref() {
            merge_tracked_sources(url, req.manifest_id, &mut req.sources).await;
        }
        report_stage(
            &tx,
            &work_activity,
            id,
            format!("resolving {} source(s)…", req.sources.len()),
        );
        let peers = node.connect_all(&req.sources).await?;
        if let Some(cred) = &req.credential {
            report_stage(&tx, &work_activity, id, "authenticating…");
            node.authenticate_all(&peers, cred).await?;
        }

        let dir = chunk_dir.clone();
        let mut disk = tokio::task::spawn_blocking(move || DiskChunkStore::open(&dir)).await??;
        let base_bytes = disk.size_on_disk().unwrap_or(0);

        report_stage(&tx, &work_activity, id, "fetching share metadata…");
        let progress_tx = tx.clone();
        let progress_activity = work_activity.clone();
        // Pin the id the invite/link promised: a source discovered for one share
        // must not be able to substitute a different manifest (chunks would still
        // "verify" — against the attacker's manifest). Resync deliberately does not
        // pin (a rescan changes the id); the first download always can.
        let config = SwarmConfig { manifest_id: Some(req.manifest_id), ..swarm_config_for(&req) };
        let dl = node
            .download_share_multi_with_progress(
                &peers,
                &mut disk,
                config,
                move |p: SwarmProgress| {
                    *progress_activity.lock().unwrap() = Instant::now();
                    let _ = progress_tx.try_send(Command::DownloadProgress { id, p, base_bytes });
                },
            )
            .await?;

        let manifest = dl.share.manifest.clone();
        let chunk_lists = dl.share.chunk_lists.clone();
        let total_bytes = manifest.total_size();
        let files = manifest.files.len();
        let version = manifest.version;

        let out = output_dir.clone();
        let m2 = manifest.clone();
        let l2 = chunk_lists.clone();
        tokio::task::spawn_blocking(move || write_share(&out, &m2, &l2, &disk)).await??;

        let _ = tx
            .send(Command::DownloadDone {
                id,
                outcome: Box::new(DownloadOutcome {
                    files,
                    total_bytes,
                    output_dir,
                    sources: dl.chunks_per_source,
                    manifest,
                    chunk_lists,
                    version,
                }),
            })
            .await;
        Ok(())
    };

    with_stall_watchdog(activity, work).await
}

async fn run_resync(
    node: &Node,
    id: TransferId,
    req: SubscribeRequest,
    output_dir: PathBuf,
    old_manifest: Manifest,
    tx: mpsc::Sender<Command>,
    rendezvous_url: Option<String>,
) -> anyhow::Result<()> {
    anyhow::ensure!(!req.sources.is_empty(), "no sources given for the subscription");
    let activity = Arc::new(Mutex::new(Instant::now()));
    let work_activity = activity.clone();

    let work = async move {
        let mut req = req;
        let name = sanitize(&req.name).unwrap_or_else(|| hex(&req.manifest_id));
        if let Some(url) = rendezvous_url.as_deref() {
            merge_tracked_sources(url, req.manifest_id, &mut req.sources).await;
        }
        report_stage(
            &tx,
            &work_activity,
            id,
            format!("resolving {} source(s)…", req.sources.len()),
        );
        let peers = node.connect_all(&req.sources).await?;
        if let Some(cred) = &req.credential {
            report_stage(&tx, &work_activity, id, "authenticating…");
            node.authenticate_all(&peers, cred).await?;
        }

        report_stage(&tx, &work_activity, id, "scanning existing files…");
        // Recover chunks that still live in the materialized output tree, so only
        // genuinely new bytes are pulled.
        let scan_dir = output_dir.clone();
        let scan_name = name;
        let old_v = old_manifest.version;
        let mut mem = tokio::task::spawn_blocking(move || {
            let mut mem = MemoryChunkStore::new();
            let _ = snapshot_dir(&scan_dir, scan_name, old_v, &mut mem);
            mem
        })
        .await?;

        report_stage(&tx, &work_activity, id, "fetching share metadata…");
        let progress_tx = tx.clone();
        let progress_activity = work_activity.clone();
        let dl = node
            .download_share_multi_with_progress(
                &peers,
                &mut mem,
                swarm_config_for(&req),
                move |p: SwarmProgress| {
                    *progress_activity.lock().unwrap() = Instant::now();
                    let _ = progress_tx.try_send(Command::ResyncProgress { id, p });
                },
            )
            .await?;

        let new_manifest = dl.share.manifest.clone();
        let new_lists = dl.share.chunk_lists.clone();
        let files = new_manifest.files.len();
        let total_bytes = new_manifest.total_size();

        let out2 = output_dir.clone();
        let om = old_manifest.clone();
        let nm = new_manifest.clone();
        let nl = new_lists.clone();
        let synced =
            tokio::task::spawn_blocking(move || sync_share(&out2, &om, &nm, &nl, &mem)).await??;

        let _ = tx
            .send(Command::ResyncDone {
                id,
                outcome: Box::new(ResyncOutcome {
                    manifest: new_manifest,
                    chunk_lists: new_lists,
                    synced,
                    files,
                    total_bytes,
                }),
            })
            .await;
        Ok(())
    };

    with_stall_watchdog(activity, work).await
}

/// Fetch the current manifest version from the first source that answers,
/// including any extra seeders the tracker at `tracker_url` knows about.
async fn check_remote_version(
    node: &Node,
    req: &SubscribeRequest,
    tracker_url: Option<&str>,
) -> anyhow::Result<u64> {
    anyhow::ensure!(!req.sources.is_empty(), "no sources given for the subscription");
    let mut sources = req.sources.clone();
    if let Some(url) = tracker_url {
        merge_tracked_sources(url, req.manifest_id, &mut sources).await;
    }
    let mut last_err = None;
    for addr in &sources {
        let attempt = async {
            let peer = node.connect(addr.clone()).await?;
            if let Some(cred) = &req.credential {
                node.authenticate(peer, cred).await?;
            }
            // `None`: a rescan changes the share's id, and this check exists to
            // notice exactly that — pinning the old id would always miss it.
            anyhow::Ok(node.fetch_manifest(peer, None).await?.version)
        };
        match attempt.await {
            Ok(v) => return Ok(v),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("no sources to ask")))
}

/// A [`SwarmConfig`] honouring the subscription's per-file scope, if any.
///
/// We deliberately leave `manifest_id` unset: sources are origins/replicas that
/// each serve one share, and a rescan changes a share's id, so pinning it would
/// break resync. A multi-share relay only ever appears as a *supplementary*
/// source next to an origin that answers the metadata request.
fn swarm_config_for(req: &SubscribeRequest) -> SwarmConfig {
    let allowed_paths = req.credential.as_ref().and_then(|c| match &c.capability.scope {
        Scope::All => None,
        Scope::Files(paths) => Some(paths.clone()),
    });
    SwarmConfig { allowed_paths, ..SwarmConfig::default() }
}

fn row_from_meta(
    m: &net::accel::ShareMeta,
    replica_chunks: Option<u64>,
    listen_addr: Option<String>,
) -> AccelShareRow {
    AccelShareRow {
        manifest_id: m.manifest_id.to_hex(),
        name: m.name.clone(),
        files: m.files,
        total_bytes: m.total_bytes,
        version: m.version,
        private: m.private,
        replica_chunks,
        listen_addr,
        replicating: None,
        error: None,
    }
}

fn err_row(name: &str, manifest_id: &str, error: String) -> AccelShareRow {
    AccelShareRow {
        manifest_id: manifest_id.to_string(),
        name: name.to_string(),
        files: 0,
        total_bytes: 0,
        version: 0,
        private: false,
        replica_chunks: None,
        listen_addr: None,
        replicating: None,
        error: Some(error),
    }
}

fn share_status_row(s: &control_plane::ShareStatus) -> AccelShareRow {
    AccelShareRow {
        manifest_id: s.manifest_id.clone(),
        name: s.name.clone(),
        files: s.files,
        total_bytes: s.total_bytes,
        version: s.version,
        private: s.private,
        replica_chunks: s.replica_chunks,
        listen_addr: s.listen_addr.clone(),
        replicating: s.replicating.as_ref().map(|p| ReplicaProgress {
            chunks_done: p.chunks_done,
            chunks_total: p.chunks_total,
            bytes_done: p.bytes_done,
            bytes_total: p.bytes_total,
        }),
        error: s.error.clone(),
    }
}

fn accel_detail(role: AcceleratorRole, shares: &[AccelShareRow]) -> String {
    let ok = shares.iter().filter(|s| s.error.is_none()).count();
    match role {
        AcceleratorRole::Relay if ok == 0 => "relay + Kademlia bootstrap".to_string(),
        AcceleratorRole::Relay => format!("caching {ok} share(s)"),
        AcceleratorRole::Nas => format!("replicating {ok} share(s) on disk"),
    }
}

async fn start_relay_accel(
    cache_bytes: u64,
    shares: Vec<ShareLink>,
) -> anyhow::Result<(AccelHandle, AcceleratorState)> {
    let relay = Arc::new(
        RelayNode::spawn_with(RelayConfig { cache_capacity_bytes: cache_bytes }).await?,
    );
    let meta = Arc::new(Node::spawn().await?);
    let peer_id = relay.peer_id();

    let mut rows = Vec::new();
    let mut tokens = Vec::new();
    for link in &shares {
        match relay_add_share(&relay, &meta, link).await {
            Ok(m) => {
                rows.push(row_from_meta(&m, None, None));
                tokens.push(link.clone().encode());
            }
            Err(e) => {
                tracing::warn!(name = %link.name, error = %format!("{e:#}"), "relay could not cache share");
                rows.push(err_row(&link.name, &link.manifest_id.to_hex(), format!("{e:#}")));
            }
        }
    }

    let listen_addrs = relay.listen_addr().await.ok().into_iter().collect();
    let cache = relay.cache_stats().await.ok();
    let state = AcceleratorState {
        role: AcceleratorRole::Relay,
        peer_id,
        listen_addrs,
        detail: accel_detail(AcceleratorRole::Relay, &rows),
        cache,
        replica_chunks: None,
        shares: rows.clone(),
    };
    let handle = AccelHandle {
        role: AcceleratorRole::Relay,
        relay: Some(relay),
        meta: Some(meta),
        nas_dir: None,
        nas_nodes: Vec::new(),
        rows,
        tokens,
    };
    Ok((handle, state))
}

async fn start_nas_accel(
    dir: PathBuf,
    shares: Vec<ShareLink>,
    operator_seed: [u8; 32],
    rendezvous_url: Option<String>,
) -> anyhow::Result<(AccelHandle, AcceleratorState)> {
    anyhow::ensure!(!shares.is_empty(), "a NAS accelerator needs at least one share");
    tokio::fs::create_dir_all(&dir).await.ok();

    let mut rows = Vec::new();
    let mut tokens = Vec::new();
    let mut nas_nodes: Vec<(Hash, Arc<Node>)> = Vec::new();
    for link in &shares {
        let seed = derive_share_seed(&operator_seed, link.manifest_id);
        match nas_replicate(&dir, net::keypair_from_seed(seed), link, rendezvous_url.as_deref(), |_| {})
            .await
        {
            Ok((node, m, chunks)) => {
                let addr = node.listen_addr().await.ok().map(|a| a.to_string());
                nas_nodes.push((m.manifest_id, Arc::new(node)));
                rows.push(row_from_meta(&m, Some(chunks as u64), addr));
                tokens.push(link.clone().encode());
            }
            Err(e) => {
                tracing::warn!(name = %link.name, error = %format!("{e:#}"), "nas could not replicate share");
                rows.push(err_row(&link.name, &link.manifest_id.to_hex(), format!("{e:#}")));
            }
        }
    }

    let peer_id = nas_nodes
        .first()
        .map(|(_, n)| n.peer_id())
        .ok_or_else(|| anyhow::anyhow!("no share could be replicated"))?;
    let listen_addrs = match nas_nodes.first() {
        Some((_, n)) => n.listen_addr().await.ok().into_iter().collect(),
        None => Vec::new(),
    };
    let replica_chunks = rows.iter().filter_map(|r| r.replica_chunks).sum::<u64>() as usize;
    let state = AcceleratorState {
        role: AcceleratorRole::Nas,
        peer_id,
        listen_addrs,
        detail: accel_detail(AcceleratorRole::Nas, &rows),
        cache: None,
        replica_chunks: Some(replica_chunks),
        shares: rows.clone(),
    };
    let handle = AccelHandle {
        role: AcceleratorRole::Nas,
        relay: None,
        meta: None,
        nas_dir: Some(dir),
        nas_nodes,
        rows,
        tokens,
    };
    Ok((handle, state))
}

/// Sequential-write throughput to `dir` + free space, and a role suggestion.
fn run_benchmark(dir: &std::path::Path) -> anyhow::Result<BenchmarkResult> {
    use std::io::Write;

    std::fs::create_dir_all(dir)?;
    let probe = dir.join(".gaggle-benchmark.tmp");
    let payload = vec![0u8; 1 << 20];
    let total: u64 = 64 << 20;

    let started = Instant::now();
    {
        let mut f = std::fs::File::create(&probe)?;
        for _ in 0..(total >> 20) {
            f.write_all(&payload)?;
        }
        f.sync_all()?;
    }
    let secs = started.elapsed().as_secs_f64().max(1e-3);
    let _ = std::fs::remove_file(&probe);

    let disk_write_bps = (total as f64 / secs) as u64;
    let free_bytes = free_space(dir).unwrap_or(0);
    let suggested = if free_bytes >= 50 << 30 && disk_write_bps >= 20 << 20 {
        AcceleratorRole::Nas
    } else {
        AcceleratorRole::Relay
    };
    Ok(BenchmarkResult { disk_write_bps, free_bytes, suggested })
}

#[cfg(unix)]
fn free_space(path: &std::path::Path) -> std::io::Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::other("path has an interior NUL"))?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut stat) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(stat.f_bavail as u64 * stat.f_frsize as u64)
}

#[cfg(windows)]
fn free_space(path: &std::path::Path) -> std::io::Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    // NUL-terminated UTF-16 of the directory; the API accepts a path on the
    // volume, not just a drive root.
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut free_to_caller: u64 = 0;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free_to_caller,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(free_to_caller)
}

#[cfg(not(any(unix, windows)))]
fn free_space(_path: &std::path::Path) -> std::io::Result<u64> {
    Ok(0)
}

/// Minimum wall-clock gap between two speed samples. A `DownloadProgress`
/// fires once per chunk, and several chunks can land in the same tokio poll
/// (parallel in-flight requests completing back to back) — computing a rate
/// from two samples microseconds apart divides real bytes by a near-zero
/// `dt` and reports absurd multi-GiB/s speeds. Below this interval, bytes are
/// left to accumulate against the last real sample instead of recomputing.
const MIN_SAMPLE_INTERVAL: Duration = Duration::from_millis(200);

/// Update `slot` (`last_sample`) with the latest `(time, bytes_done)` reading
/// and return a new bytes/sec rate — but only once at least
/// [`MIN_SAMPLE_INTERVAL`] has passed since the sample `slot` was last
/// updated. `None` means "too soon, or this is the first sample" — the
/// caller should leave the existing displayed speed alone rather than
/// overwrite it with a noisy or unset value.
fn sample_speed(slot: &mut Option<(Instant, u64)>, now: Instant, done: u64) -> Option<u64> {
    match *slot {
        Some((t0, b0)) if now.duration_since(t0) >= MIN_SAMPLE_INTERVAL => {
            let dt = now.duration_since(t0).as_secs_f64();
            *slot = Some((now, done));
            Some((done.saturating_sub(b0) as f64 / dt) as u64)
        }
        Some(_) => None,
        None => {
            *slot = Some((now, done));
            None
        }
    }
}

/// Credit one chunk to a source in a row's per-source breakdown.
fn bump_source(sources: &mut Vec<SourceStats>, peer: PeerId, chunk_len: u64) {
    match sources.iter_mut().find(|s| s.peer == peer) {
        Some(s) => {
            s.chunks += 1;
            s.bytes += chunk_len;
        }
        None => sources.push(SourceStats { peer, chunks: 1, bytes: chunk_len }),
    }
    sources.sort_by_key(|s| s.peer.to_bytes());
}

/// A filesystem-safe folder name, or `None` if nothing usable remains.
fn sanitize(name: &str) -> Option<String> {
    let last = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = last
        .chars()
        .map(|c| if c.is_alphanumeric() || " -_.".contains(c) { c } else { '_' })
        .collect();
    let trimmed = cleaned.trim_matches(['.', ' ']).to_string();
    (!trimmed.is_empty()).then_some(trimmed)
}

fn hex(h: &Hash) -> String {
    h.to_hex()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_paths_and_specials() {
        assert_eq!(sanitize("modpack v2").as_deref(), Some("modpack v2"));
        assert_eq!(sanitize("/tmp/some/Cool-Pack").as_deref(), Some("Cool-Pack"));
        assert_eq!(sanitize("a/b/weird:*name").as_deref(), Some("weird__name"));
        assert_eq!(sanitize("..").as_deref(), None);
        assert_eq!(sanitize("").as_deref(), None);
    }

    #[test]
    fn scan_progress_sink_always_sends_the_first_and_final_update() {
        let (tx, mut rx) = mpsc::channel(16);
        let mut sink = scan_progress_sink(tx, 42);

        // The very first call is always sent (the sink is seeded with a
        // backdated `last_sent`), even though nothing has actually elapsed.
        sink(ScanProgress { files_done: 0, files_total: 2, bytes_done: 0, bytes_total: 200 });
        // A same-instant follow-up mid-scan tick may legitimately be
        // throttled away — only the first and last update are guaranteed.
        sink(ScanProgress { files_done: 1, files_total: 2, bytes_done: 100, bytes_total: 200 });
        // The final ("done") tick always goes through regardless of timing.
        sink(ScanProgress { files_done: 2, files_total: 2, bytes_done: 200, bytes_total: 200 });

        let mut seen = Vec::new();
        while let Ok(cmd) = rx.try_recv() {
            seen.push(cmd);
        }
        assert!(seen.len() >= 2, "expected at least the first and last update, got {}", seen.len());

        let Command::ScanProgress { id, files_total, bytes_done, bytes_total } = &seen[0] else {
            panic!("expected a ScanProgress command");
        };
        assert_eq!(*id, 42);
        assert_eq!(*files_total, 2);
        assert_eq!(*bytes_done, 0);
        assert_eq!(*bytes_total, 200);

        let Command::ScanProgress { bytes_done, bytes_total, .. } = seen.last().unwrap() else {
            panic!("expected a ScanProgress command");
        };
        assert_eq!(*bytes_done, 200);
        assert_eq!(*bytes_total, 200);
    }
}
