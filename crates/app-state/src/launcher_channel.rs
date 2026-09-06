//! The desktop launcher's release-channel selection, shared with the GUI.
//!
//! `crates/launcher` persists which release stream it tracks — `stable` or
//! `beta` — in `<data-dir>/Gaggle/launcher.json`, and reads it on every update
//! check. The `gui` crate can't depend on the launcher binary, so this module
//! re-implements the minimal read/write of that one file. It lets Settings
//! expose a channel switch whose effect lands the next time the launcher runs
//! (mirrors `launcher::channel`, deliberately kept byte-compatible with it).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Which release stream the launcher tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LauncherChannel {
    /// Non-prerelease `main` builds. The default.
    #[default]
    Stable,
    /// Pre-release `beta` builds — newer, less tested.
    Beta,
}

impl LauncherChannel {
    /// The lowercase token written into `launcher.json` — matches
    /// `launcher::channel::Channel::as_str`.
    pub fn as_str(self) -> &'static str {
        match self {
            LauncherChannel::Stable => "stable",
            LauncherChannel::Beta => "beta",
        }
    }

    /// Title-case label for the UI.
    pub fn label(self) -> &'static str {
        match self {
            LauncherChannel::Stable => "Stable",
            LauncherChannel::Beta => "Beta",
        }
    }

    /// Parse the persisted token, treating the empty string as `Stable`
    /// (exactly `launcher::channel::Channel::parse`).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "stable" => Some(LauncherChannel::Stable),
            "beta" => Some(LauncherChannel::Beta),
            _ => None,
        }
    }

    /// The two channels, for rendering a picker.
    pub const ALL: [LauncherChannel; 2] = [LauncherChannel::Stable, LauncherChannel::Beta];
}

/// `<data-dir>/Gaggle/launcher.json` — the same path
/// `launcher::paths::launcher_json` resolves to (`dirs::data_dir()` + `Gaggle`).
/// `None` only when the OS exposes no data directory.
pub fn default_path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("Gaggle").join("launcher.json"))
}

/// Read the persisted channel, falling back to [`LauncherChannel::Stable`] when
/// the file is missing, unreadable, or unparseable — the launcher's own
/// behaviour when it can't read its config.
pub fn read(path: &Path) -> LauncherChannel {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|v| v.get("channel").and_then(|c| c.as_str()).map(str::to_string))
        .and_then(|s| LauncherChannel::parse(&s))
        .unwrap_or_default()
}

/// Write `channel` into `launcher.json`, preserving any other keys already in
/// the file. The launcher only stores `channel` today, but a read-modify-write
/// keeps this from clobbering anything it adds later.
pub fn write(path: &Path, channel: LauncherChannel) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut obj = std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| {
            serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&raw).ok()
        })
        .unwrap_or_default();
    obj.insert("channel".into(), channel.as_str().into());
    let body = serde_json::to_vec_pretty(&serde_json::Value::Object(obj))?;
    std::fs::write(path, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_matches_launcher_semantics() {
        assert_eq!(LauncherChannel::parse("BETA"), Some(LauncherChannel::Beta));
        assert_eq!(LauncherChannel::parse("  stable "), Some(LauncherChannel::Stable));
        assert_eq!(LauncherChannel::parse(""), Some(LauncherChannel::Stable));
        assert_eq!(LauncherChannel::parse("nightly"), None);
    }

    #[test]
    fn missing_file_reads_as_stable() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read(&dir.path().join("launcher.json")), LauncherChannel::Stable);
    }

    #[test]
    fn write_then_read_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/launcher.json");
        write(&path, LauncherChannel::Beta).unwrap();
        assert_eq!(read(&path), LauncherChannel::Beta);
        write(&path, LauncherChannel::Stable).unwrap();
        assert_eq!(read(&path), LauncherChannel::Stable);
    }

    #[test]
    fn write_preserves_other_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("launcher.json");
        std::fs::write(&path, r#"{"channel":"stable","desktop_shortcut":true}"#).unwrap();
        write(&path, LauncherChannel::Beta).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["channel"], "beta");
        assert_eq!(v["desktop_shortcut"], true);
    }

    #[test]
    fn reads_the_launchers_own_format() {
        // Exactly what `launcher::channel::save` writes.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("launcher.json");
        let body = serde_json::to_vec_pretty(&serde_json::json!({ "channel": "beta" })).unwrap();
        std::fs::write(&path, body).unwrap();
        assert_eq!(read(&path), LauncherChannel::Beta);
    }
}
