//! On-disk record of the share/transfer list, restored by [`crate::App::new`]
//! on the next start when [`crate::Settings::persist_shares`] is enabled (the
//! default). Lives next to `settings.json` as `shares.json` — see
//! `Manager::shares_path`.
//!
//! Deliberately thin: a seed is restored by re-running the same scan it
//! started with (its source folder, private-share signing seed, and current
//! version — so a rescanned share reproduces the same manifest id it had
//! before restart), and an *unfinished* download by re-issuing the same
//! [`SubscribeRequest`] it was created from, which naturally resumes from
//! whatever partial chunks already made it to disk.
//!
//! A *finished* download is restored differently ([`PersistedSub`]): its
//! output tree is re-indexed offline and it comes straight back as a
//! `Complete`, already-seeding transfer with no re-download — re-running the
//! swarm download just to rediscover "we already have everything" showed an
//! empty progress bar and a "Connecting" status on every restart (and outright
//! failed when the origin was offline).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::manager::SubscribeRequest;

/// Everything needed to recreate the share/transfer list on the next start.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct PersistedState {
    pub seeds: Vec<PersistedSeed>,
    /// Downloads that had *not* finished — re-issued as fresh
    /// [`SubscribeRequest`]s so they resume from their partial chunks.
    pub subscriptions: Vec<SubscribeRequest>,
    /// Downloads that *had* finished — restored as `Complete` + seeding with no
    /// re-download. Old files without the key keep any finished downloads in
    /// `subscriptions`; they migrate here on the next save.
    pub completed_subscriptions: Vec<PersistedSub>,
    /// Manifest-id hexes of completed downloads the user paused seeding for.
    /// A re-completed subscription checks this before auto-seeding again, so a
    /// pause survives a restart. Old files without the key load as empty.
    pub paused_seeds: Vec<String>,
}

/// One finished download from a previous session. Restored by re-indexing the
/// materialized output tree (no network): the re-index reproduces the same
/// manifest id — hence the pinned `version` — and rebuilds the chunk lists the
/// seed, verify and resync paths need.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PersistedSub {
    pub request: SubscribeRequest,
    /// Where the files were written — carried explicitly because a sanitized
    /// share name means the folder name can differ from the manifest name.
    pub output_dir: PathBuf,
    /// The origin manifest's `name`, needed to re-index the tree into the same
    /// manifest id. Not always the same as the link / request name (which the
    /// origin need not have derived from its folder name).
    #[serde(default)]
    pub name: String,
    #[serde(default = "one")]
    pub version: u64,
}

/// One local folder this node was seeding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PersistedSeed {
    pub dir: PathBuf,
    /// `Some` for a private share — the per-share signing seed, so the
    /// restored share keeps the same identity and already-minted invites
    /// keep working.
    #[serde(default)]
    pub share_seed: Option<[u8; 32]>,
    #[serde(default = "one")]
    pub version: u64,
}

fn one() -> u64 {
    1
}
