//! Release-channel selection: `stable` (non-prerelease `main` builds, the
//! default) vs `beta` (pre-release builds cut from the `beta` branch, for
//! testing). The choice persists in `launcher.json` and picks which descriptor
//! URL the [`crate::updater`] fetches.

use anyhow::{Context, Result};

use crate::paths;

/// Which release stream the launcher tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum Channel {
    /// Non-prerelease `main` builds. The default.
    #[default]
    Stable,
    /// Pre-release `beta` builds — for testing new versions.
    Beta,
}

impl Channel {
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Beta => "beta",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "stable" => Some(Channel::Stable),
            "beta" => Some(Channel::Beta),
            _ => None,
        }
    }

    /// The release descriptor URL for this channel. Stable rides GitHub's
    /// "latest" pointer (prereleases are excluded from it); beta rides the
    /// rolling `beta` tag the CI overwrites on every `beta` push.
    pub const fn manifest_url(self) -> &'static str {
        match self {
            Channel::Stable => {
                "https://github.com/thomasgoossen04/Gaggle/releases/latest/download/latest.json"
            }
            Channel::Beta => {
                "https://github.com/thomasgoossen04/Gaggle/releases/download/beta/latest.json"
            }
        }
    }
}

/// The persisted channel (`launcher.json`), defaulting to [`Channel::Stable`].
pub fn load() -> Channel {
    let Ok(path) = paths::launcher_json() else {
        return Channel::Stable;
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|v| v.get("channel").and_then(|c| c.as_str()).map(str::to_string))
        .and_then(|s| Channel::parse(&s))
        .unwrap_or(Channel::Stable)
}

/// Persist the chosen channel to `launcher.json`.
pub fn save(channel: Channel) -> Result<()> {
    let path = paths::launcher_json()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::json!({ "channel": channel.as_str() });
    std::fs::write(&path, serde_json::to_vec_pretty(&body)?)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_known_names_only() {
        assert_eq!(Channel::parse("BETA"), Some(Channel::Beta));
        assert_eq!(Channel::parse("  stable "), Some(Channel::Stable));
        assert_eq!(Channel::parse(""), Some(Channel::Stable));
        assert_eq!(Channel::parse("nightly"), None);
    }

    #[test]
    fn channel_urls_differ() {
        assert!(Channel::Stable.manifest_url().contains("/releases/latest/download/"));
        assert!(Channel::Beta.manifest_url().contains("/releases/download/beta/"));
        assert_ne!(Channel::Stable.manifest_url(), Channel::Beta.manifest_url());
    }
}
