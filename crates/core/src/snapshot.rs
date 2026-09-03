//! [`snapshot_dir`] — the one place in this crate that touches the filesystem.
//!
//! It walks a folder, chunks every regular file (reading incrementally, so a
//! 100 GB folder never needs 100 GB of RAM), pushes chunk bytes into a
//! [`ChunkStore`], and returns the [`Manifest`] plus the per-file [`ChunkList`]s.
//! It only reads.

use std::collections::BTreeMap;
use std::fs::{self, File, Metadata};
use std::io::BufReader;
use std::path::{Component, Path, PathBuf};

use crate::chunk::{ChunkerConfig, chunk_reader};
use crate::chunklist::ChunkList;
use crate::error::{Error, Result};
use crate::manifest::{FileEntry, Manifest};
use crate::store::ChunkStore;

/// Everything milestone 1 derives from a folder.
pub struct Snapshot {
    /// The small, shareable document.
    pub manifest: Manifest,
    /// Per-file chunk lists, keyed by the same relative path used in the
    /// manifest. Each one's Merkle root equals the matching `FileEntry::root`.
    pub chunk_lists: BTreeMap<String, ChunkList>,
    /// Paths that were not regular files or directories (symlinks, sockets,
    /// fifos, devices) and were left out.
    pub skipped: Vec<PathBuf>,
}

/// Chunk every regular file under `root`, populate `store`, and build the
/// manifest + chunk lists. Chunk size adapts to each file (see
/// [`ChunkerConfig::for_file_size`]).
pub fn snapshot_dir(
    root: &Path,
    name: impl Into<String>,
    version: u64,
    store: &mut dyn ChunkStore,
) -> Result<Snapshot> {
    if !root.is_dir() {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::NotADirectory,
            format!("{} is not a directory", root.display()),
        )));
    }

    let mut files = Vec::new();
    let mut dirs = Vec::new();
    let mut skipped = Vec::new();
    collect(root, &mut files, &mut dirs, &mut skipped)?;

    let mut manifest = Manifest::new(name, version);
    for dir in &dirs {
        manifest.dirs.push(rel_path(root, dir)?);
    }

    let mut chunk_lists = BTreeMap::new();
    for path in &files {
        let rel = rel_path(root, path)?;
        let meta = fs::symlink_metadata(path)?;
        let size = meta.len();
        let cfg = ChunkerConfig::for_file_size(size);

        let reader = BufReader::new(File::open(path)?);
        let mut chunks = Vec::new();
        for item in chunk_reader(reader, cfg)? {
            let cwd = item?;
            store.put(cwd.chunk.hash, cwd.data);
            chunks.push(cwd.chunk);
        }

        let list = ChunkList::from_chunks(&chunks);
        if list.total_size != size {
            return Err(Error::Verify(format!(
                "{rel}: chunked {} bytes but the file is {size}",
                list.total_size
            )));
        }
        manifest.files.push(FileEntry { path: rel.clone(), size, root: list.root(), mode: mode_of(&meta) });
        chunk_lists.insert(rel, list);
    }

    manifest.canonicalize();
    manifest.validate()?;
    Ok(Snapshot { manifest, chunk_lists, skipped })
}

/// Iterative depth-first walk. Directory entries are sorted so `skipped` is
/// deterministic; the manifest is canonicalized regardless.
fn collect(
    root: &Path,
    files: &mut Vec<PathBuf>,
    dirs: &mut Vec<PathBuf>,
    skipped: &mut Vec<PathBuf>,
) -> Result<()> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut entries: Vec<_> = fs::read_dir(&dir)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                dirs.push(path.clone());
                stack.push(path);
            } else if file_type.is_file() {
                files.push(path);
            } else {
                skipped.push(path);
            }
        }
    }
    Ok(())
}

/// `path` relative to `root`, as a `/`-separated string. Errors on non-UTF-8 or
/// any non-`Normal` component.
fn rel_path(root: &Path, path: &Path) -> Result<String> {
    let stripped = path
        .strip_prefix(root)
        .map_err(|_| Error::Manifest(format!("{} is not under {}", path.display(), root.display())))?;

    let mut out = String::new();
    for comp in stripped.components() {
        let Component::Normal(part) = comp else {
            return Err(Error::Manifest(format!("odd path component in {}", path.display())));
        };
        let part = part
            .to_str()
            .ok_or_else(|| Error::Manifest(format!("non-UTF-8 path {}", path.display())))?;
        if !out.is_empty() {
            out.push('/');
        }
        out.push_str(part);
    }
    if out.is_empty() {
        return Err(Error::Manifest("empty relative path".into()));
    }
    Ok(out)
}

#[cfg(unix)]
fn mode_of(meta: &Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(meta.permissions().mode() & 0o7777)
}

#[cfg(not(unix))]
fn mode_of(_meta: &Metadata) -> Option<u32> {
    None
}
