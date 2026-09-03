//! The transfer manager: a background task that owns the `net` nodes and turns
//! high-level commands ("share this folder", "subscribe to that one", "pause")
//! into swarm activity, publishing an [`AppState`] snapshot after every change.
//!
//! The GUI never touches `net`. It calls the sync methods on [`App`], reads
//! [`App::snapshot`], and optionally listens on [`App::events`]. All the async
//! lives here.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use gaggle_core::{
    DiskChunkStore, Hash, MemoryChunkStore, SignedCapability, snapshot_dir, write_share,
};
use net::{Catalog, Multiaddr, Node, PeerId, SwarmConfig, SwarmProgress};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;

use crate::settings::Settings;
use crate::state::{
    AppState, SourceStats, SwarmStatus, TransferId, TransferKind, TransferRow, TransferStatus,
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
}

/// Everything needed to start pulling a remote share.
#[derive(Debug, Clone)]
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

enum Command {
    AddLocalShare { dir: PathBuf },
    Subscribe(SubscribeRequest),
    Pause(TransferId),
    Resume(TransferId),
    Remove(TransferId),
    UpdateSettings(Box<Settings>),
    Shutdown,

    // Internal, from worker tasks.
    LocalShareReady { id: TransferId, node: Box<Node>, addr: Multiaddr, info: ShareInfo },
    WorkerFailed { id: TransferId, error: String },
    DownloadProgress { id: TransferId, p: SwarmProgress, base_bytes: u64 },
    DownloadDone { id: TransferId, outcome: Box<DownloadOutcome> },
}

struct ShareInfo {
    name: String,
    manifest_id: Hash,
    files: usize,
    bytes: u64,
}

struct DownloadOutcome {
    files: usize,
    total_bytes: u64,
    output_dir: PathBuf,
    /// Authoritative chunk-count-per-source from the finished swarm download.
    sources: HashMap<PeerId, usize>,
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

        let state = AppState {
            settings,
            transfers: Default::default(),
            swarm: SwarmStatus {
                download_peer_id: Some(download_node.peer_id()),
                seeding: 0,
                downloading: 0,
            },
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
            seeds: HashMap::new(),
            downloads: HashMap::new(),
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

    /// Snapshot `dir` and start seeding it. A row appears immediately and flips
    /// to `Complete` once the snapshot finishes.
    pub fn add_local_share(&self, dir: impl Into<PathBuf>) {
        self.send(Command::AddLocalShare { dir: dir.into() });
    }

    /// Start pulling a remote share.
    pub fn subscribe(&self, request: SubscribeRequest) {
        self.send(Command::Subscribe(request));
    }

    pub fn pause(&self, id: TransferId) {
        self.send(Command::Pause(id));
    }

    pub fn resume(&self, id: TransferId) {
        self.send(Command::Resume(id));
    }

    /// Stop and forget a transfer (a seed stops serving; a download's partial
    /// chunks are discarded).
    pub fn remove(&self, id: TransferId) {
        self.send(Command::Remove(id));
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

struct DownloadJob {
    task: JoinHandle<()>,
    request: SubscribeRequest,
    chunk_dir: PathBuf,
    /// For the rolling speed estimate.
    last_sample: Option<(Instant, u64)>,
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
    seeds: HashMap<TransferId, Node>,
    downloads: HashMap<TransferId, DownloadJob>,
}

impl Manager {
    async fn run(mut self) {
        while let Some(command) = self.rx.recv().await {
            if matches!(command, Command::Shutdown) {
                break;
            }
            self.handle(command);
        }
        // Dropping `self` drops every `Node`, which aborts its swarm task.
        for job in self.downloads.values() {
            job.task.abort();
        }
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::AddLocalShare { dir } => self.add_local_share(dir),
            Command::Subscribe(req) => self.subscribe(req),
            Command::Pause(id) => self.pause(id),
            Command::Resume(id) => self.resume(id),
            Command::Remove(id) => self.remove(id),
            Command::UpdateSettings(s) => {
                self.state.settings = *s;
                if let Some(path) = &self.config_path
                    && let Err(e) = self.state.settings.save(path)
                {
                    tracing::warn!(error = %e, "could not save settings");
                }
                self.publish();
            }
            Command::Shutdown => {}

            Command::LocalShareReady { id, node, addr, info } => {
                self.seeds.insert(id, *node);
                if let Some(row) = self.state.transfers.get_mut(&id) {
                    row.name = info.name;
                    row.manifest_id = info.manifest_id;
                    row.files = info.files;
                    row.total_bytes = info.bytes;
                    row.done_bytes = info.bytes;
                    row.status = TransferStatus::Complete;
                    row.share_addr = Some(addr);
                }
                self.recount();
                self.publish();
                let _ = self.events.send(AppEvent::TransferCompleted(id));
            }
            Command::WorkerFailed { id, error } => {
                self.downloads.remove(&id);
                if let Some(row) = self.state.transfers.get_mut(&id) {
                    row.status = TransferStatus::Failed;
                    row.error = Some(error.clone());
                    row.speed_bps = 0;
                }
                self.recount();
                self.publish();
                let _ = self.events.send(AppEvent::TransferFailed(id, error));
            }
            Command::DownloadProgress { id, p, base_bytes } => {
                let now = Instant::now();
                let done = base_bytes + p.bytes_done;
                if let Some(job) = self.downloads.get_mut(&id) {
                    let speed = match job.last_sample.replace((now, done)) {
                        Some((t0, b0)) => {
                            let dt = now.duration_since(t0).as_secs_f64();
                            if dt > 0.0 {
                                (done.saturating_sub(b0) as f64 / dt) as u64
                            } else {
                                0
                            }
                        }
                        None => 0,
                    };
                    if let Some(row) = self.state.transfers.get_mut(&id) {
                        row.status = TransferStatus::Active;
                        row.total_bytes = base_bytes + p.bytes_total;
                        row.done_bytes = done;
                        // Blend with the previous estimate to smooth spikes.
                        row.speed_bps = if row.speed_bps == 0 {
                            speed
                        } else {
                            (row.speed_bps * 3 + speed) / 4
                        };
                        bump_source(&mut row.sources, p.from, p.chunk_len);
                    }
                    self.publish();
                    let _ = self.events.send(AppEvent::TransferProgress(id));
                }
            }
            Command::DownloadDone { id, outcome } => {
                // The scratch chunk store has served its purpose — the files are
                // now materialised in the output dir and a `Complete` transfer is
                // never resumed. Drop it so it doesn't sit around forever.
                if let Some(job) = self.downloads.remove(&id) {
                    clear_partial(job.chunk_dir);
                }
                if let Some(row) = self.state.transfers.get_mut(&id) {
                    row.status = TransferStatus::Complete;
                    row.files = outcome.files;
                    row.total_bytes = outcome.total_bytes;
                    row.done_bytes = outcome.total_bytes;
                    row.speed_bps = 0;
                    row.output_dir = Some(outcome.output_dir);
                    // Reconcile chunk counts with the authoritative tally; keep
                    // the byte figures accumulated from progress events.
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
        }
    }

    fn add_local_share(&mut self, dir: PathBuf) {
        let id = self.alloc_id();
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| dir.display().to_string());
        self.insert_row(TransferRow {
            id,
            name: name.clone(),
            kind: TransferKind::Seeding,
            status: TransferStatus::Connecting,
            manifest_id: Hash::of(b""),
            files: 0,
            total_bytes: 0,
            done_bytes: 0,
            speed_bps: 0,
            sources: Vec::new(),
            share_addr: None,
            output_dir: None,
            error: None,
        });

        let tx = self.self_tx.clone();
        tokio::spawn(async move {
            let built = tokio::task::spawn_blocking(move || {
                let mut store = MemoryChunkStore::new();
                let snap = snapshot_dir(&dir, name, 1, &mut store)?;
                anyhow::Ok((snap, store))
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
            };
            let catalog = Catalog::new(snap.manifest, snap.chunk_lists, store);
            let node = match Node::spawn_serving(catalog).await {
                Ok(n) => n,
                Err(e) => return fail(&tx, id, format!("could not start serving: {e:#}")).await,
            };
            let addr = match node.listen_addr().await {
                Ok(a) => a,
                Err(e) => return fail(&tx, id, format!("no listen address: {e:#}")).await,
            };
            let _ = tx
                .send(Command::LocalShareReady { id, node: Box::new(node), addr, info })
                .await;
        });
    }

    fn subscribe(&mut self, request: SubscribeRequest) {
        let id = self.alloc_id();
        self.insert_row(TransferRow {
            id,
            name: request.name.clone(),
            kind: TransferKind::Downloading,
            status: TransferStatus::Connecting,
            manifest_id: request.manifest_id,
            files: 0,
            total_bytes: 0,
            done_bytes: 0,
            speed_bps: 0,
            sources: Vec::new(),
            share_addr: None,
            output_dir: None,
            error: None,
        });
        let chunk_dir = self.partial_dir(request.manifest_id);
        self.spawn_download(id, request, chunk_dir);
        self.recount();
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

        let task = tokio::spawn(async move {
            let out = out_root.join(name);
            if let Err(e) = run_download(node.as_ref(), id, req, dir, out, tx.clone()).await {
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

    fn remove(&mut self, id: TransferId) {
        self.seeds.remove(&id);
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
            // Job already finished or failed; its scratch dir may still be here.
            clear_partial(self.partial_dir(mid));
        }
        self.state.transfers.remove(&id);
        self.recount();
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
}

async fn fail(tx: &mpsc::Sender<Command>, id: TransferId, error: String) {
    let _ = tx.send(Command::WorkerFailed { id, error }).await;
}

/// Delete a download's scratch chunk store off-thread, and the shared
/// `.gaggle-partial` parent once it's left empty. Best-effort — any error
/// (already gone, another download still using the parent) is ignored.
fn clear_partial(dir: PathBuf) {
    tokio::task::spawn_blocking(move || {
        let _ = std::fs::remove_dir_all(&dir);
        if let Some(parent) = dir.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    });
}

async fn run_download(
    node: &Node,
    id: TransferId,
    req: SubscribeRequest,
    chunk_dir: PathBuf,
    output_dir: PathBuf,
    tx: mpsc::Sender<Command>,
) -> anyhow::Result<()> {
    anyhow::ensure!(!req.sources.is_empty(), "no sources given for the subscription");

    let mut peers = Vec::with_capacity(req.sources.len());
    for addr in &req.sources {
        peers.push(node.connect(addr.clone()).await?);
    }
    if let Some(cred) = &req.credential {
        node.authenticate_all(&peers, cred).await?;
    }

    let dir = chunk_dir.clone();
    let mut disk = tokio::task::spawn_blocking(move || DiskChunkStore::open(&dir)).await??;
    let base_bytes = disk.size_on_disk().unwrap_or(0);

    let progress_tx = tx.clone();
    let dl = node
        .download_share_multi_with_progress(
            &peers,
            &mut disk,
            SwarmConfig::default(),
            move |p: SwarmProgress| {
                let _ = progress_tx.try_send(Command::DownloadProgress { id, p, base_bytes });
            },
        )
        .await?;

    let manifest = dl.share.manifest.clone();
    let chunk_lists = dl.share.chunk_lists.clone();
    let total_bytes = manifest.total_size();
    let files = manifest.files.len();

    let out = output_dir.clone();
    tokio::task::spawn_blocking(move || write_share(&out, &manifest, &chunk_lists, &disk)).await??;

    let _ = tx
        .send(Command::DownloadDone {
            id,
            outcome: Box::new(DownloadOutcome {
                files,
                total_bytes,
                output_dir,
                sources: dl.chunks_per_source,
            }),
        })
        .await;
    Ok(())
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
}
