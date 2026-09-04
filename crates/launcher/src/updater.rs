//! The headless update engine: fetch the descriptor, compare versions, download
//! and verify and install the GUI, then launch it. The gpui view in
//! [`crate::ui`] only reads [`Updater::state`] and calls the trigger methods
//! (`check`, `install`, `launch_now`); the first two spawn a `std::thread`.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};

use crate::channel::Channel;
use crate::desktop;
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

/// What the launcher is doing / found. Cheap to clone; the view renders from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Idle,
    Checking,
    UpToDate { version: String },
    NotInstalled { version: String },
    UpdateAvailable { version: String, notes: String },
    Downloading { done: u64, total: u64 },
    Verifying,
    Installing,
    Ready { version: String },
    Launching,
    Error(String),
}

impl Status {
    /// A one-line monospace summary for the window.
    pub fn line(&self) -> String {
        match self {
            Status::Idle => "Ready".into(),
            Status::Checking => "Checking for updates…".into(),
            Status::UpToDate { version } => format!("Up to date — {version}"),
            Status::NotInstalled { version } => format!("Not installed — {version} available"),
            Status::UpdateAvailable { version, .. } => format!("Update available — {version}"),
            Status::Downloading { done, total } => {
                if *total > 0 {
                    format!(
                        "Downloading… {:.0}% ({} / {} MiB)",
                        (*done as f64 / *total as f64) * 100.0,
                        done / (1 << 20),
                        total / (1 << 20)
                    )
                } else {
                    format!("Downloading… {} MiB", done / (1 << 20))
                }
            }
            Status::Verifying => "Verifying…".into(),
            Status::Installing => "Installing…".into(),
            Status::Ready { version } => format!("Installed {version} — ready to launch"),
            Status::Launching => "Launching Gaggle…".into(),
            Status::Error(e) => format!("Error: {e}"),
        }
    }

    /// Download fraction while [`Status::Downloading`], else `None`.
    pub fn download_frac(&self) -> Option<f32> {
        match self {
            Status::Downloading { done, total } if *total > 0 => {
                Some((*done as f32 / *total as f32).clamp(0.0, 1.0))
            }
            _ => None,
        }
    }
}

/// What `installed.json` records about the on-disk GUI.
#[derive(Debug, Clone)]
pub struct Installed {
    pub version: String,
    pub channel: String,
}

/// Pure decision. Commit hashes aren't ordered, so *any* version difference — or
/// a channel switch — is treated as a newer/needed release.
pub fn decide(
    installed: Option<&Installed>,
    remote_version: &str,
    remote_channel: Channel,
) -> Status {
    match installed {
        None => Status::NotInstalled {
            version: remote_version.to_string(),
        },
        Some(i) if i.version == remote_version && i.channel == remote_channel.as_str() => {
            Status::UpToDate {
                version: remote_version.to_string(),
            }
        }
        Some(_) => Status::UpdateAvailable {
            version: remote_version.to_string(),
            notes: String::new(),
        },
    }
}

/// The `installed.json` record, if the GUI has been installed.
pub fn installed_record() -> Option<Installed> {
    let raw = std::fs::read_to_string(paths::installed_json().ok()?).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    Some(Installed {
        version: v.get("version")?.as_str()?.to_string(),
        channel: v
            .get("channel")
            .and_then(|c| c.as_str())
            .unwrap_or("stable")
            .to_string(),
    })
}

/// Fetch + parse the descriptor. Accepts `http(s)://…`, `file://<path>`, or a
/// bare local path (the last two for local testing).
pub fn fetch_manifest(url: &str) -> Result<Manifest> {
    fetch_manifest_with_timeout(url, None)
}

/// Like [`fetch_manifest`], but with an optional request timeout — used for
/// the pre-launch "should we auto-launch?" probe so a slow/dead network can't
/// hang what should feel like an instant open.
fn fetch_manifest_with_timeout(url: &str, timeout: Option<Duration>) -> Result<Manifest> {
    let raw = if let Some(path) = url.strip_prefix("file://") {
        std::fs::read_to_string(path).with_context(|| format!("read {path}"))?
    } else if !url.contains("://") {
        std::fs::read_to_string(url).with_context(|| format!("read {url}"))?
    } else {
        let agent = match timeout {
            Some(t) => ureq::AgentBuilder::new().timeout(t).build(),
            None => ureq::agent(),
        };
        agent
            .get(url)
            .call()
            .with_context(|| format!("GET {url}"))?
            .into_string()
            .context("read manifest body")?
    };
    serde_json::from_str(&raw).context("parse manifest JSON")
}

/// A thread-safe handle to the update state machine.
#[derive(Clone)]
pub struct Updater {
    state: Arc<Mutex<Status>>,
    url: String,
    channel: Channel,
    /// The user's "create desktop shortcut" opt-in, read by `install_blocking`.
    desktop_shortcut: Arc<AtomicBool>,
}

impl Updater {
    /// Track a release [`Channel`] (the normal case).
    pub fn for_channel(channel: Channel) -> Self {
        Self {
            state: Arc::new(Mutex::new(Status::Idle)),
            url: channel.manifest_url().to_string(),
            channel,
            desktop_shortcut: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Use an explicit descriptor URL (`--manifest-url` / `$GAGGLE_UPDATE_URL`).
    /// The install is recorded under [`Channel::Stable`].
    pub fn with_url(url: String) -> Self {
        Self {
            state: Arc::new(Mutex::new(Status::Idle)),
            url,
            channel: Channel::Stable,
            desktop_shortcut: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The channel this updater tracks.
    pub fn channel(&self) -> Channel {
        self.channel
    }

    /// Opt in (or out) of creating a desktop shortcut on the next install.
    /// The apps-menu / Start Menu entry is always created regardless.
    pub fn set_desktop_shortcut(&self, wanted: bool) {
        self.desktop_shortcut.store(wanted, Ordering::Relaxed);
    }

    /// Fetch + parse the descriptor for this updater's URL.
    pub fn fetch(&self) -> Result<Manifest> {
        fetch_manifest(&self.url)
    }

    /// Like [`Updater::fetch`], but bounded to a few seconds — used for the
    /// pre-launch "is this still current?" probe, which should never make a
    /// dead network stall opening the app.
    pub fn fetch_quick(&self) -> Result<Manifest> {
        fetch_manifest_with_timeout(&self.url, Some(Duration::from_secs(4)))
    }

    /// Current status (cloned).
    pub fn state(&self) -> Status {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn set(&self, s: Status) {
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = s;
    }

    /// Fetch the descriptor and compare against the installed version + channel.
    pub fn check(&self) {
        let this = self.clone();
        thread::spawn(move || {
            this.set(Status::Checking);
            match fetch_manifest(&this.url) {
                Ok(m) => {
                    let mut st = decide(installed_record().as_ref(), &m.version, this.channel);
                    if let Status::UpdateAvailable { notes, .. } = &mut st {
                        *notes = m.notes.clone();
                    }
                    this.set(st);
                }
                Err(e) => this.set(Status::Error(e.to_string())),
            }
        });
    }

    /// Download + verify + install the host build, then mark [`Status::Ready`].
    pub fn install(&self) {
        let this = self.clone();
        thread::spawn(move || match this.install_blocking() {
            Ok(version) => this.set(Status::Ready { version }),
            Err(e) => this.set(Status::Error(e.to_string())),
        });
    }

    /// Synchronous install path (used by `install` and by `gaggle-launcher
    /// update`). Returns the installed version string.
    pub fn install_blocking(&self) -> Result<String> {
        let m = fetch_manifest(&self.url)?;
        let asset = m
            .asset_for_host()
            .ok_or_else(|| anyhow!("no build for this platform ({})", platform_key()))?
            .clone();

        let archive = self.download_verify(&asset)?;
        self.set(Status::Installing);
        let res = install_archive(&archive, &m.version, self.channel);
        let _ = std::fs::remove_file(&archive);
        res?;

        let opts = desktop::Shortcuts {
            desktop: self.desktop_shortcut.load(Ordering::Relaxed),
        };
        if let Ok(bin) = paths::installed_launcher()
            && let Err(e) = desktop::install(&bin, opts)
        {
            eprintln!("warning: could not create shortcuts: {e}");
        }

        Ok(m.version)
    }

    fn download_verify(&self, asset: &Asset) -> Result<PathBuf> {
        self.set(Status::Downloading { done: 0, total: 0 });
        download_to_temp(asset, &|done, total| {
            self.set(Status::Downloading { done, total })
        })
        .inspect(|_| self.set(Status::Verifying))
    }

    /// Spawn the installed GUI on the calling thread and mark
    /// [`Status::Launching`]. Synchronous so the window can close itself only
    /// once the child has actually started.
    pub fn launch_now(&self) -> Result<()> {
        self.set(Status::Launching);
        launch_gui()
    }
}

/// Spawn the installed GUI with no window/status bookkeeping — the silent
/// hand-off path `main.rs` takes when there's nothing for the launcher to show.
pub fn launch_installed() -> Result<()> {
    launch_gui()
}

/// Should `gaggle-launcher run` skip its window and go straight to the GUI?
///
/// Pure and unit-tested: `fetched` is `None` for "the update check failed"
/// (treated as offline — launch whatever's on disk) and `Some(status)` for
/// "the check succeeded, and here's what `decide` made of it".
pub fn wants_auto_launch(
    installed_present: bool,
    gui_present: bool,
    fetched: Option<Status>,
) -> bool {
    if !installed_present || !gui_present {
        return false;
    }
    match fetched {
        None => true,
        Some(Status::UpToDate { .. }) => true,
        Some(_) => false,
    }
}

/// Stream `asset.url` to a temp file, hashing as it goes; verify the SHA-256.
fn download_to_temp(asset: &Asset, progress: &dyn Fn(u64, u64)) -> Result<PathBuf> {
    let resp = ureq::get(&asset.url)
        .call()
        .with_context(|| format!("GET {}", asset.url))?;
    let total: u64 = resp
        .header("Content-Length")
        .and_then(|s| s.parse().ok())
        .unwrap_or(asset.size);

    let path = std::env::temp_dir().join(format!("gaggle-dl-{}.zip", std::process::id()));
    let mut file =
        std::fs::File::create(&path).with_context(|| format!("create {}", path.display()))?;
    let mut reader = resp.into_reader();
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut done = 0u64;
    loop {
        let n = reader.read(&mut buf).context("read response body")?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).context("write download")?;
        hasher.update(&buf[..n]);
        done += n as u64;
        progress(done, total);
    }
    file.flush().ok();

    verify_sha(&asset.sha256, &hex(&hasher.finalize())).inspect_err(|_| {
        let _ = std::fs::remove_file(&path);
    })?;
    Ok(path)
}

/// Compare a hex SHA-256 against what was computed. An empty `expected` skips
/// the check (descriptor opted out).
fn verify_sha(expected: &str, got: &str) -> Result<()> {
    if !expected.is_empty() && !got.eq_ignore_ascii_case(expected) {
        bail!("sha256 mismatch: expected {expected}, got {got}");
    }
    Ok(())
}

/// Install `zip_path` into [`paths::install_dir`] and record version + channel.
fn install_archive(zip_path: &Path, version: &str, channel: Channel) -> Result<()> {
    let dir = paths::install_dir()?;
    let wrote = extract_into(zip_path, &dir)?;
    if wrote == 0 {
        bail!("archive contained no files");
    }
    write_installed_json(version, channel)?;
    Ok(())
}

/// Extract every file in `zip_path` into `dir`, flattening any directory
/// structure. A pre-existing target is moved aside to `*.old` first, so
/// replacing a still-running `gaggle-gui` succeeds; leftovers are swept on the
/// next install. Returns the number of files written.
fn extract_into(zip_path: &Path, dir: &Path) -> Result<usize> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    sweep_old(dir);

    let file = std::fs::File::open(zip_path)?;
    let mut zip = zip::ZipArchive::new(file).context("open zip")?;
    let mut wrote = 0usize;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        if entry.is_dir() {
            continue;
        }
        let Some(name) = entry
            .enclosed_name()
            .and_then(|p| p.file_name().map(|f| f.to_owned()))
        else {
            continue;
        };
        let dest = dir.join(&name);
        if dest.exists() {
            let old = dest.with_extension("old");
            let _ = std::fs::remove_file(&old);
            let _ = std::fs::rename(&dest, &old);
        }
        let mut out =
            std::fs::File::create(&dest).with_context(|| format!("create {}", dest.display()))?;
        std::io::copy(&mut entry, &mut out)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))?;
        }
        wrote += 1;
    }
    Ok(wrote)
}

fn sweep_old(dir: &Path) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.path().extension().is_some_and(|x| x == "old") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

fn write_installed_json(version: &str, channel: Channel) -> Result<()> {
    let path = paths::installed_json()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let body = serde_json::json!({
        "version": version,
        "channel": channel.as_str(),
        "installed_at": secs,
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&body)?)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn launch_gui() -> Result<()> {
    let bin = paths::gui_binary()?;
    if !bin.exists() {
        bail!("{} is not installed", bin.display());
    }
    std::process::Command::new(&bin)
        .spawn()
        .with_context(|| format!("spawn {}", bin.display()))?;
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
        let rec = |v: &str, c: &str| Installed {
            version: v.to_string(),
            channel: c.to_string(),
        };
        assert!(matches!(
            decide(None, "2.0.abc1234", Channel::Stable),
            Status::NotInstalled { .. }
        ));
        assert!(matches!(
            decide(
                Some(&rec("2.0.abc1234", "stable")),
                "2.0.abc1234",
                Channel::Stable
            ),
            Status::UpToDate { .. }
        ));
        assert!(matches!(
            decide(
                Some(&rec("2.0.abc1234", "stable")),
                "2.0.def5678",
                Channel::Stable
            ),
            Status::UpdateAvailable { .. }
        ));
        // Same version string but a different channel ⇒ a channel switch is an update.
        assert!(matches!(
            decide(
                Some(&rec("2.0.abc1234", "stable")),
                "2.0.abc1234",
                Channel::Beta
            ),
            Status::UpdateAvailable { .. }
        ));
    }

    #[test]
    fn wants_auto_launch_truth_table() {
        let up_to_date = Some(Status::UpToDate {
            version: "2.0.abc".into(),
        });
        let update_available = Some(Status::UpdateAvailable {
            version: "2.0.def".into(),
            notes: String::new(),
        });

        // Nothing installed, or GUI binary missing ⇒ never skip the window.
        assert!(!wants_auto_launch(false, true, up_to_date.clone()));
        assert!(!wants_auto_launch(true, false, up_to_date.clone()));

        // Installed + up to date ⇒ silent hand-off.
        assert!(wants_auto_launch(true, true, up_to_date));

        // Installed + the check failed (offline) ⇒ launch what's on disk.
        assert!(wants_auto_launch(true, true, None));

        // Installed but an update is available ⇒ show the window.
        assert!(!wants_auto_launch(true, true, update_available));
    }

    #[test]
    fn url_override_reads_cli_then_env() {
        // SAFETY: single-threaded test; no other code reads this var here.
        unsafe { std::env::remove_var("GAGGLE_UPDATE_URL") };
        assert_eq!(url_override(None), None);
        assert_eq!(
            url_override(Some("http://x/y.json")),
            Some("http://x/y.json".to_string())
        );
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
    fn extract_into_flattens_and_sets_exec_bit() {
        use std::io::Write as _;
        use zip::write::SimpleFileOptions;

        let tmp = tempfile::tempdir().unwrap();
        let zip_path = tmp.path().join("pkg.zip");
        {
            let f = std::fs::File::create(&zip_path).unwrap();
            let mut zw = zip::ZipWriter::new(f);
            let opts = SimpleFileOptions::default();
            zw.start_file("nested/dir/gaggle-gui", opts).unwrap();
            zw.write_all(b"#!/bin/true\n").unwrap();
            zw.start_file("gaggle-launcher", opts).unwrap();
            zw.write_all(b"launcher-bytes").unwrap();
            zw.finish().unwrap();
        }

        let dest = tmp.path().join("bin");
        let n = extract_into(&zip_path, &dest).unwrap();
        assert_eq!(n, 2);
        assert!(dest.join("gaggle-gui").is_file());
        assert!(dest.join("gaggle-launcher").is_file());
        assert!(
            !dest.join("nested").exists(),
            "structure should be flattened"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dest.join("gaggle-gui"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111, "exec bits set");
        }
    }

    #[test]
    fn manifest_json_round_trips() {
        let src = r#"{
          "version": "2.0.deadbee",
          "channel": "beta",
          "notes": "hi",
          "pub_date": "2026-09-04T00:00:00Z",
          "platforms": {
            "linux-x86_64": { "url": "https://e/x.zip", "sha256": "ab", "size": 5 }
          }
        }"#;
        let m: Manifest = serde_json::from_str(src).unwrap();
        assert_eq!(m.version, "2.0.deadbee");
        assert_eq!(m.channel, "beta");
        let back = serde_json::to_string(&m).unwrap();
        assert_eq!(serde_json::from_str::<Manifest>(&back).unwrap(), m);
    }
}
