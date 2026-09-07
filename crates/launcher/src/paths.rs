//! Where the launcher installs and records the GUI.
//!
//! Two shapes:
//!
//! * **macOS, run from `Gaggle.app`** (the `.dmg` install) — the GUI ships
//!   *inside* the bundle at `Contents/MacOS/gaggle-gui`, and a self-update
//!   swaps the whole bundle in place. Nothing is installed under the data dir.
//! * **everything else** (Linux, Windows, or a bare `gaggle-launcher` binary on
//!   macOS) — the launcher downloads the GUI into the per-user data dir, so no
//!   elevated rights are needed: `~/.local/share/Gaggle` (Linux),
//!   `~/Library/Application Support/Gaggle` (macOS), `%APPDATA%\Gaggle`.
//!
//! `installed.json` / `launcher.json` always live under the data dir — they
//! must survive a bundle swap.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// `<data-dir>/Gaggle`.
pub fn data_root() -> Result<PathBuf> {
    Ok(dirs::data_dir()
        .context("no OS data directory")?
        .join("Gaggle"))
}

/// `<data-dir>/Gaggle/bin` — holds `gaggle-gui` and a copy of `gaggle-launcher`
/// on the non-bundle install path.
pub fn install_dir() -> Result<PathBuf> {
    Ok(data_root()?.join("bin"))
}

/// The `Gaggle.app` this launcher is running from, if any — i.e. the `.dmg`
/// install. `None` for a bare `gaggle-launcher` binary or on other platforms.
#[cfg(target_os = "macos")]
pub fn macos_bundle_root() -> Option<PathBuf> {
    bundle_root_from_exe(&std::env::current_exe().ok()?)
}

/// Pure core of [`macos_bundle_root`]: given `…/Gaggle.app/Contents/MacOS/<exe>`,
/// return `…/Gaggle.app`. `None` if the path isn't inside a `*.app/Contents/MacOS`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn bundle_root_from_exe(exe: &Path) -> Option<PathBuf> {
    let macos_dir = exe.parent()?;
    let contents = macos_dir.parent()?;
    let app = contents.parent()?;
    (macos_dir.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && app.extension().is_some_and(|x| x.eq_ignore_ascii_case("app")))
    .then(|| app.to_path_buf())
}

/// `CFBundleShortVersionString` from the running bundle's `Info.plist`, if we
/// are running from one. Lets the launcher treat a fresh `.dmg` install as
/// "installed at this version" before the first self-update writes
/// `installed.json`.
#[cfg(target_os = "macos")]
pub fn macos_bundle_version() -> Option<String> {
    let plist = std::fs::read_to_string(macos_bundle_root()?.join("Contents/Info.plist")).ok()?;
    plist_string_value(&plist, "CFBundleShortVersionString")
}

/// Minimal `<key>NAME</key><string>VALUE</string>` reader for the one field we
/// need — avoids pulling in a plist crate for a file we also generate.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn plist_string_value(plist: &str, key: &str) -> Option<String> {
    let after = plist.split_once(&format!("<key>{key}</key>"))?.1;
    let start = after.find("<string>")? + "<string>".len();
    let end = after[start..].find("</string>")?;
    Some(after[start..start + end].trim().to_string())
}

/// The installed GUI executable path (`.exe` on Windows). Inside the bundle on
/// a macOS `.dmg` install; under the data dir otherwise.
pub fn gui_binary() -> Result<PathBuf> {
    #[cfg(target_os = "macos")]
    if let Some(app) = macos_bundle_root() {
        return Ok(app.join("Contents/MacOS/gaggle-gui"));
    }
    let name = if cfg!(windows) {
        "gaggle-gui.exe"
    } else {
        "gaggle-gui"
    };
    Ok(install_dir()?.join(name))
}

/// The installed launcher executable path (`.exe` on Windows) — the release
/// zip extracts a copy of `gaggle-launcher` alongside `gaggle-gui`, and
/// shortcuts point at this copy so they keep working across self-updates.
/// (Unused on the macOS bundle path — the bundle is the install.)
pub fn installed_launcher() -> Result<PathBuf> {
    let name = if cfg!(windows) {
        "gaggle-launcher.exe"
    } else {
        "gaggle-launcher"
    };
    Ok(install_dir()?.join(name))
}

/// `<data-dir>/Gaggle/installed.json` — the record of what version is on disk.
pub fn installed_json() -> Result<PathBuf> {
    Ok(data_root()?.join("installed.json"))
}

/// `<data-dir>/Gaggle/launcher.json` — the launcher's own settings (currently
/// just the selected release channel).
pub fn launcher_json() -> Result<PathBuf> {
    Ok(data_root()?.join("launcher.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_root_recognised_only_inside_an_app() {
        assert_eq!(
            bundle_root_from_exe(Path::new(
                "/Applications/Gaggle.app/Contents/MacOS/gaggle-launcher"
            )),
            Some(PathBuf::from("/Applications/Gaggle.app"))
        );
        assert_eq!(
            bundle_root_from_exe(Path::new("/usr/local/bin/gaggle-launcher")),
            None
        );
        // Right leaf shape, but the top dir isn't a *.app.
        assert_eq!(
            bundle_root_from_exe(Path::new("/tmp/Gaggle/Contents/MacOS/gaggle-launcher")),
            None
        );
    }

    #[test]
    fn plist_value_is_extracted() {
        let plist = r#"
            <dict>
                <key>CFBundleExecutable</key>
                <string>gaggle-launcher</string>
                <key>CFBundleShortVersionString</key>
                <string>2.0.deadbee</string>
            </dict>"#;
        assert_eq!(
            plist_string_value(plist, "CFBundleShortVersionString").as_deref(),
            Some("2.0.deadbee")
        );
        assert_eq!(plist_string_value(plist, "CFBundleMissing"), None);
    }
}
