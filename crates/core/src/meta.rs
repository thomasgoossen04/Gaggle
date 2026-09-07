//! Optional, human-editable share metadata.
//!
//! A share can carry a small [`ShareMeta`] describing it — a display name, a
//! free-form version string, a description — plus any number of [`LaunchTarget`]
//! entries that say how to *run* something out of the folder (a game's `.exe`, a
//! server's `.sh`, a `.bat`). Each entry names a target OS, so a Windows game
//! shared into a Linux swarm can still carry a "run it through Proton/Wine"
//! entry.
//!
//! It lives in the share folder as [`META_FILENAME`], so it is chunked,
//! transferred and verified exactly like any other file and lands in the
//! downloaded tree automatically — no manifest or wire-protocol change. The
//! origin writes it when the share is created; a downloader reads it back out of
//! the materialized folder.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// The file a [`ShareMeta`] is stored as, at the root of the share folder.
pub const META_FILENAME: &str = ".gaggle-meta.toml";

/// Descriptive metadata + launch entries for a share. Every field is optional;
/// an all-empty value is treated as "no metadata" and is not written to disk.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareMeta {
    /// A friendly display name, shown instead of the folder name where present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Free-form version label (`"1.5.97"`, `"2024-Q1"`, …) — independent of the
    /// manifest's monotonic numeric `version`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// A short human description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Ways to launch an executable from the folder. Rendered by the GUI as
    /// "Run" entries on a transfer once its files are on disk.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub launch: Vec<LaunchTarget>,
}

/// Which OS family a [`LaunchTarget`] is built for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TargetOs {
    /// A cross-platform launcher (a script, a `.jar` wrapper) — runs on any host.
    #[default]
    Any,
    Windows,
    Linux,
    Macos,
}

impl TargetOs {
    pub const ALL: [TargetOs; 4] =
        [TargetOs::Any, TargetOs::Windows, TargetOs::Linux, TargetOs::Macos];

    pub fn label(self) -> &'static str {
        match self {
            TargetOs::Any => "Any",
            TargetOs::Windows => "Windows",
            TargetOs::Linux => "Linux",
            TargetOs::Macos => "macOS",
        }
    }

    /// The OS family this binary is running on.
    pub fn host() -> TargetOs {
        if cfg!(target_os = "windows") {
            TargetOs::Windows
        } else if cfg!(target_os = "macos") {
            TargetOs::Macos
        } else {
            TargetOs::Linux
        }
    }

    /// Whether a target of this OS runs natively on the current host (`Any`
    /// always does).
    pub fn runs_native(self) -> bool {
        self == TargetOs::Any || self == TargetOs::host()
    }

    /// Whether a "run it anyway through Wine/Proton" option is meaningful for a
    /// target built for this OS — only Windows, since that is what Wine/Proton
    /// run. Deliberately host-independent, so a share's creator can enable it
    /// from a Windows machine for their Linux/macOS downloaders. (There is no
    /// equivalent for running a macOS or Linux binary on a foreign host.)
    pub fn supports_compat(self) -> bool {
        matches!(self, TargetOs::Windows)
    }
}

/// One launchable entry: an executable path relative to the share root, plus how
/// to invoke it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchTarget {
    /// Button label, e.g. `"Play"`, `"Mod Organizer 2"`.
    pub label: String,
    /// OS family the executable is built for.
    #[serde(default)]
    pub os: TargetOs,
    /// Executable path, relative to the share root, `/`-separated
    /// (`"bin/game.exe"`).
    pub path: String,
    /// Extra arguments passed to the executable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Working directory for the process, relative to the share root. Empty ⇒
    /// the share root itself.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub workdir: String,
    /// Run through a compatibility layer (Wine / Proton) when the host OS is not
    /// [`Self::os`]. Advisory: a non-native target with `compat` unset simply
    /// won't offer to run on a foreign host.
    #[serde(default)]
    pub compat: bool,
}

impl LaunchTarget {
    /// Validate [`Self::path`] as a clean relative path that cannot escape the
    /// share root.
    pub fn checked_path(&self) -> Result<&str> {
        crate::manifest::check_rel_path(&self.path)?;
        Ok(&self.path)
    }

    /// Validate [`Self::workdir`] (empty is fine ⇒ the caller uses the root).
    pub fn checked_workdir(&self) -> Result<Option<&str>> {
        if self.workdir.is_empty() {
            return Ok(None);
        }
        crate::manifest::check_rel_path(&self.workdir)?;
        Ok(Some(&self.workdir))
    }

    /// Whether this entry can be launched on the current host: either it runs
    /// natively, or it is marked [`compat`](Self::compat) and is a Windows
    /// target on a non-Windows host (so Wine/Proton can run it).
    pub fn launchable_on_host(&self) -> bool {
        if self.os.runs_native() {
            return true;
        }
        self.compat && self.os.supports_compat() && TargetOs::host() != TargetOs::Windows
    }
}

impl ShareMeta {
    /// `true` when nothing is set — the origin skips writing the file, and a
    /// downloader treats a parsed-but-empty file as "no metadata".
    pub fn is_empty(&self) -> bool {
        self.name.as_deref().is_none_or(str::is_empty)
            && self.version.as_deref().is_none_or(str::is_empty)
            && self.description.as_deref().is_none_or(str::is_empty)
            && self.launch.is_empty()
    }

    /// Parse from the TOML text of a [`META_FILENAME`] file.
    pub fn from_toml_str(s: &str) -> Result<Self> {
        toml::from_str(s).map_err(|e| Error::Meta(e.to_string()))
    }

    /// Render to TOML for writing as [`META_FILENAME`].
    pub fn to_toml_string(&self) -> Result<String> {
        toml::to_string_pretty(self).map_err(|e| Error::Meta(e.to_string()))
    }

    /// Drop empty strings to `None`, trim, and discard launch entries with no
    /// label or no path — call before persisting a form-built value.
    pub fn normalized(mut self) -> Self {
        let clean = |o: Option<String>| {
            o.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
        };
        self.name = clean(self.name);
        self.version = clean(self.version);
        self.description = clean(self.description);
        self.launch.retain_mut(|l| {
            l.label = l.label.trim().to_string();
            l.path = l.path.trim().trim_start_matches(['/', '\\']).replace('\\', "/");
            l.workdir = l.workdir.trim().trim_matches(['/', '\\']).replace('\\', "/");
            l.args.retain(|a| !a.trim().is_empty());
            !l.label.is_empty() && !l.path.is_empty()
        });
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_toml() {
        let m = ShareMeta {
            name: Some("Skyrim SE — Modded".into()),
            version: Some("1.5.97".into()),
            description: Some("300 mods, MO2 profile bundled".into()),
            launch: vec![
                LaunchTarget {
                    label: "Play".into(),
                    os: TargetOs::Windows,
                    path: "SkyrimSE.exe".into(),
                    args: vec!["--skip-intro".into()],
                    workdir: String::new(),
                    compat: true,
                },
                LaunchTarget {
                    label: "Dedicated server".into(),
                    os: TargetOs::Linux,
                    path: "server/start.sh".into(),
                    args: vec![],
                    workdir: "server".into(),
                    compat: false,
                },
            ],
        };
        let text = m.to_toml_string().unwrap();
        assert_eq!(ShareMeta::from_toml_str(&text).unwrap(), m);
    }

    #[test]
    fn empty_meta_is_empty() {
        assert!(ShareMeta::default().is_empty());
        assert!(!ShareMeta { version: Some("1".into()), ..Default::default() }.is_empty());
    }

    #[test]
    fn launch_path_is_validated() {
        let bad = LaunchTarget {
            label: "x".into(),
            os: TargetOs::Any,
            path: "../../etc/passwd".into(),
            args: vec![],
            workdir: String::new(),
            compat: false,
        };
        assert!(bad.checked_path().is_err());

        let ok = LaunchTarget { path: "bin/run".into(), ..bad.clone() };
        assert_eq!(ok.checked_path().unwrap(), "bin/run");
    }

    #[test]
    fn normalized_drops_blank_entries_and_leading_slashes() {
        let m = ShareMeta {
            name: Some("   ".into()),
            version: Some(" 2.0 ".into()),
            description: None,
            launch: vec![
                LaunchTarget {
                    label: "  ".into(),
                    os: TargetOs::Any,
                    path: "x".into(),
                    args: vec![],
                    workdir: String::new(),
                    compat: false,
                },
                LaunchTarget {
                    label: "Run".into(),
                    os: TargetOs::Windows,
                    path: "/Game/game.exe".into(),
                    args: vec![" ".into(), "-w".into()],
                    workdir: "/Game".into(),
                    compat: true,
                },
            ],
        }
        .normalized();
        assert_eq!(m.name, None);
        assert_eq!(m.version.as_deref(), Some("2.0"));
        assert_eq!(m.launch.len(), 1);
        assert_eq!(m.launch[0].path, "Game/game.exe");
        assert_eq!(m.launch[0].workdir, "Game");
        assert_eq!(m.launch[0].args, ["-w"]);
    }

    #[test]
    fn launchable_on_host_logic() {
        let native_any = LaunchTarget {
            label: "s".into(),
            os: TargetOs::Any,
            path: "s".into(),
            args: vec![],
            workdir: String::new(),
            compat: false,
        };
        assert!(native_any.launchable_on_host());

        let win_compat = LaunchTarget { os: TargetOs::Windows, compat: true, ..native_any.clone() };
        // Only meaningful off Windows; on a Windows host it's native anyway.
        assert!(win_compat.launchable_on_host());

        let win_no_compat =
            LaunchTarget { os: TargetOs::Windows, compat: false, ..native_any.clone() };
        assert_eq!(win_no_compat.launchable_on_host(), TargetOs::host() == TargetOs::Windows);

        // A macOS target with `compat` set is *not* launchable off macOS — Wine
        // / Proton only run Windows binaries.
        let mac_compat = LaunchTarget { os: TargetOs::Macos, compat: true, ..native_any };
        assert_eq!(mac_compat.launchable_on_host(), TargetOs::host() == TargetOs::Macos);

        assert!(TargetOs::Windows.supports_compat());
        assert!(!TargetOs::Macos.supports_compat());
        assert!(!TargetOs::Linux.supports_compat());
        assert!(!TargetOs::Any.supports_compat());
    }
}
