//! The snapshot the GUI renders. Cloneable plain data — a view takes a copy and
//! diffs against it; nothing here knows about gpui.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use gaggle_core::Hash;
use net::{CacheStats, Multiaddr, PeerId};

use crate::settings::Settings;
use crate::stats::SpeedSample;

/// Stable per-session id for a share / transfer row.
pub type TransferId = u64;

/// Which direction a transfer runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferKind {
    /// A local folder this node originates and serves.
    Seeding,
    /// A remote share this node is pulling down.
    Downloading,
}

/// Lifecycle of a transfer row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferStatus {
    /// Accepted, not started yet.
    Queued,
    /// Seed only: the local folder is being walked and chunked. See
    /// [`TransferRow::progress`] for a live fraction (`done_bytes`/`total_bytes`
    /// count scanned bytes, not transferred ones, while in this state).
    Scanning,
    /// Resolving routes / fetching the manifest.
    Connecting,
    /// Chunks are moving.
    Active,
    /// User-paused; partial progress is kept.
    Paused,
    /// All bytes present (a seed is `Complete` as soon as it is snapshotted).
    Complete,
    /// Gave up; see [`TransferRow::error`].
    Failed,
}

impl TransferStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, TransferStatus::Complete | TransferStatus::Failed)
    }

    pub fn label(self) -> &'static str {
        match self {
            TransferStatus::Queued => "Queued",
            TransferStatus::Scanning => "Scanning",
            TransferStatus::Connecting => "Connecting",
            TransferStatus::Active => "Active",
            TransferStatus::Paused => "Paused",
            TransferStatus::Complete => "Complete",
            TransferStatus::Failed => "Failed",
        }
    }
}

/// How much a single source has contributed to a download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceStats {
    pub peer: PeerId,
    pub chunks: usize,
    pub bytes: u64,
}

/// One row in the share list / transfer manager.
#[derive(Debug, Clone)]
pub struct TransferRow {
    pub id: TransferId,
    /// Folder name (from the manifest).
    pub name: String,
    pub kind: TransferKind,
    pub status: TransferStatus,
    pub manifest_id: Hash,
    pub files: usize,
    pub total_bytes: u64,
    pub done_bytes: u64,
    /// Rolling throughput estimate, bytes per second.
    pub speed_bps: u64,
    /// Per-source breakdown (downloads only).
    pub sources: Vec<SourceStats>,
    /// For a seed: a dialable `/quic-v1/…/p2p/<id>` address to hand out.
    pub share_addr: Option<Multiaddr>,
    /// For a seed: every dialable address (LAN, VPN overlay, loopback),
    /// ranked best-first — what a share link actually embeds, so a
    /// subscriber on any of those networks can connect.
    pub share_addrs: Vec<Multiaddr>,
    /// For a completed download: where the files were written.
    pub output_dir: Option<PathBuf>,
    pub error: Option<String>,
    /// Manifest version currently held (download) or served (seed). Bumped by a
    /// rescan or a resync.
    pub version: u64,
    /// Seed only: `true` once the share has been made invite-only.
    pub private: bool,
    /// Seed only: the local folder this share snapshots — the source a rescan
    /// re-reads.
    pub source_dir: Option<PathBuf>,
    /// Seed only: the manifest's file paths, `/`-separated and sorted — the GUI
    /// builds the invite's file picker from this. Cheap to clone (`Arc`).
    pub file_paths: Arc<Vec<String>>,
    /// Download only: a newer manifest version seen by the last update check.
    /// Cleared by a successful resync.
    pub update_available: Option<u64>,
    /// Completed download only: `true` while this peer is serving the
    /// downloaded files back to the swarm. Toggled by the per-row
    /// seed/pause control; starts on when
    /// [`Settings::seed_after_download`](crate::Settings::seed_after_download)
    /// is set.
    pub seeding: bool,
    /// Human-readable phase text while `status` is `Connecting` — e.g.
    /// "resolving 2 source(s)…", "authenticating…", "fetching share
    /// metadata…" — so the UI can show *what* it's waiting on instead of a
    /// static label. Cleared once real transfer progress starts. Paired with
    /// a stall watchdog: if this goes stale too long the transfer fails with
    /// a clear error instead of sitting on "Connecting" forever.
    pub detail: Option<String>,
    /// Download only, while `status` is `Active`: a slow rolling-average
    /// estimate of the seconds left to completion — averaged over ~the last
    /// minute of progress (see [`EtaEstimator`](crate::EtaEstimator)) so it is
    /// steady enough to show as a countdown. `None` before there is enough
    /// history, or whenever the row is not actively downloading.
    pub eta_secs: Option<u64>,
    /// Download only: `Some(n)` when the subscription pulls a *strict subset* of
    /// `n` files chosen in the pre-download picker, `None` for a whole-share
    /// download. A partial download never seeds back (its file set hashes to a
    /// different manifest id than the origin's), so the row's seed control is
    /// hidden.
    pub selected_files: Option<usize>,
    /// Completed download only: `true` while a
    /// [`verify_share`](crate::App::verify_share) integrity pass is running.
    pub verifying: bool,
    /// Completed download only: the outcome of the last
    /// [`verify_share`](crate::App::verify_share).
    pub verify_result: Option<VerifyReport>,
}

/// Outcome of a [`verify_share`](crate::App::verify_share) integrity pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyReport {
    /// `true` when every checked file already matched the manifest and nothing
    /// had to be refetched.
    pub healthy: bool,
    /// Files whose bytes were checked against the manifest.
    pub checked_files: usize,
    /// Total size of the checked files, bytes.
    pub checked_bytes: u64,
    /// Manifest paths that failed the check and were rebuilt from the swarm.
    pub repaired: Vec<String>,
    /// Set when the pass could not finish (e.g. a damaged file but no source
    /// still serving this version). The on-disk tree is left as it was found.
    pub error: Option<String>,
}

impl TransferRow {
    /// Fraction complete in `[0.0, 1.0]`.
    pub fn progress(&self) -> f32 {
        if self.total_bytes == 0 {
            return if self.status == TransferStatus::Complete { 1.0 } else { 0.0 };
        }
        (self.done_bytes as f64 / self.total_bytes as f64).clamp(0.0, 1.0) as f32
    }
}

/// Connectivity summary for the status bar.
#[derive(Debug, Clone, Default)]
pub struct SwarmStatus {
    pub download_peer_id: Option<PeerId>,
    pub seeding: usize,
    pub downloading: usize,
}

/// Which accelerator role a machine has opted into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcceleratorRole {
    /// High-bandwidth hot-chunk cache + NAT relay / rendezvous point.
    Relay,
    /// Durable full replica of a designated share, LAN-priority serving.
    Nas,
}

impl AcceleratorRole {
    pub fn label(self) -> &'static str {
        match self {
            AcceleratorRole::Relay => "Relay",
            AcceleratorRole::Nas => "NAS replica",
        }
    }
}

/// Result of [`App::benchmark`](crate::App::benchmark) — the numbers the setup
/// wizard uses to suggest a role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BenchmarkResult {
    /// Sequential write throughput to the download directory, bytes per second.
    pub disk_write_bps: u64,
    /// Free space on the download directory's filesystem, bytes.
    pub free_bytes: u64,
    /// The role these numbers point at.
    pub suggested: AcceleratorRole,
}

/// One share an accelerator (local or remote) is carrying.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccelShareRow {
    /// Manifest id, hex.
    pub manifest_id: String,
    pub name: String,
    pub files: usize,
    pub total_bytes: u64,
    pub version: u64,
    pub private: bool,
    /// NAS role: chunks of this share on the durable replica.
    pub replica_chunks: Option<u64>,
    /// NAS role: bytes the replica occupies on disk — the *compressed* footprint
    /// when the accelerator stores it compressed.
    pub disk_bytes: Option<u64>,
    /// NAS role, local accelerator only: where the replica lives on disk.
    pub replica_path: Option<PathBuf>,
    /// NAS role: whether this share is currently served (uploaded). `false` =
    /// paused by the operator — the replica bytes stay on disk and background
    /// update checks keep running, but nothing is uploaded until re-enabled.
    pub seeding: bool,
    /// NAS role: this share's own serving address.
    pub listen_addr: Option<String>,
    /// NAS role: set while the initial replication is still under way.
    pub replicating: Option<ReplicaProgress>,
    pub error: Option<String>,
}

/// A NAS share's replication progress, reported once per chunk while it is
/// still under way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicaProgress {
    pub chunks_done: usize,
    pub chunks_total: usize,
    pub bytes_done: u64,
    pub bytes_total: u64,
}

/// Live status of an in-process accelerator this node is running.
#[derive(Debug, Clone)]
pub struct AcceleratorState {
    pub role: AcceleratorRole,
    pub peer_id: PeerId,
    /// Dialable `…/p2p/<id>` listen addresses.
    pub listen_addrs: Vec<Multiaddr>,
    /// One-line human status ("caching 3 shares from 1 upstream", …).
    pub detail: String,
    /// Relay role: hot-chunk cache occupancy and hit rate.
    pub cache: Option<CacheStats>,
    /// NAS role: chunks currently on the durable replica.
    pub replica_chunks: Option<usize>,
    /// NAS role: the folder replica chunks are stored in.
    pub replica_dir: Option<PathBuf>,
    /// NAS role: free space on the replica volume, bytes.
    pub replica_free_bytes: Option<u64>,
    /// NAS role: bytes every replica occupies on disk (sum across shares).
    pub replica_used_bytes: Option<u64>,
    /// NAS role: the configured storage ceiling, bytes (`None` = unlimited).
    /// Shares whose size would exceed it are refused.
    pub storage_cap_bytes: Option<u64>,
    /// Every share this accelerator is carrying.
    pub shares: Vec<AccelShareRow>,
}

/// Live status of a remote accelerator daemon this node manages.
#[derive(Debug, Clone)]
pub struct RemoteAccelState {
    pub label: String,
    pub admin_url: String,
    /// `true` once a signed status call has succeeded.
    pub reachable: bool,
    /// Daemon libp2p peer id, from its last status.
    pub peer_id: Option<String>,
    /// Pinned daemon identity (hex `AgentId`).
    pub daemon_key: Option<String>,
    pub role: Option<AcceleratorRole>,
    pub shares: Vec<AccelShareRow>,
    /// NAS role: the folder the daemon stores replica chunks in — editable
    /// from the GUI ([`App::remote_set_storage`](crate::App::remote_set_storage)).
    pub replica_dir: Option<String>,
    /// NAS role: free space on the replica volume, bytes.
    pub replica_free_bytes: Option<u64>,
    /// NAS role: bytes every replica occupies on disk (sum across shares).
    pub replica_used_bytes: Option<u64>,
    /// NAS role: the configured storage ceiling, bytes (`None` = unlimited).
    pub storage_cap_bytes: Option<u64>,
    /// Populated when the last poll failed.
    pub error: Option<String>,
}

/// Throughput history for the Stats tab. Cheap plain data, refreshed on the
/// manager's 2 s tick.
#[derive(Debug, Clone, Default)]
pub struct StatsSnapshot {
    /// This machine's own history: real aggregate download + upload rates.
    pub local: Vec<SpeedSample>,
    /// One row per registered remote accelerator, in the same order as
    /// [`AppState::remote_accelerators`]. `down_bps` is always `0` in these —
    /// a remote accelerator only ever serves outward.
    pub accelerators: Vec<AccelStatsRow>,
}

/// Served-throughput history for one remote accelerator, keyed by its label.
#[derive(Debug, Clone, Default)]
pub struct AccelStatsRow {
    pub label: String,
    pub history: Vec<SpeedSample>,
}

/// A public share the seeder tracker at [`Settings::rendezvous_url`] is
/// currently advertising — browsable and joinable with no share link. Refresh
/// the list with [`App::refresh_directory`](crate::App::refresh_directory);
/// join one with [`App::subscribe_discovered`](crate::App::subscribe_discovered).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredShare {
    pub manifest_id: Hash,
    pub name: String,
    /// How many peers are serving it right now (origins + replicas).
    pub seeders: usize,
}

/// A `gaggleshare1…` token just produced by
/// [`App::mint_invite`](crate::App::mint_invite), so the GUI can pick it up on
/// its next poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintedInvite {
    pub transfer: TransferId,
    pub token: String,
}

/// One file offered in the pre-download picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewFile {
    /// `/`-separated manifest path.
    pub path: String,
    pub size: u64,
}

/// A remote share's contents, fetched by
/// [`App::preview_share`](crate::App::preview_share) so the GUI can show a
/// file/destination picker before the download starts. Carries the originating
/// [`SubscribeRequest`](crate::SubscribeRequest) so the picker can complete it
/// with a file selection and destination.
#[derive(Debug, Clone)]
pub struct SharePreview {
    pub name: String,
    pub manifest_id: Hash,
    pub version: u64,
    /// Sum of every file's size — the whole-share download size.
    pub total_bytes: u64,
    /// Every file in the share, manifest order.
    pub files: Vec<PreviewFile>,
    /// Every directory in the share (so empty ones show in the tree).
    pub dirs: Vec<String>,
    /// The request this preview was fetched for, minus any selection.
    pub request: crate::SubscribeRequest,
}

/// State of the pre-download share preview — see
/// [`AppState::share_preview`].
#[derive(Debug, Clone)]
pub enum PreviewStatus {
    /// A preview fetch is in flight for a share displayed as `name`.
    Loading { name: String },
    /// The share's contents are ready to show in the picker.
    Ready(Box<SharePreview>),
    /// The preview fetch failed (no reachable source, auth rejected, …).
    Failed { name: String, error: String },
}

/// The whole observable app state. The GUI holds one of these and replaces it
/// wholesale whenever [`App`](crate::App) signals a change.
#[derive(Debug, Clone, Default)]
pub struct AppState {
    /// Rows in insertion order; use [`transfers_sorted`](Self::transfers_sorted)
    /// for a stable display order.
    pub transfers: BTreeMap<TransferId, TransferRow>,
    pub settings: Settings,
    pub swarm: SwarmStatus,
    /// The accelerator this node is running, if any.
    pub accelerator: Option<AcceleratorState>,
    /// Remote accelerator daemons this node manages, one per Settings entry.
    pub remote_accelerators: Vec<RemoteAccelState>,
    /// The most recent [`App::benchmark`](crate::App::benchmark) result.
    pub benchmark: Option<BenchmarkResult>,
    /// The token from the most recent [`App::mint_invite`](crate::App::mint_invite).
    pub minted_invite: Option<MintedInvite>,
    /// This node's operator public key (hex) — authorise it on a daemon with
    /// `accelerator authorize <key>`.
    pub operator_key: String,
    /// Rolling download / upload throughput history — see the Stats tab.
    pub stats: StatsSnapshot,
    /// Public shares the seeder tracker is advertising, from the last
    /// [`App::refresh_directory`](crate::App::refresh_directory). Empty until
    /// asked for, and when no `rendezvous_url` is set.
    pub discovered_shares: Vec<DiscoveredShare>,
    /// The pending pre-download share preview, if any — set by
    /// [`App::preview_share`](crate::App::preview_share), cleared by
    /// [`App::clear_preview`](crate::App::clear_preview).
    pub share_preview: Option<PreviewStatus>,
}

impl AppState {
    pub fn get(&self, id: TransferId) -> Option<&TransferRow> {
        self.transfers.get(&id)
    }

    /// Rows split by direction, each ordered by id.
    pub fn seeds(&self) -> impl Iterator<Item = &TransferRow> {
        self.transfers.values().filter(|t| t.kind == TransferKind::Seeding)
    }

    pub fn downloads(&self) -> impl Iterator<Item = &TransferRow> {
        self.transfers.values().filter(|t| t.kind == TransferKind::Downloading)
    }

    /// All rows, id order.
    pub fn transfers_sorted(&self) -> impl Iterator<Item = &TransferRow> {
        self.transfers.values()
    }
}
