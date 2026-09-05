//! The subset of the shared `latest.json` release descriptor this launcher
//! cares about: its `accelerator` platform map. `latest.json` is the *same*
//! descriptor the desktop `gaggle-launcher` fetches (see
//! `.github/scripts/make_latest.py`) — its own `platforms` map (the GUI+
//! launcher zip) is simply ignored here (serde skips unknown fields by
//! default), and this crate's `accelerator` map is likewise ignored by the
//! desktop launcher's `Manifest` type. One descriptor, two independent
//! consumers.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Top-level descriptor. `version` is the release's `2.0.<commit>` string; any
/// difference from the installed version means "update available" (commit
/// hashes have no ordering, so this is a string compare, not a `>` check).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    /// `"stable"` or `"beta"` — informational; this launcher already knows
    /// which channel URL it fetched.
    #[serde(default)]
    pub channel: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub pub_date: String,
    /// The standalone `gaggle-accelerator-<platform>` binaries — a plain
    /// executable per platform, not a zip (nothing to extract).
    #[serde(default)]
    pub accelerator: BTreeMap<String, Asset>,
}

/// One platform's downloadable binary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    pub url: String,
    /// Lowercase hex SHA-256 of the binary. Empty disables the check.
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub size: u64,
}

impl Manifest {
    /// The asset for the host platform, if the descriptor carries one.
    pub fn asset_for_host(&self) -> Option<&Asset> {
        self.accelerator.get(platform_key())
    }
}

/// `accelerator` map key for the current build: `linux-x86_64`, `windows-x86_64`,
/// `macos-aarch64`, `macos-x86_64`, or `unsupported`. Identical scheme to the
/// desktop launcher's `platforms` map keys.
pub fn platform_key() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "linux-x86_64",
        ("windows", "x86_64") => "windows-x86_64",
        ("macos", "aarch64") => "macos-aarch64",
        ("macos", "x86_64") => "macos-x86_64",
        _ => "unsupported",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_json_round_trips_and_ignores_the_gui_platforms_map() {
        let src = r#"{
          "version": "2.0.deadbee",
          "channel": "beta",
          "notes": "hi",
          "pub_date": "2026-09-04T00:00:00Z",
          "platforms": {
            "linux-x86_64": { "url": "https://e/gaggle-linux-x86_64.zip", "sha256": "aa", "size": 5 }
          },
          "accelerator": {
            "linux-x86_64": { "url": "https://e/gaggle-accelerator-linux-x86_64", "sha256": "ab", "size": 6 }
          }
        }"#;
        let m: Manifest = serde_json::from_str(src).unwrap();
        assert_eq!(m.version, "2.0.deadbee");
        assert_eq!(m.channel, "beta");
        assert_eq!(m.accelerator.len(), 1);
        let back = serde_json::to_string(&m).unwrap();
        assert_eq!(serde_json::from_str::<Manifest>(&back).unwrap(), m);
    }

    #[test]
    fn a_descriptor_with_no_accelerator_map_yet_still_parses() {
        let src = r#"{"version": "2.0.deadbee", "platforms": {}}"#;
        let m: Manifest = serde_json::from_str(src).unwrap();
        assert!(m.accelerator.is_empty());
    }
}
