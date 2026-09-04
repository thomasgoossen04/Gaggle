//! The folder manifest — Gaggle's "torrent file" equivalent.
//!
//! It is deliberately small: one [`FileEntry`] per file carrying the file's size
//! and Merkle root, plus the list of (possibly empty) directories. It does **not**
//! embed per-file chunk lists — for a 100 GB folder that would be megabytes of
//! hashes. A subscriber takes the manifest, then fetches each [`ChunkList`] it
//! wants and verifies it against the root recorded here.
//!
//! [`ChunkList`]: crate::chunklist::ChunkList

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::hash::Hash;

/// Value of [`Manifest::format`]. Bumped only on a breaking wire-format change.
pub const MANIFEST_FORMAT: &str = "gaggle-manifest-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Path relative to the share root, `/`-separated. No `.` / `..` / empty
    /// components, never absolute.
    pub path: String,
    pub size: u64,
    /// Merkle root over the file's chunk hashes (see [`crate::merkle`]).
    pub root: Hash,
    /// Unix permission bits (`& 0o7777`). `None` on platforms without them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: String,
    /// Monotonic version. A folder update bumps this; files whose bytes did not
    /// change keep their `root`, so a subscriber diffs roots and pulls only the
    /// chunks it is missing.
    pub version: u64,
    /// Human-facing label (usually the folder name). Not identity-bearing.
    pub name: String,
    /// Files, sorted and unique by `path` after [`canonicalize`](Self::canonicalize).
    pub files: Vec<FileEntry>,
    /// Directories (so empty ones survive a round trip), sorted and unique.
    pub dirs: Vec<String>,
}

impl Manifest {
    pub fn new(name: impl Into<String>, version: u64) -> Self {
        Self {
            format: MANIFEST_FORMAT.to_string(),
            version,
            name: name.into(),
            files: Vec::new(),
            dirs: Vec::new(),
        }
    }

    /// Sort and de-duplicate entries so serialization is byte-stable regardless
    /// of insertion order.
    pub fn canonicalize(&mut self) {
        self.files.sort_by(|a, b| a.path.cmp(&b.path));
        self.files.dedup_by(|a, b| a.path == b.path);
        self.dirs.sort();
        self.dirs.dedup();
    }

    pub fn total_size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    /// Look up a file by exact path. Requires the manifest to be canonicalized.
    pub fn file(&self, path: &str) -> Option<&FileEntry> {
        self.files
            .binary_search_by(|f| f.path.as_str().cmp(path))
            .ok()
            .map(|i| &self.files[i])
    }

    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Content address of the manifest itself: `blake3` over its compact JSON.
    ///
    /// This is the "manifest hash" an invite link carries and the
    /// key a share is announced and discovered under on the DHT.
    /// [`canonicalize`](Self::canonicalize) first — the hash is only stable for
    /// sorted, de-duplicated entries.
    pub fn id(&self) -> Hash {
        // `serde_json` serializes struct fields in declaration order and this
        // manifest's collections are pre-sorted, so the bytes are deterministic.
        let bytes = serde_json::to_vec(self).expect("a Manifest always serializes");
        Hash::of(&bytes)
    }

    /// Parse and [`validate`](Self::validate).
    pub fn from_json(s: &str) -> Result<Self> {
        let m: Manifest = serde_json::from_str(s)?;
        m.validate()?;
        Ok(m)
    }

    /// Structural checks: known format, safe relative paths, entries sorted and
    /// unique.
    pub fn validate(&self) -> Result<()> {
        if self.format != MANIFEST_FORMAT {
            return Err(Error::Manifest(format!("unknown format {:?}", self.format)));
        }
        for f in &self.files {
            check_rel_path(&f.path)?;
        }
        for d in &self.dirs {
            check_rel_path(d)?;
        }
        if self.files.windows(2).any(|w| w[0].path >= w[1].path) {
            return Err(Error::Manifest("files not sorted and unique by path".into()));
        }
        if self.dirs.windows(2).any(|w| w[0] >= w[1]) {
            return Err(Error::Manifest("dirs not sorted and unique".into()));
        }
        Ok(())
    }

    /// Compare two manifests by path and root. A file present in both with an
    /// unchanged root needs no transfer.
    pub fn diff<'a>(old: &'a Manifest, new: &'a Manifest) -> ManifestDiff<'a> {
        let mut old_sorted: Vec<&FileEntry> = old.files.iter().collect();
        let mut new_sorted: Vec<&FileEntry> = new.files.iter().collect();
        old_sorted.sort_by(|a, b| a.path.cmp(&b.path));
        new_sorted.sort_by(|a, b| a.path.cmp(&b.path));

        let mut diff = ManifestDiff::default();
        let (mut i, mut j) = (0, 0);
        while i < old_sorted.len() && j < new_sorted.len() {
            let (o, n) = (old_sorted[i], new_sorted[j]);
            match o.path.cmp(&n.path) {
                std::cmp::Ordering::Less => {
                    diff.removed.push(o);
                    i += 1;
                }
                std::cmp::Ordering::Greater => {
                    diff.added.push(n);
                    j += 1;
                }
                std::cmp::Ordering::Equal => {
                    if o.root == n.root {
                        diff.unchanged += 1;
                    } else {
                        diff.changed.push((o, n));
                    }
                    i += 1;
                    j += 1;
                }
            }
        }
        diff.removed.extend(old_sorted[i..].iter().copied());
        diff.added.extend(new_sorted[j..].iter().copied());
        diff
    }
}

/// Result of [`Manifest::diff`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ManifestDiff<'a> {
    pub added: Vec<&'a FileEntry>,
    pub removed: Vec<&'a FileEntry>,
    /// `(old, new)` pairs at the same path with different roots.
    pub changed: Vec<(&'a FileEntry, &'a FileEntry)>,
    pub unchanged: usize,
}

impl ManifestDiff<'_> {
    /// No files added, removed, or changed.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
}

/// Reject anything that could escape the share root or is not a clean relative
/// path.
fn check_rel_path(p: &str) -> Result<()> {
    if p.is_empty() {
        return Err(Error::Manifest("empty path".into()));
    }
    if p.starts_with('/') || p.contains('\\') || p.contains('\0') {
        return Err(Error::Manifest(format!("unsafe path {p:?}")));
    }
    for comp in p.split('/') {
        if comp.is_empty() || comp == "." || comp == ".." {
            return Err(Error::Manifest(format!("unsafe path component in {p:?}")));
        }
        // Keep materialization safe on Windows regardless of which OS produced
        // the manifest. `:` is a drive letter (`C:evil` → `root.join` escapes
        // the target dir) or an NTFS alternate-data-stream; a trailing dot or
        // space and the reserved DOS device names (CON, NUL, COM1…) are traps
        // that would silently break `write_share` on a Windows peer.
        if comp.contains(':') {
            return Err(Error::Manifest(format!("unsafe path component {comp:?} in {p:?}")));
        }
        if comp.ends_with(['.', ' ']) {
            return Err(Error::Manifest(format!("trailing dot or space in {comp:?} ({p:?})")));
        }
        if is_dos_device_name(comp) {
            return Err(Error::Manifest(format!("reserved device name {comp:?} in {p:?}")));
        }
    }
    Ok(())
}

/// Windows reserves these names (optionally with any extension, any case):
/// `CON PRN AUX NUL COM1-9 LPT1-9`. A file so named cannot be created there.
fn is_dos_device_name(comp: &str) -> bool {
    let stem = comp.split('.').next().unwrap_or(comp);
    let s = stem.to_ascii_uppercase();
    match s.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" => true,
        _ => {
            let b = s.as_bytes();
            b.len() == 4
                && (s.starts_with("COM") || s.starts_with("LPT"))
                && b[3].is_ascii_digit()
                && b[3] != b'0'
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, seed: &[u8]) -> FileEntry {
        FileEntry { path: path.into(), size: seed.len() as u64, root: Hash::of(seed), mode: None }
    }

    #[test]
    fn canonicalize_sorts_and_dedups() {
        let mut m = Manifest::new("share", 1);
        m.files.push(file("b.txt", b"b"));
        m.files.push(file("a.txt", b"a"));
        m.files.push(file("a.txt", b"a"));
        m.dirs.push("z".into());
        m.dirs.push("a".into());
        m.dirs.push("a".into());
        m.canonicalize();

        assert_eq!(m.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["a.txt", "b.txt"]);
        assert_eq!(m.dirs, ["a", "z"]);
        m.validate().unwrap();
    }

    #[test]
    fn id_is_stable_and_content_sensitive() {
        let mut a = Manifest::new("modpack", 1);
        a.files.push(file("b.jar", b"bbb"));
        a.files.push(file("a.jar", b"aaa"));
        a.canonicalize();

        // Insertion order does not matter once canonicalized.
        let mut b = Manifest::new("modpack", 1);
        b.files.push(file("a.jar", b"aaa"));
        b.files.push(file("b.jar", b"bbb"));
        b.canonicalize();
        assert_eq!(a.id(), b.id());

        // Any content change moves the id.
        let mut c = a.clone();
        c.version = 2;
        assert_ne!(a.id(), c.id());
        let mut d = a.clone();
        d.files[0].root = Hash::of(b"different");
        assert_ne!(a.id(), d.id());
    }

    #[test]
    fn json_round_trip_with_hex_roots() {
        let mut m = Manifest::new("modpack", 7);
        m.files.push(file("mods/a.jar", b"aaa"));
        m.files.push(file("config/x.cfg", b"xxx"));
        m.dirs.push("config".into());
        m.dirs.push("mods".into());
        m.canonicalize();

        let json = m.to_json().unwrap();
        assert!(json.contains(&m.files[0].root.to_hex()));
        assert_eq!(Manifest::from_json(&json).unwrap(), m);
    }

    #[test]
    fn validate_rejects_bad_input() {
        let mut m = Manifest::new("s", 1);
        m.format = "something-else".into();
        assert!(m.validate().is_err());

        for bad in [
            "../evil",
            "/etc/passwd",
            "a//b",
            "a/./b",
            "",
            "a\\b",
            // Windows-hostile: drive-relative, ADS, reserved names, trailing dot.
            "C:evil.exe",
            "sub/C:/evil",
            "mods/stream.txt:zone",
            "NUL",
            "dir/con",
            "COM1",
            "lpt9.txt",
            "weird.",
            "trailing space ",
        ] {
            let mut m = Manifest::new("s", 1);
            m.files.push(file(bad, b"x"));
            assert!(m.validate().is_err(), "should reject {bad:?}");
        }

        // …but names that merely resemble device names are fine.
        for ok in ["coma", "coms.txt", "com10", "com0", "coma/lptx", "coneheads"] {
            let mut m = Manifest::new("s", 1);
            m.files.push(file(ok, b"x"));
            m.canonicalize();
            assert!(m.validate().is_ok(), "should accept {ok:?}");
        }

        let mut m = Manifest::new("s", 1);
        m.files.push(file("b", b"b"));
        m.files.push(file("a", b"a")); // not sorted
        assert!(m.validate().is_err());
    }

    #[test]
    fn diff_classifies_every_file() {
        let mut old = Manifest::new("s", 1);
        old.files.push(file("keep", b"same"));
        old.files.push(file("edit", b"old"));
        old.files.push(file("gone", b"x"));
        old.canonicalize();

        let mut new = Manifest::new("s", 2);
        new.files.push(file("keep", b"same"));
        new.files.push(file("edit", b"new"));
        new.files.push(file("fresh", b"y"));
        new.canonicalize();

        let d = Manifest::diff(&old, &new);
        assert_eq!(d.unchanged, 1);
        assert_eq!(d.added.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["fresh"]);
        assert_eq!(d.removed.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["gone"]);
        assert_eq!(d.changed.len(), 1);
        assert_eq!(d.changed[0].0.path, "edit");
        assert!(!d.is_empty());

        assert!(Manifest::diff(&old, &old).is_empty());
    }
}
