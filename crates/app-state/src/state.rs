//! The snapshot the GUI renders. Cloneable plain data — a view takes a copy and
//! diffs against it; nothing here knows about gpui.

use std::collections::BTreeMap;
use std::path::PathBuf;

use gaggle_core::Hash;
use net::{Multiaddr, PeerId};

use crate::settings::Settings;

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
    /// For a completed download: where the files were written.
    pub output_dir: Option<PathBuf>,
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

/// The whole observable app state. The GUI holds one of these and replaces it
/// wholesale whenever [`App`](crate::App) signals a change.
#[derive(Debug, Clone, Default)]
pub struct AppState {
    /// Rows in insertion order; use [`transfers_sorted`](Self::transfers_sorted)
    /// for a stable display order.
    pub transfers: BTreeMap<TransferId, TransferRow>,
    pub settings: Settings,
    pub swarm: SwarmStatus,
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
