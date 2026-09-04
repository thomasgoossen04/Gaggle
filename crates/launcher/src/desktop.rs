//! Native OS integration: an apps-menu / Start Menu entry (always created on
//! install) and an optional desktop shortcut, both pointing at the installed
//! `gaggle-launcher` so every open re-checks for updates. macOS additionally
//! gets a real `Gaggle.app` bundle.
//!
//! Best-effort throughout: a shortcut failure is logged by the caller but
//! never fails the install (see [`crate::updater::install_archive`]).

use std::path::Path;

use anyhow::{Context, Result};

/// Placeholder "GG" wordmark, generated from `assets/icon.svg`.
const ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");
#[cfg(windows)]
const ICON_ICO: &[u8] = include_bytes!("../assets/icon.ico");
#[cfg(target_os = "macos")]
const ICON_ICNS: &[u8] = include_bytes!("../assets/icon.icns");

/// `2.0.<short-commit-hash>`, baked in by `build.rs`. Only read on macOS (the
/// `Info.plist` version field); `#[allow(dead_code)]` keeps other platforms'
/// builds warning-free.
#[allow(dead_code)]
const VERSION: &str = env!("GAGGLE_VERSION");

/// Which shortcuts to create. The menu / Start Menu entry is always made;
/// `desktop` is the user's opt-in checkbox.
#[derive(Debug, Clone, Copy, Default)]
pub struct Shortcuts {
    pub desktop: bool,
}

/// Create the native shortcut(s) for `launcher_bin` (the installed
/// `gaggle-launcher`). Best-effort: returns `Err` only when nothing could be
/// created at all, so callers can log a warning without failing the install.
pub fn install(launcher_bin: &Path, opts: Shortcuts) -> Result<()> {
    platform::install(launcher_bin, opts)
}

/// The `[Desktop Entry]` text for a Linux `.desktop` file. Only called on
/// Linux; `#[allow(dead_code)]` keeps other platforms' builds warning-free
/// (the tests below exercise it on every platform).
#[allow(dead_code)]
fn desktop_entry(exec: &Path, icon: &Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Gaggle\n\
         Comment=Share very large folders over private swarms\n\
         Exec=\"{exec}\" %U\n\
         Icon={icon}\n\
         Terminal=false\n\
         Categories=Network;FileTransfer;\n\
         StartupWMClass=gaggle-launcher\n",
        exec = exec.display(),
        icon = icon.display(),
    )
}

/// The macOS `Contents/Info.plist` text for the `Gaggle.app` bundle. Only
/// called on macOS; `#[allow(dead_code)]` keeps other platforms' builds
/// warning-free (the tests below exercise it on every platform).
#[allow(dead_code)]
fn info_plist(exec_name: &str, icon_name: &str, version: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>Gaggle</string>
    <key>CFBundleIdentifier</key>
    <string>com.gaggle.launcher</string>
    <key>CFBundleExecutable</key>
    <string>{exec_name}</string>
    <key>CFBundleIconFile</key>
    <string>{icon_name}</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>{version}</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
</dict>
</plist>
"#
    )
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use std::fs;

    pub fn install(launcher_bin: &Path, opts: Shortcuts) -> Result<()> {
        let icon = write_icon()?;
        let entry = desktop_entry(launcher_bin, &icon);

        let apps_dir = dirs::data_dir()
            .context("no OS data directory")?
            .join("applications");
        fs::create_dir_all(&apps_dir).with_context(|| format!("create {}", apps_dir.display()))?;
        let menu_path = apps_dir.join("gaggle.desktop");
        fs::write(&menu_path, &entry).with_context(|| format!("write {}", menu_path.display()))?;
        chmod_exec(&menu_path);

        // Best-effort: refresh the menu's desktop-file cache.
        let _ = std::process::Command::new("update-desktop-database")
            .arg(&apps_dir)
            .status();

        if opts.desktop
            && let Some(desktop_dir) = dirs::desktop_dir()
        {
            fs::create_dir_all(&desktop_dir).ok();
            let path = desktop_dir.join("gaggle.desktop");
            if fs::write(&path, &entry).is_ok() {
                chmod_exec(&path);
                // Best-effort: mark it "trusted" so file managers don't show
                // the untrusted-launcher warning.
                let _ = std::process::Command::new("gio")
                    .args(["set", &path.to_string_lossy(), "metadata::trusted", "true"])
                    .status();
            }
        }
        Ok(())
    }

    fn write_icon() -> Result<std::path::PathBuf> {
        let path = crate::paths::data_root()?.join("gaggle.png");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, ICON_PNG).with_context(|| format!("write {}", path.display()))?;
        Ok(path)
    }

    fn chmod_exec(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = fs::metadata(path) {
            let mut perm = meta.permissions();
            perm.set_mode(0o755);
            let _ = fs::set_permissions(path, perm);
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::fs;

    pub fn install(launcher_bin: &Path, opts: Shortcuts) -> Result<()> {
        let icon = write_icon()?;

        let start_menu = dirs::data_dir()
            .context("no OS data directory")?
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs");
        fs::create_dir_all(&start_menu)
            .with_context(|| format!("create {}", start_menu.display()))?;
        create_lnk(&start_menu.join("Gaggle.lnk"), launcher_bin, &icon)?;

        if opts.desktop
            && let Some(desktop_dir) = dirs::desktop_dir()
        {
            fs::create_dir_all(&desktop_dir).ok();
            let _ = create_lnk(&desktop_dir.join("Gaggle.lnk"), launcher_bin, &icon);
        }
        Ok(())
    }

    fn write_icon() -> Result<std::path::PathBuf> {
        let path = crate::paths::data_root()?.join("gaggle.ico");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, ICON_ICO).with_context(|| format!("write {}", path.display()))?;
        Ok(path)
    }

    /// Create a `.lnk` via PowerShell's `WScript.Shell` COM object — no extra
    /// crate needed.
    fn create_lnk(lnk: &Path, target: &Path, icon: &Path) -> Result<()> {
        let script = format!(
            "$s = New-Object -ComObject WScript.Shell; \
             $l = $s.CreateShortcut('{lnk}'); \
             $l.TargetPath = '{target}'; \
             $l.IconLocation = '{icon}'; \
             $l.WorkingDirectory = '{workdir}'; \
             $l.Save()",
            lnk = ps_quote(lnk),
            target = ps_quote(target),
            icon = ps_quote(icon),
            workdir = ps_quote(target.parent().unwrap_or(target)),
        );
        let status = std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .status()
            .context("run powershell")?;
        if !status.success() {
            anyhow::bail!("powershell exited with {status}");
        }
        Ok(())
    }

    /// Escape a path for embedding in a single-quoted PowerShell string.
    fn ps_quote(path: &Path) -> String {
        path.display().to_string().replace('\'', "''")
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    pub fn install(launcher_bin: &Path, opts: Shortcuts) -> Result<()> {
        let home = dirs::home_dir().context("no home directory")?;
        let app = home.join("Applications").join("Gaggle.app");
        let contents = app.join("Contents");
        let macos = contents.join("MacOS");
        let resources = contents.join("Resources");
        fs::create_dir_all(&macos).with_context(|| format!("create {}", macos.display()))?;
        fs::create_dir_all(&resources)
            .with_context(|| format!("create {}", resources.display()))?;

        fs::write(
            contents.join("Info.plist"),
            info_plist("gaggle-launcher", "gaggle", VERSION),
        )?;
        fs::write(resources.join("gaggle.icns"), ICON_ICNS)?;

        // A shim, not a copy of the binary, so the bundle keeps working
        // across self-updates (the real binary lives under install_dir).
        let shim = macos.join("gaggle-launcher");
        fs::write(
            &shim,
            format!("#!/bin/sh\nexec \"{}\" \"$@\"\n", launcher_bin.display()),
        )?;
        let mut perm = fs::metadata(&shim)?.permissions();
        perm.set_mode(0o755);
        fs::set_permissions(&shim, perm)?;

        // Best-effort: nudge Launch Services to pick up the new bundle.
        let _ = std::process::Command::new("touch").arg(&app).status();

        if opts.desktop
            && let Some(desktop_dir) = dirs::desktop_dir()
        {
            fs::create_dir_all(&desktop_dir).ok();
            let link = desktop_dir.join("Gaggle.app");
            let _ = fs::remove_file(&link);
            let _ = std::os::unix::fs::symlink(&app, &link);
        }
        Ok(())
    }
}

#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
mod platform {
    use super::*;

    pub fn install(_launcher_bin: &Path, _opts: Shortcuts) -> Result<()> {
        anyhow::bail!("no shortcut support on this platform")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn desktop_entry_has_required_keys() {
        let exec = PathBuf::from("/home/x/.local/share/Gaggle/bin/gaggle-launcher");
        let icon = PathBuf::from("/home/x/.local/share/Gaggle/gaggle.png");
        let text = desktop_entry(&exec, &icon);
        assert!(text.starts_with("[Desktop Entry]\n"));
        assert!(text.contains("Name=Gaggle\n"));
        assert!(text.contains(&format!("Exec=\"{}\" %U\n", exec.display())));
        assert!(text.contains(&format!("Icon={}\n", icon.display())));
        assert!(text.contains("Terminal=false\n"));
    }

    #[test]
    fn info_plist_is_well_formed_and_has_expected_keys() {
        let text = info_plist("gaggle-launcher", "gaggle", "2.0.abc1234");
        assert!(text.starts_with("<?xml"));
        assert!(text.contains("<key>CFBundleExecutable</key>"));
        assert!(text.contains("<string>gaggle-launcher</string>"));
        assert!(text.contains("<key>CFBundleIconFile</key>"));
        assert!(text.contains("<string>gaggle</string>"));
        assert!(text.contains("<string>2.0.abc1234</string>"));
        assert!(text.trim_end().ends_with("</plist>"));
        // Every opened tag closes: cheap balance check for the fixed skeleton.
        assert_eq!(
            text.matches("<dict>").count(),
            text.matches("</dict>").count()
        );
    }
}
