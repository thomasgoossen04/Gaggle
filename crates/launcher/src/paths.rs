//! Where the launcher installs and records the GUI.
//!
//! Everything lives under the per-user data dir so no elevated rights are
//! needed: `~/.local/share/Gaggle` (Linux), `~/Library/Application
//! Support/Gaggle` (macOS), `%APPDATA%\Gaggle` (Windows).

use std::path::PathBuf;

use anyhow::{Context, Result};

/// `<data-dir>/Gaggle`.
pub fn data_root() -> Result<PathBuf> {
    Ok(dirs::data_dir()
        .context("no OS data directory")?
        .join("Gaggle"))
}

/// `<data-dir>/Gaggle/bin` — holds `gaggle-gui` and a copy of `gaggle-launcher`.
pub fn install_dir() -> Result<PathBuf> {
    Ok(data_root()?.join("bin"))
}

/// The installed GUI executable path (`.exe` on Windows).
pub fn gui_binary() -> Result<PathBuf> {
    let name = if cfg!(windows) {
        "gaggle-gui.exe"
    } else {
        "gaggle-gui"
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
