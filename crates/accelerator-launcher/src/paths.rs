//! Where this launcher installs the `accelerator` binary and records its own
//! state.
//!
//! Deliberately its own subdirectory (`<data-dir>/Gaggle/accelerator-launcher`)
//! rather than the desktop launcher's `<data-dir>/Gaggle` root: the two
//! self-updaters track independent products (the accelerator daemon vs. the
//! GUI) with independent versions/channels, and must never share (or clobber)
//! each other's `installed.json` / `launcher.json`. No elevated rights are
//! needed either way.

use std::path::PathBuf;

use anyhow::{Context, Result};

/// `<data-dir>/Gaggle/accelerator-launcher`.
pub fn data_root() -> Result<PathBuf> {
    Ok(dirs::data_dir()
        .context("no OS data directory")?
        .join("Gaggle")
        .join("accelerator-launcher"))
}

/// `<data-root>/bin` — holds the installed `accelerator` binary.
pub fn install_dir() -> Result<PathBuf> {
    Ok(data_root()?.join("bin"))
}

/// The installed accelerator daemon executable path (`.exe` on Windows).
pub fn accelerator_binary() -> Result<PathBuf> {
    let name = if cfg!(windows) { "accelerator.exe" } else { "accelerator" };
    Ok(install_dir()?.join(name))
}

/// `<data-root>/installed.json` — the record of what version is on disk.
pub fn installed_json() -> Result<PathBuf> {
    Ok(data_root()?.join("installed.json"))
}

/// `<data-root>/launcher.json` — this launcher's own settings (currently just
/// the selected release channel).
pub fn launcher_json() -> Result<PathBuf> {
    Ok(data_root()?.join("launcher.json"))
}
