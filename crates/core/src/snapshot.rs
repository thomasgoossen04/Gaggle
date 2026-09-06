//! [`snapshot_dir`] — the one place in this crate that *reads* the filesystem to
//! build a share, and [`write_share`] — its inverse, materializing a share's
//! files from a [`ChunkStore`] back onto disk (the cache/NAS accelerator's
//! replica).
//!
//! `snapshot_dir` walks a folder, chunks every regular file (reading
//! incrementally, so a 100 GB folder never needs 100 GB of RAM), pushes chunk
//! bytes into a [`ChunkStore`], and returns the [`Manifest`] plus the per-file
//! [`ChunkList`]s.
//!
//! The chunk-and-hash pass runs on the rayon pool — files in parallel, and
//! large chunks tree-hashed in parallel too (see [`Hash::of`]) — while the
//! results are collected on the calling thread through a bounded channel, so the
//! caller's `ChunkStore` and progress callback never see more than one thread.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File, Metadata};
use std::io::{BufReader, BufWriter, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use rayon::prelude::*;

use crate::chunk::{ChunkWithData, ChunkerConfig, chunk_reader};
use crate::chunklist::ChunkList;
use crate::error::{Error, Result};
use crate::hash::Hash;
use crate::manifest::{FileEntry, Manifest};
use crate::store::{ChunkLocation, ChunkStore};

/// Everything derived from a folder.
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

/// A folder scan that also records where each chunk lives in the source tree,
/// so a share can be **served by reading chunks back from the original files on
/// demand** — no chunk bytes are retained. See [`index_dir`].
pub struct IndexedSnapshot {
    pub manifest: Manifest,
    pub chunk_lists: BTreeMap<String, ChunkList>,
    pub skipped: Vec<PathBuf>,
    /// First-seen source location of every distinct chunk hash. A chunk shared
    /// between files (dedup) is recorded once — any occurrence serves it.
    pub locations: HashMap<Hash, ChunkLocation>,
}

/// How far a scan ([`index_dir_with_progress`]) has gotten through the tree.
/// `files_total` / `bytes_total` are fixed once the walk finishes (before any
/// chunking starts); `files_done` / `bytes_done` grow monotonically as each
/// file is read and chunked, so `bytes_done as f64 / bytes_total as f64` is a
/// stable fraction for a progress bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanProgress {
    pub files_done: usize,
    pub files_total: usize,
    pub bytes_done: u64,
    pub bytes_total: u64,
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
    scan_tree(
        root,
        name,
        version,
        |_rel, cwd| {
            store.put(cwd.chunk.hash, cwd.data);
        },
        |_| {},
    )
}

/// Like [`snapshot_dir`] but instead of storing chunk bytes it records a
/// `hash -> `[`ChunkLocation`] map (path + byte range within the source tree).
/// Feed the result to [`SourceChunkStore`](crate::SourceChunkStore) to seed a
/// large folder with only a bounded in-RAM hot-chunk cache — no second copy on
/// disk, no whole-folder buffer.
pub fn index_dir(root: &Path, name: impl Into<String>, version: u64) -> Result<IndexedSnapshot> {
    index_dir_with_progress(root, name, version, |_| {})
}

/// Like [`index_dir`], but calls `on_progress` as the walk proceeds — once
/// with the totals right after the directory walk finishes (`files_done: 0`),
/// then again after every file is fully chunked. Lets a caller show a live
/// progress bar while a large folder is being scanned.
pub fn index_dir_with_progress(
    root: &Path,
    name: impl Into<String>,
    version: u64,
    on_progress: impl FnMut(ScanProgress),
) -> Result<IndexedSnapshot> {
    let mut locations: HashMap<Hash, ChunkLocation> = HashMap::new();
    let snap = scan_tree(
        root,
        name,
        version,
        |rel, cwd| {
            locations.entry(cwd.chunk.hash).or_insert(ChunkLocation {
                path: rel.to_owned(),
                offset: cwd.chunk.offset,
                len: cwd.chunk.len,
            });
        },
        on_progress,
    )?;
    Ok(IndexedSnapshot {
        manifest: snap.manifest,
        chunk_lists: snap.chunk_lists,
        skipped: snap.skipped,
        locations,
    })
}

/// The shared walk-and-chunk pass behind [`snapshot_dir`] and [`index_dir`].
/// `on_chunk` is handed each chunk (with its bytes) as it is produced, together
/// with the owning file's manifest-relative path. `on_progress` is called once
/// up front with the totals (from a cheap metadata-only pre-pass) and then
/// again after every file finishes chunking.
fn scan_tree(
    root: &Path,
    name: impl Into<String>,
    version: u64,
    mut on_chunk: impl FnMut(&str, ChunkWithData),
    mut on_progress: impl FnMut(ScanProgress),
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

    // A metadata-only pre-pass so progress can report a stable total up front;
    // negligible next to the chunking/hashing pass below (stat, not a read).
    let files_total = files.len();
    let bytes_total: u64 =
        files.iter().filter_map(|p| fs::symlink_metadata(p).ok()).map(|m| m.len()).sum();
    on_progress(ScanProgress { files_done: 0, files_total, bytes_done: 0, bytes_total });

    // Chunk + hash every file in parallel across the rayon pool. The CPU-bound
    // work (FastCDC + BLAKE3) is what scales; the results funnel back through a
    // bounded channel to this thread, where the non-`Send` sinks (`on_chunk`,
    // `on_progress`, and whatever `store` the caller closed over) stay
    // single-threaded. The channel bound caps in-flight chunk bytes at roughly
    // `2 * threads` chunks regardless of how large the folder is, preserving the
    // "a 100 GB folder never needs 100 GB of RAM" guarantee. Files complete out
    // of walk order, so `manifest.files` is pushed unsorted and
    // `manifest.canonicalize()` below sorts it; `files_done` / `bytes_done` are
    // counted on this thread and so still only ever grow.
    let mut chunk_lists: BTreeMap<String, ChunkList> = BTreeMap::new();
    let threads = rayon::current_num_threads().max(1);
    let (tx, rx) = std::sync::mpsc::sync_channel::<ScanItem>(2 * threads);
    let files = &files;

    let produced = std::thread::scope(|scope| {
        let producer = scope.spawn(move || -> Result<()> {
            files
                .par_iter()
                .try_for_each_with(tx, |tx, path| chunk_one_file(root, path, tx))
        });

        // Drain until every sender (the seed and each rayon clone) is dropped.
        let mut files_done = 0usize;
        let mut bytes_done = 0u64;
        for item in rx {
            match item {
                ScanItem::Chunk { rel, cwd } => on_chunk(&rel, cwd),
                ScanItem::File { rel, entry, list } => {
                    bytes_done += entry.size;
                    files_done += 1;
                    manifest.files.push(entry);
                    chunk_lists.insert(rel, list);
                    on_progress(ScanProgress { files_done, files_total, bytes_done, bytes_total });
                }
            }
        }
        producer.join().expect("scan producer thread panicked")
    });
    produced?;

    manifest.canonicalize();
    manifest.validate()?;
    Ok(Snapshot { manifest, chunk_lists, skipped })
}

/// One message from a [`scan_tree`] worker to the collecting thread. `Chunk`
/// carries a chunk's bytes (for the store / location map); `File` closes a file
/// out with its finished [`FileEntry`] and [`ChunkList`]. `rel` on `Chunk` is an
/// [`Arc<str>`] so a large file's thousands of chunks share one path allocation.
enum ScanItem {
    Chunk { rel: Arc<str>, cwd: ChunkWithData },
    File { rel: String, entry: FileEntry, list: ChunkList },
}

/// Chunk one file and stream its chunks + closing [`ScanItem::File`] into `tx`.
/// Runs on a rayon worker; the bounded `tx` provides backpressure so a fast
/// worker cannot outrun the collector and buffer the whole file in the channel.
fn chunk_one_file(
    root: &Path,
    path: &Path,
    tx: &mut std::sync::mpsc::SyncSender<ScanItem>,
) -> Result<()> {
    let gone = || Error::Io(std::io::Error::other("scan collector went away"));

    let rel = rel_path(root, path)?;
    let rel_arc: Arc<str> = Arc::from(rel.as_str());
    let meta = fs::symlink_metadata(path)?;
    let size = meta.len();
    let cfg = ChunkerConfig::for_file_size(size);

    let reader = BufReader::new(File::open(path)?);
    let mut chunks = Vec::new();
    for item in chunk_reader(reader, cfg)? {
        let cwd = item?;
        chunks.push(cwd.chunk);
        tx.send(ScanItem::Chunk { rel: rel_arc.clone(), cwd }).map_err(|_| gone())?;
    }

    let list = ChunkList::from_chunks(&chunks);
    if list.total_size != size {
        return Err(Error::Verify(format!(
            "{rel}: chunked {} bytes but the file is {size}",
            list.total_size
        )));
    }
    let entry =
        FileEntry { path: rel.clone(), size, root: list.root(), mode: mode_of(&meta) };
    tx.send(ScanItem::File { rel, entry, list }).map_err(|_| gone())?;
    Ok(())
}

/// Materialize `manifest`'s files under `root`, pulling each file's bytes from
/// `store` chunk by chunk. Creates every directory in `manifest.dirs` (so empty
/// ones survive) and every file's parent. On Unix, `FileEntry::mode` is applied.
///
/// `chunk_lists` must contain an entry for every file in `manifest` (as returned
/// together by [`snapshot_dir`] or a completed download). Errors if a referenced
/// chunk is missing from `store` or a rebuilt file does not match its recorded
/// size.
pub fn write_share(
    root: &Path,
    manifest: &Manifest,
    chunk_lists: &BTreeMap<String, ChunkList>,
    store: &dyn ChunkStore,
) -> Result<()> {
    fs::create_dir_all(root)?;
    for dir in &manifest.dirs {
        fs::create_dir_all(safe_dest(root, dir)?)?;
    }

    for file in &manifest.files {
        let list = chunk_lists
            .get(&file.path)
            .ok_or_else(|| Error::Manifest(format!("no chunk list for {}", file.path)))?;
        materialize_file(root, file, list, store)?;
    }
    Ok(())
}

/// What [`sync_share`] changed on disk.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SyncOutcome {
    /// Manifest paths (re)built from the store (files added or with a new root).
    pub written: Vec<String>,
    /// Manifest paths deleted from disk because `new` no longer lists them.
    pub removed: Vec<String>,
}

impl SyncOutcome {
    /// Nothing on disk needed to change.
    pub fn is_noop(&self) -> bool {
        self.written.is_empty() && self.removed.is_empty()
    }
}

/// Bring the tree under `root` from `old` to `new`, touching only what changed.
///
/// This is the delta-sync counterpart of [`write_share`]. It uses
/// [`Manifest::diff`]: files that are new or whose Merkle root changed are
/// rebuilt from `store` exactly as [`write_share`] would; files listed by `old`
/// but not `new` are deleted; unchanged files are left untouched and their bytes
/// are never read. Directories in `new.dirs` are created; directories `old`
/// listed that `new` does not are removed when empty (deepest first).
///
/// `store` must hold every chunk of every file that needs rebuilding — top it up
/// with a swarm download first. `new_chunk_lists` must have an entry for every
/// added/changed file (as returned together by a download). `old` must describe
/// what is actually on disk, or unchanged-but-stale files will be missed.
pub fn sync_share(
    root: &Path,
    old: &Manifest,
    new: &Manifest,
    new_chunk_lists: &BTreeMap<String, ChunkList>,
    store: &dyn ChunkStore,
) -> Result<SyncOutcome> {
    fs::create_dir_all(root)?;
    for dir in &new.dirs {
        fs::create_dir_all(safe_dest(root, dir)?)?;
    }

    let diff = Manifest::diff(old, new);
    let mut outcome = SyncOutcome::default();

    // Added + changed: rebuild from the (topped-up) store.
    let rebuild = diff.added.iter().copied().chain(diff.changed.iter().map(|(_, n)| *n));
    for file in rebuild {
        let list = new_chunk_lists
            .get(&file.path)
            .ok_or_else(|| Error::Manifest(format!("no chunk list for {}", file.path)))?;
        materialize_file(root, file, list, store)?;
        outcome.written.push(file.path.clone());
    }

    // Removed files. Resolve through `safe_dest` first so a symlinked *parent*
    // component planted in the tree can't redirect the unlink outside `root`,
    // then only unlink a real file (never a symlink) at the leaf itself.
    for file in &diff.removed {
        let Ok(dest) = safe_dest(root, &file.path) else {
            continue;
        };
        match fs::symlink_metadata(&dest) {
            Ok(md) if md.file_type().is_file() => {
                fs::remove_file(&dest)?;
                outcome.removed.push(file.path.clone());
            }
            Ok(_) => {} // a dir or a symlink — leave it
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(Error::Io(e)),
        }
    }

    // Directories `old` had that `new` no longer lists — deepest first, only if
    // now empty (best effort).
    let new_dirs: BTreeSet<&str> = new.dirs.iter().map(String::as_str).collect();
    let mut gone: Vec<&str> =
        old.dirs.iter().map(String::as_str).filter(|d| !new_dirs.contains(d)).collect();
    gone.sort_by(|a, b| b.cmp(a));
    for dir in gone {
        let _ = fs::remove_dir(root.join(dir));
    }

    outcome.written.sort();
    outcome.removed.sort();
    Ok(outcome)
}

/// Rebuild one file under `root` from `store`, verifying every chunk and the
/// final size. Shared by [`write_share`] and [`sync_share`].
fn materialize_file(
    root: &Path,
    file: &FileEntry,
    list: &ChunkList,
    store: &dyn ChunkStore,
) -> Result<()> {
    let dest = safe_dest(root, &file.path)?;
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    // A pre-existing symlink where the file goes would be followed by
    // `File::create`; remove the link itself (not its target) and write fresh.
    if let Ok(md) = fs::symlink_metadata(&dest)
        && md.file_type().is_symlink()
    {
        fs::remove_file(&dest)?;
    }

    let mut written = 0u64;
    let mut out = BufWriter::new(File::create(&dest)?);
    for chunk in &list.chunks {
        let bytes = store
            .get(&chunk.hash)
            .ok_or_else(|| Error::Verify(format!("{}: missing chunk {}", file.path, chunk.hash)))?;
        if Hash::of(&bytes) != chunk.hash {
            return Err(Error::Verify(format!(
                "{}: chunk {} in the store hashes wrong",
                file.path, chunk.hash
            )));
        }
        out.write_all(&bytes)?;
        written += bytes.len() as u64;
    }
    out.flush()?;
    drop(out);

    if written != file.size {
        return Err(Error::Verify(format!(
            "{}: rebuilt {written} bytes but the manifest says {}",
            file.path, file.size
        )));
    }
    apply_mode(&dest, file.mode)?;
    Ok(())
}

#[cfg(unix)]
fn apply_mode(path: &Path, mode: Option<u32>) -> Result<()> {
    if let Some(mode) = mode {
        use std::os::unix::fs::PermissionsExt;
        // Permission bits only. Never honour setuid/setgid/sticky from a
        // manifest we did not produce — a downloaded share must not be able to
        // drop a setuid file into the download tree.
        fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o777))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn apply_mode(_path: &Path, _mode: Option<u32>) -> Result<()> {
    Ok(())
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

/// Resolve `root`-relative `rel` to an absolute path, rejecting it if any
/// existing path component (a parent dir, or the leaf) is a symlink.
///
/// `rel` has already passed [`Manifest::validate`](crate::Manifest) so it cannot
/// contain `..` or an absolute prefix; this closes the *physical* escape a
/// planted symlink in the output tree would otherwise open (`create_dir_all` and
/// `File::create` both follow links). Costs a handful of `lstat`s per file —
/// nothing beside writing the file's bytes.
fn safe_dest(root: &Path, rel: &str) -> Result<PathBuf> {
    let mut cur = root.to_path_buf();
    for comp in rel.split('/') {
        cur.push(comp);
        match fs::symlink_metadata(&cur) {
            Ok(md) if md.file_type().is_symlink() => {
                return Err(Error::Verify(format!(
                    "refusing to follow a symlink at {} while materializing {rel}",
                    cur.display()
                )));
            }
            Ok(_) => {}
            // Nothing exists here yet, so nothing deeper can be a link either.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => return Err(Error::Io(e)),
        }
    }
    Ok(root.join(rel))
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
