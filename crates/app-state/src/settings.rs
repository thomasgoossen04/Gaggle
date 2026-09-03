//! User-tunable settings — the "Settings" view's model. Plain data plus
//! optional JSON persistence; no UI framework in sight.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Which colour scheme the GUI should use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

impl Theme {
    pub const ALL: [Theme; 3] = [Theme::System, Theme::Light, Theme::Dark];

    pub fn label(self) -> &'static str {
        match self {
            Theme::System => "System",
            Theme::Light => "Light",
            Theme::Dark => "Dark",
        }
    }
}

/// Everything the Settings view edits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Where completed downloads are written (one sub-folder per share).
    pub download_dir: PathBuf,
    /// Download / upload bandwidth ceilings in bytes per second (`None` = no cap).
    pub download_cap_bps: Option<u64>,
    pub upload_cap_bps: Option<u64>,
    /// Storage ceiling for cache-accelerator mode, in bytes (`None` = no cap).
    pub storage_cap_bytes: Option<u64>,
    pub theme: Theme,
    /// If set, subscribed shares are polled this often (seconds) for a newer
    /// manifest version. A newer version is only *flagged* — never applied
    /// without an explicit resync. `None` disables the background poll.
    #[serde(default)]
    pub auto_resync_secs: Option<u64>,
    /// Remote accelerator daemons this node manages over their admin API.
    #[serde(default)]
    pub remote_accelerators: Vec<RemoteAccelerator>,
}

/// A remote accelerator daemon registered in Settings — its admin URL and the
/// daemon identity pinned on first contact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteAccelerator {
    /// A short human label, unique within the list.
    pub label: String,
    /// Base URL of the daemon's admin API, e.g. `http://host:8749`.
    pub admin_url: String,
    /// Hex `AgentId` of the daemon, learned + pinned on the first successful
    /// call. `None` until then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon_key: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            download_dir: default_download_dir(),
            download_cap_bps: None,
            upload_cap_bps: None,
            storage_cap_bytes: None,
            theme: Theme::System,
            auto_resync_secs: None,
            remote_accelerators: Vec::new(),
        }
    }
}

impl Settings {
    /// Load from `path`; missing file → [`Settings::default`]. A malformed file
    /// is an error (the caller decides whether to fall back).
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Write to `path` (pretty JSON), creating parent directories.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }
}

fn default_download_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join("Downloads").join("Gaggle")
    } else {
        std::env::temp_dir().join("gaggle-downloads")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json() {
        let s = Settings {
            download_cap_bps: Some(2_000_000),
            theme: Theme::Dark,
            ..Settings::default()
        };
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Settings>(&json).unwrap(), s);
    }

    #[test]
    fn missing_file_is_defaults_but_bad_file_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.json");
        assert_eq!(Settings::load(&path).unwrap(), Settings::default());

        std::fs::write(&path, b"{not json").unwrap();
        assert!(Settings::load(&path).is_err());
    }

    #[test]
    fn save_then_load_preserves_edits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfg/settings.json");
        let s = Settings { storage_cap_bytes: Some(50 << 30), ..Settings::default() };
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), s);
    }
}
