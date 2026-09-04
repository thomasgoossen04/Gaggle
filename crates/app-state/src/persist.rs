//! On-disk record of the share/transfer list, restored by [`crate::App::new`]
//! on the next start when [`crate::Settings::persist_shares`] is enabled (the
//! default). Lives next to `settings.json` as `shares.json` — see
//! `Manager::shares_path`.
//!
//! Deliberately thin: a seed is restored by re-running the same scan it
//! started with (its source folder, private-share signing seed, and current
//! version — so a rescanned share reproduces the same manifest id it had
//! before restart), and a download by re-issuing the same
//! [`SubscribeRequest`] it was created from, which naturally resumes from
//! whatever partial chunks already made it to disk.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::manager::SubscribeRequest;

/// Everything needed to recreate the share/transfer list on the next start.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct PersistedState {
    pub seeds: Vec<PersistedSeed>,
    pub subscriptions: Vec<SubscribeRequest>,
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
