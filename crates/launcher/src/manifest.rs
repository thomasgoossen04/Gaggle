//! The `latest.json` release descriptor the launcher fetches from a URL.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Top-level descriptor. `version` is the release's `2.0.<commit>` string; any
/// difference from the installed version means "update available" (commit
/// hashes have no ordering, so this is a string compare, not a `>` check).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    /// `"stable"` or `"beta"` — informational; the launcher already knows which
    /// channel URL it fetched.
    #[serde(default)]
    pub channel: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub pub_date: String,
    pub platforms: BTreeMap<String, Asset>,
}

/// One platform's downloadable archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    pub url: String,
    /// Lowercase hex SHA-256 of the archive. Empty disables the check.
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub size: u64,
}

impl Manifest {
    /// The asset for the host platform, if the descriptor carries one.
    pub fn asset_for_host(&self) -> Option<&Asset> {
        self.platforms.get(platform_key())
    }
}

/// `platforms` map key for the current build: `linux-x86_64`, `windows-x86_64`,
/// `macos-aarch64`, `macos-x86_64`, or `unsupported`.
pub fn platform_key() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "linux-x86_64",
        ("windows", "x86_64") => "windows-x86_64",
        ("macos", "aarch64") => "macos-aarch64",
        ("macos", "x86_64") => "macos-x86_64",
        _ => "unsupported",
    }
}
