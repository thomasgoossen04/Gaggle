//! The headless update engine: fetch the descriptor, compare versions,
//! download + verify + install the `accelerator` binary. No GUI, no
//! background thread — every call here is synchronous, called straight from
//! `main`.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};

use crate::channel::Channel;
use crate::manifest::{Asset, Manifest, platform_key};
use crate::paths;

/// An explicit descriptor-URL override: `--manifest-url`, else `$GAGGLE_UPDATE_URL`.
/// `None` means "use the selected channel's URL" ([`Channel::manifest_url`]).
pub fn url_override(cli: Option<&str>) -> Option<String> {
    cli.map(str::to_string)
        .or_else(|| std::env::var("GAGGLE_UPDATE_URL").ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// What [`decide`] made of the installed-vs-remote comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    UpToDate { version: String },
    NotInstalled { version: String },
    UpdateAvailable { version: String },
}

/// What `installed.json` records about the on-disk accelerator binary.
#[derive(Debug, Clone)]
pub struct Installed {
    pub version: String,
    pub channel: String,
}

/// Pure decision. Commit hashes aren't ordered, so *any* version difference —
/// or a channel switch — is treated as a newer/needed release. Mirrors
/// `crates/launcher/src/updater.rs::decide` exactly.
pub fn decide(installed: Option<&Installed>, remote_version: &str, remote_channel: Channel) -> Status {
    match installed {
        None => Status::NotInstalled { version: remote_version.to_string() },
        Some(i) if i.version == remote_version && i.channel == remote_channel.as_str() => {
            Status::UpToDate { version: remote_version.to_string() }
        }
        Some(_) => Status::UpdateAvailable { version: remote_version.to_string() },
    }
}

/// The `installed.json` record, if the accelerator binary has been installed
/// through this launcher.
pub fn installed_record() -> Option<Installed> {
    read_installed_record(&paths::installed_json().ok()?)
}

fn read_installed_record(path: &Path) -> Option<Installed> {
    let raw = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    Some(Installed {
        version: v.get("version")?.as_str()?.to_string(),
        channel: v.get("channel").and_then(|c| c.as_str()).unwrap_or("stable").to_string(),
    })
}

/// Fetch + parse the descriptor. Accepts `http(s)://…`, `file://<path>`, or a
/// bare local path (the last two for local testing).
pub fn fetch_manifest(url: &str) -> Result<Manifest> {
    let raw = if let Some(path) = url.strip_prefix("file://") {
        std::fs::read_to_string(path).with_context(|| format!("read {path}"))?
    } else if !url.contains("://") {
        std::fs::read_to_string(url).with_context(|| format!("read {url}"))?
    } else {
        ureq::get(url)
            .call()
            .with_context(|| format!("GET {url}"))?
            .into_string()
            .context("read manifest body")?
    };
    serde_json::from_str(&raw).context("parse manifest JSON")
}

/// Tracks a release channel (or an explicit override URL) and knows how to
/// fetch + install the accelerator binary for it.
pub struct Updater {
    url: String,
    channel: Channel,
}

impl Updater {
    /// Track a release [`Channel`] (the normal case).
    pub fn for_channel(channel: Channel) -> Self {
        Self { url: channel.manifest_url().to_string(), channel }
    }

    /// Use an explicit descriptor URL (`--manifest-url` / `$GAGGLE_UPDATE_URL`).
    /// The install is recorded under [`Channel::Stable`].
    pub fn with_url(url: String) -> Self {
        Self { url, channel: Channel::Stable }
    }

    /// The channel this updater tracks.
    pub fn channel(&self) -> Channel {
        self.channel
    }

    /// Fetch + parse the descriptor for this updater's URL.
    pub fn fetch(&self) -> Result<Manifest> {
        fetch_manifest(&self.url)
    }

    /// Download + verify + install the accelerator binary for the host
    /// platform, and record it in `installed.json`. Returns the installed
    /// version string.
    pub fn install_blocking(&self) -> Result<String> {
        let m = self.fetch()?;
        let asset = m
            .asset_for_host()
            .ok_or_else(|| anyhow!("no accelerator build for this platform ({})", platform_key()))?
            .clone();

        let tmp = download_verify(&asset)?;
        install_binary(&tmp, &m.version, self.channel)?;
        Ok(m.version)
    }
}

/// Stream `asset.url` to a temp file *inside the install directory* (so the
/// final rename in [`install_binary`] is same-filesystem and so atomic),
/// hashing as it goes; verify the SHA-256 and set the exec bit.
fn download_verify(asset: &Asset) -> Result<PathBuf> {
    let resp = ureq::get(&asset.url).call().with_context(|| format!("GET {}", asset.url))?;

    let dir = paths::install_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let tmp = dir.join(format!(".gaggle-accelerator-download-{}", std::process::id()));
    let mut file =
        std::fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;

    let mut reader = resp.into_reader();
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf).context("read response body")?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).context("write download")?;
        hasher.update(&buf[..n]);
    }
    file.flush().ok();
    drop(file);

    if let Err(e) = verify_sha(&asset.sha256, &hex(&hasher.finalize())) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
            .with_context(|| format!("chmod {}", tmp.display()))?;
    }
    Ok(tmp)
}

/// Compare a hex SHA-256 against what was computed. An empty `expected` skips
/// the check (descriptor opted out).
fn verify_sha(expected: &str, got: &str) -> Result<()> {
    if !expected.is_empty() && !got.eq_ignore_ascii_case(expected) {
        bail!("sha256 mismatch: expected {expected}, got {got}");
    }
    Ok(())
}

/// Move `tmp` into place at [`paths::accelerator_binary`] and record it in
/// [`paths::installed_json`].
fn install_binary(tmp: &Path, version: &str, channel: Channel) -> Result<()> {
    replace_binary(tmp, &paths::accelerator_binary()?)?;
    write_installed_json(&paths::installed_json()?, version, channel)
}

/// Move `tmp` into place at `dest`. A pre-existing binary at `dest` is moved
/// aside to `*.old` first (rather than overwritten in place) so this never
/// races a still-exiting previous instance of the daemon; leftovers are swept
/// on the next install.
fn replace_binary(tmp: &Path, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    if dest.exists() {
        let old = dest.with_extension("old");
        let _ = std::fs::remove_file(&old);
        let _ = std::fs::rename(dest, &old);
    }
    std::fs::rename(tmp, dest).with_context(|| format!("install {}", dest.display()))?;
    Ok(())
}

fn write_installed_json(path: &Path, version: &str, channel: Channel) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let body = serde_json::json!({
        "version": version,
        "channel": channel.as_str(),
        "installed_at": secs,
    });
    std::fs::write(path, serde_json::to_vec_pretty(&body)?)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decide_compares_version_and_channel() {
        let rec = |v: &str, c: &str| Installed { version: v.to_string(), channel: c.to_string() };
        assert!(matches!(
            decide(None, "2.0.abc1234", Channel::Stable),
            Status::NotInstalled { .. }
        ));
        assert!(matches!(
            decide(Some(&rec("2.0.abc1234", "stable")), "2.0.abc1234", Channel::Stable),
            Status::UpToDate { .. }
        ));
        assert!(matches!(
            decide(Some(&rec("2.0.abc1234", "stable")), "2.0.def5678", Channel::Stable),
            Status::UpdateAvailable { .. }
        ));
        // Same version string but a different channel ⇒ a channel switch is an update.
        assert!(matches!(
            decide(Some(&rec("2.0.abc1234", "stable")), "2.0.abc1234", Channel::Beta),
            Status::UpdateAvailable { .. }
        ));
    }

    #[test]
    fn url_override_reads_cli_then_env() {
        // SAFETY: single-threaded test; no other code reads this var here.
        unsafe { std::env::remove_var("GAGGLE_UPDATE_URL") };
        assert_eq!(url_override(None), None);
        assert_eq!(url_override(Some("http://x/y.json")), Some("http://x/y.json".to_string()));
    }

    #[test]
    fn hex_pads() {
        assert_eq!(hex(&[0x00, 0x0f, 0xff]), "000fff");
    }

    #[test]
    fn verify_sha_matches_case_insensitively_and_skips_when_empty() {
        assert!(verify_sha("", "anything").is_ok());
        assert!(verify_sha("ABCD", "abcd").is_ok());
        assert!(verify_sha("abcd", "ef01").is_err());
    }

    #[test]
    fn replace_binary_moves_a_previous_install_aside_instead_of_overwriting() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("bin").join("accelerator");

        let src = tmp.path().join("downloaded");
        std::fs::write(&src, b"binary-v1").unwrap();
        replace_binary(&src, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"binary-v1");
        assert!(!src.exists(), "the temp file was moved, not copied");

        // A second install moves the first binary aside instead of failing.
        let src2 = tmp.path().join("downloaded2");
        std::fs::write(&src2, b"binary-v2").unwrap();
        replace_binary(&src2, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"binary-v2");
        assert!(dest.with_extension("old").exists());
    }

    #[test]
    fn installed_json_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested").join("installed.json");

        write_installed_json(&path, "2.0.aaa1111", Channel::Beta).unwrap();
        let rec = read_installed_record(&path).unwrap();
        assert_eq!(rec.version, "2.0.aaa1111");
        assert_eq!(rec.channel, "beta");
    }
}
