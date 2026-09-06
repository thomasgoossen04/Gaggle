//! Content-addressed chunk storage and dedup accounting.
//!
//! Dedup is not a separate pass — it falls out of content addressing: [`put`]
//! keys on `blake3(data)`, so a chunk that already exists (a shared DLL across
//! two modded installs, an unchanged region of an updated archive) is stored
//! once no matter how many files or folders reference it.
//!
//! [`put`]: ChunkStore::put
//!
//! Three implementations ship:
//!
//! - [`MemoryChunkStore`] — a plain in-RAM map with dedup accounting. What a
//!   normal peer downloads into.
//! - [`LruChunkCache`] — a byte-budgeted in-RAM cache that evicts the
//!   least-recently-used chunk when full. The relay accelerator's hot-chunk
//!   cache: high bandwidth, deliberately small storage.
//! - [`DiskChunkStore`] — a durable content-addressed store on the filesystem,
//!   one file per chunk, sharded by hash prefix. The cache/NAS accelerator's
//!   full replica: survives restarts, resumes partial fills.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::hash::Hash;

/// A content-addressed store of chunk bytes.
pub trait ChunkStore {
    fn contains(&self, hash: &Hash) -> bool;

    fn get(&self, hash: &Hash) -> Option<Vec<u8>>;

    /// Insert a chunk. Returns `true` if it was newly stored, `false` if an
    /// identical chunk was already present. Implementations may assume
    /// `hash == blake3(data)`.
    fn put(&mut self, hash: Hash, data: Vec<u8>) -> bool;

    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// In-memory [`ChunkStore`] with dedup accounting — what a normal peer
/// downloads into. See [`LruChunkCache`] and [`DiskChunkStore`] for the
/// accelerator-side stores.
#[derive(Debug, Default)]
pub struct MemoryChunkStore {
    chunks: HashMap<Hash, Vec<u8>>,
    unique_bytes: u64,
    duplicate_bytes: u64,
    puts: u64,
}

impl MemoryChunkStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn stats(&self) -> DedupStats {
        DedupStats {
            unique_chunks: self.chunks.len() as u64,
            unique_bytes: self.unique_bytes,
            duplicate_bytes: self.duplicate_bytes,
            logical_bytes: self.unique_bytes + self.duplicate_bytes,
            total_puts: self.puts,
        }
    }
}

impl ChunkStore for MemoryChunkStore {
    fn contains(&self, hash: &Hash) -> bool {
        self.chunks.contains_key(hash)
    }

    fn get(&self, hash: &Hash) -> Option<Vec<u8>> {
        self.chunks.get(hash).cloned()
    }

    fn put(&mut self, hash: Hash, data: Vec<u8>) -> bool {
        debug_assert_eq!(Hash::of(&data), hash, "put() called with a mismatched hash");
        self.puts += 1;
        let n = data.len() as u64;
        if self.chunks.contains_key(&hash) {
            self.duplicate_bytes += n;
            false
        } else {
            self.unique_bytes += n;
            self.chunks.insert(hash, data);
            true
        }
    }

    fn len(&self) -> usize {
        self.chunks.len()
    }
}

/// Snapshot of how much a [`MemoryChunkStore`] has absorbed and how much of that
/// was eliminated by dedup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DedupStats {
    pub unique_chunks: u64,
    /// Bytes actually stored.
    pub unique_bytes: u64,
    /// Bytes offered to [`put`](ChunkStore::put) that were already present.
    pub duplicate_bytes: u64,
    /// `unique_bytes + duplicate_bytes` — the total logical size referenced.
    pub logical_bytes: u64,
    pub total_puts: u64,
}

impl DedupStats {
    /// Fraction of referenced bytes that dedup removed, in `[0.0, 1.0]`.
    pub fn dedup_ratio(&self) -> f64 {
        if self.logical_bytes == 0 {
            0.0
        } else {
            self.duplicate_bytes as f64 / self.logical_bytes as f64
        }
    }
}

/// A byte-budgeted, in-memory chunk cache with least-recently-used eviction.
///
/// This is the relay accelerator's hot-chunk cache: a node with
/// lots of bandwidth but little storage keeps only the chunks that are being
/// asked for right now, and re-fetches a cold chunk from upstream if it comes
/// back into demand. [`get`](ChunkStore::get) and [`contains`](ChunkStore::contains)
/// count as a use and refresh a chunk's recency; [`put`](ChunkStore::put)
/// evicts the coldest chunks until the newcomer fits.
///
/// A chunk larger than the whole budget is refused (`put` returns `false`
/// without evicting anything).
#[derive(Debug)]
pub struct LruChunkCache {
    capacity: u64,
    used: u64,
    clock: u64,
    /// hash -> (bytes, last-use tick)
    entries: HashMap<Hash, (Vec<u8>, u64)>,
    /// last-use tick -> hash, for O(log n) coldest-first eviction
    by_age: BTreeMap<u64, Hash>,
    hits: u64,
    misses: u64,
    evictions: u64,
}

impl LruChunkCache {
    /// A cache that will hold at most `capacity_bytes` of chunk data.
    pub fn new(capacity_bytes: u64) -> Self {
        Self {
            capacity: capacity_bytes,
            used: 0,
            clock: 0,
            entries: HashMap::new(),
            by_age: BTreeMap::new(),
            hits: 0,
            misses: 0,
            evictions: 0,
        }
    }

    /// Bytes of chunk data currently held. Always `<= capacity`.
    pub fn used_bytes(&self) -> u64 {
        self.used
    }

    pub fn capacity_bytes(&self) -> u64 {
        self.capacity
    }

    pub fn stats(&self) -> CacheStats {
        CacheStats {
            chunks: self.entries.len() as u64,
            used_bytes: self.used,
            capacity_bytes: self.capacity,
            hits: self.hits,
            misses: self.misses,
            evictions: self.evictions,
            bytes_served: 0,
        }
    }

    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    /// Move `hash` to the most-recently-used position. Caller guarantees it is
    /// present.
    fn touch(&mut self, hash: &Hash) {
        let now = self.tick();
        let (_, last) = self.entries.get_mut(hash).expect("touch() on an absent chunk");
        self.by_age.remove(last);
        *last = now;
        self.by_age.insert(now, *hash);
    }

    fn evict_one(&mut self) -> bool {
        let Some((&age, &hash)) = self.by_age.iter().next() else {
            return false;
        };
        self.by_age.remove(&age);
        if let Some((data, _)) = self.entries.remove(&hash) {
            self.used -= data.len() as u64;
            self.evictions += 1;
        }
        true
    }
}

impl ChunkStore for LruChunkCache {
    fn contains(&self, hash: &Hash) -> bool {
        self.entries.contains_key(hash)
    }

    fn get(&self, hash: &Hash) -> Option<Vec<u8>> {
        // `get` takes `&self`; recency is refreshed on the `&mut` paths
        // (`get_refreshing`, `put`). A plain read still counts as a hit/miss.
        self.entries.get(hash).map(|(d, _)| d.clone())
    }

    fn put(&mut self, hash: Hash, data: Vec<u8>) -> bool {
        debug_assert_eq!(Hash::of(&data), hash, "put() called with a mismatched hash");
        let n = data.len() as u64;
        if self.entries.contains_key(&hash) {
            self.touch(&hash);
            return false;
        }
        if n > self.capacity {
            return false; // never going to fit; don't thrash the cache for it
        }
        while self.used + n > self.capacity {
            if !self.evict_one() {
                break;
            }
        }
        let now = self.tick();
        self.entries.insert(hash, (data, now));
        self.by_age.insert(now, hash);
        self.used += n;
        true
    }

    fn len(&self) -> usize {
        self.entries.len()
    }
}

impl LruChunkCache {
    /// Like [`ChunkStore::get`] but refreshes the chunk's recency and updates
    /// hit/miss counters — the accelerator's serving path calls this.
    pub fn get_refreshing(&mut self, hash: &Hash) -> Option<Vec<u8>> {
        if self.entries.contains_key(hash) {
            self.touch(hash);
            self.hits += 1;
            Some(self.entries[hash].0.clone())
        } else {
            self.misses += 1;
            None
        }
    }
}

/// Snapshot of an [`LruChunkCache`]'s occupancy and hit rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CacheStats {
    pub chunks: u64,
    pub used_bytes: u64,
    pub capacity_bytes: u64,
    /// [`get_refreshing`](LruChunkCache::get_refreshing) calls that hit.
    pub hits: u64,
    /// [`get_refreshing`](LruChunkCache::get_refreshing) calls that missed.
    pub misses: u64,
    pub evictions: u64,
    /// Cumulative bytes actually handed to downloaders — distinct from
    /// `used_bytes` (the cache's own footprint). The cache does not count this
    /// itself; a serving layer in front of it (the relay) fills it in on the
    /// [`stats`](LruChunkCache::stats) it returns.
    pub bytes_served: u64,
}

/// zstd level used when [`DiskChunkStore`] writes a compressed chunk. Level 3
/// is zstd's own default — a good ratio/speed balance for a replica that is
/// written once and read many times.
pub const DISK_ZSTD_LEVEL: i32 = 3;

/// A durable content-addressed chunk store on the filesystem.
///
/// One file per chunk, sharded into 256 directories by the first hash byte so no
/// single directory holds the whole store. A chunk is stored either raw (file
/// named by lowercase-hex hash) or zstd-compressed (same name + `.zst`); the
/// store is self-describing per chunk, so a store written raw and reopened
/// compressed (or vice versa) keeps reading every chunk and only *new* chunks
/// take the current form — no migration. Compression is opt-in
/// ([`open_with_opts`](Self::open_with_opts)) and only kept for a chunk when it
/// actually shrinks it, so already-compressed game assets stay raw.
///
/// This is the cache/NAS accelerator's replica backing: it survives a restart,
/// and because [`put`](ChunkStore::put) skips chunks already on disk a
/// half-finished replication just resumes.
///
/// The [`ChunkStore`] impl is infallible by contract, so a read/write I/O error
/// surfaces as `None` / `false` and bumps [`io_errors`](Self::io_errors); the
/// [`try_get`](Self::try_get) / [`try_put`](Self::try_put) methods expose the
/// underlying [`io::Result`] for callers that need it.
#[derive(Debug)]
pub struct DiskChunkStore {
    root: PathBuf,
    /// hash → is this chunk stored zstd-compressed (a `.zst` file)?
    index: HashMap<Hash, bool>,
    /// Whether *new* chunks are written compressed (when that shrinks them).
    compress: bool,
    tmp_counter: AtomicU64,
    io_errors: AtomicU64,
}

impl DiskChunkStore {
    /// Open (creating if needed) a store rooted at `dir` that writes chunks
    /// raw, indexing whatever chunks (raw or `.zst`) are already there.
    pub fn open(dir: impl AsRef<Path>) -> io::Result<Self> {
        Self::open_with_opts(dir, false)
    }

    /// [`open`](Self::open), but `compress` controls whether newly written
    /// chunks are zstd-compressed when that makes them smaller. Existing chunks
    /// are read back in whatever form they were stored regardless.
    pub fn open_with_opts(dir: impl AsRef<Path>, compress: bool) -> io::Result<Self> {
        let root = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&root)?;
        let mut index = HashMap::new();
        for shard in std::fs::read_dir(&root)? {
            let shard = shard?;
            if !shard.file_type()?.is_dir() {
                continue;
            }
            for entry in std::fs::read_dir(shard.path())? {
                let name = entry?.file_name();
                let Some(name) = name.to_str() else { continue };
                let (stem, compressed) = match name.strip_suffix(".zst") {
                    Some(s) => (s, true),
                    None => (name, false),
                };
                if let Ok(hash) = Hash::from_hex(stem) {
                    index.insert(hash, compressed);
                }
            }
        }
        Ok(Self {
            root,
            index,
            compress,
            tmp_counter: AtomicU64::new(0),
            io_errors: AtomicU64::new(0),
        })
    }

    /// How many `ChunkStore` operations have swallowed an I/O error.
    pub fn io_errors(&self) -> u64 {
        self.io_errors.load(Ordering::Relaxed)
    }

    /// Total bytes of chunk files on disk (a directory walk; not cached). With
    /// compression on this is the *compressed* footprint — the point of the
    /// feature.
    pub fn size_on_disk(&self) -> io::Result<u64> {
        let mut total = 0;
        for (hash, &compressed) in &self.index {
            total += std::fs::metadata(self.path_for_form(hash, compressed))?.len();
        }
        Ok(total)
    }

    fn path_for(&self, hash: &Hash) -> PathBuf {
        let hex = hash.to_hex();
        self.root.join(&hex[..2]).join(hex)
    }

    /// The on-disk path for a chunk in a given form (`.zst` suffix when
    /// compressed).
    fn path_for_form(&self, hash: &Hash, compressed: bool) -> PathBuf {
        let p = self.path_for(hash);
        if compressed { p.with_extension("zst") } else { p }
    }

    /// Read a chunk, distinguishing "absent" (`Ok(None)`) from an I/O error.
    pub fn try_get(&self, hash: &Hash) -> io::Result<Option<Vec<u8>>> {
        let Some(&compressed) = self.index.get(hash) else {
            return Ok(None);
        };
        let bytes = match std::fs::read(self.path_for_form(hash, compressed)) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        if compressed {
            zstd::decode_all(&bytes[..]).map(Some)
        } else {
            Ok(Some(bytes))
        }
    }

    /// Write a chunk (atomically: temp file + rename). `Ok(false)` means it was
    /// already present. Compressed with zstd when the store was opened with
    /// compression on *and* that shrinks the chunk.
    pub fn try_put(&mut self, hash: Hash, data: &[u8]) -> io::Result<bool> {
        debug_assert_eq!(Hash::of(data), hash, "try_put() called with a mismatched hash");
        if self.index.contains_key(&hash) {
            return Ok(false);
        }
        let (payload, compressed): (Cow<[u8]>, bool) = if self.compress {
            match zstd::encode_all(data, DISK_ZSTD_LEVEL) {
                Ok(z) if z.len() < data.len() => (Cow::Owned(z), true),
                _ => (Cow::Borrowed(data), false),
            }
        } else {
            (Cow::Borrowed(data), false)
        };
        let final_path = self.path_for_form(&hash, compressed);
        let shard = final_path.parent().expect("path_for always has a shard parent");
        std::fs::create_dir_all(shard)?;
        let n = self.tmp_counter.fetch_add(1, Ordering::Relaxed);
        let tmp = shard.join(format!(".{}.{n}.tmp", hash.to_hex()));
        std::fs::write(&tmp, &payload)?;
        match std::fs::rename(&tmp, &final_path) {
            Ok(()) => {
                self.index.insert(hash, compressed);
                Ok(true)
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                Err(e)
            }
        }
    }
}

impl ChunkStore for DiskChunkStore {
    fn contains(&self, hash: &Hash) -> bool {
        self.index.contains_key(hash)
    }

    fn get(&self, hash: &Hash) -> Option<Vec<u8>> {
        match self.try_get(hash) {
            Ok(v) => v,
            Err(_) => {
                self.io_errors.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    fn put(&mut self, hash: Hash, data: Vec<u8>) -> bool {
        match self.try_put(hash, &data) {
            Ok(newly) => newly,
            Err(_) => {
                self.io_errors.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    fn len(&self) -> usize {
        self.index.len()
    }
}

impl DiskChunkStore {
    /// How many stored chunks are held zstd-compressed on disk.
    pub fn compressed_chunks(&self) -> usize {
        self.index.values().filter(|&&c| c).count()
    }
}

/// A cloneable handle to one shared [`ChunkStore`]. Every clone locks and
/// delegates to the same inner store, so an in-progress download can keep
/// writing landed chunks through one handle while a serving `Catalog` built
/// from another answers `GetChunk` for the chunks that have already arrived —
/// the way a still-downloading peer uploads the part of a share it holds.
///
/// Reads ([`contains`](ChunkStore::contains) / [`get`](ChunkStore::get) /
/// [`len`](ChunkStore::len)) take a shared lock and run concurrently;
/// [`put`](ChunkStore::put) takes the exclusive lock for the brief insert. The
/// inner store's own work still happens under that lock, so this fits stores
/// whose ops are short (a map insert, one chunk-file read or write) — not a
/// whole-share pass.
pub struct SharedChunkStore<S> {
    inner: Arc<RwLock<S>>,
}

impl<S> Clone for SharedChunkStore<S> {
    fn clone(&self) -> Self {
        Self { inner: Arc::clone(&self.inner) }
    }
}

impl<S> SharedChunkStore<S> {
    pub fn new(store: S) -> Self {
        Self { inner: Arc::new(RwLock::new(store)) }
    }

    /// Recover the inner store once every other handle (and every `Catalog`
    /// built from a clone) has been dropped; hands the handle back unchanged if
    /// other clones are still live.
    pub fn into_inner(self) -> Result<S, Self> {
        Arc::try_unwrap(self.inner)
            .map(|lock| lock.into_inner().unwrap_or_else(|e| e.into_inner()))
            .map_err(|inner| Self { inner })
    }
}

impl<S: ChunkStore> ChunkStore for SharedChunkStore<S> {
    fn contains(&self, hash: &Hash) -> bool {
        self.inner.read().unwrap_or_else(|e| e.into_inner()).contains(hash)
    }

    fn get(&self, hash: &Hash) -> Option<Vec<u8>> {
        self.inner.read().unwrap_or_else(|e| e.into_inner()).get(hash)
    }

    fn put(&mut self, hash: Hash, data: Vec<u8>) -> bool {
        self.inner.write().unwrap_or_else(|e| e.into_inner()).put(hash, data)
    }

    fn len(&self) -> usize {
        self.inner.read().unwrap_or_else(|e| e.into_inner()).len()
    }
}

/// Where a chunk's bytes live inside a source tree: a file path relative to the
/// share root plus the byte range. Built by [`index_dir`](crate::index_dir) and
/// consumed by [`SourceChunkStore`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkLocation {
    /// `/`-separated path relative to the share root.
    pub path: String,
    pub offset: u64,
    pub len: u32,
}

/// A [`ChunkStore`] that keeps **no** durable copy of a share: it reads each
/// requested chunk straight from the original files under `root`, verifies it
/// against its content address, and holds it in a small byte-budgeted
/// [`LruChunkCache`] so a burst of requests for the same chunk costs one disk
/// read. Nothing is written to disk and RAM use is capped at the budget (plus
/// the location index).
///
/// This is what a peer uses to *seed a folder it already has*: a 100 GB install
/// is served with a few hundred MB of cache, not 100 GB of RAM and not a second
/// on-disk copy. [`put`](ChunkStore::put) is a no-op — the store only ever
/// serves what [`index_dir`](crate::index_dir) found. A `get` whose source
/// bytes no longer hash as expected (the file changed since the scan) returns
/// `None`; a fresh `index_dir` fixes it.
#[derive(Debug)]
pub struct SourceChunkStore {
    root: PathBuf,
    locations: HashMap<Hash, ChunkLocation>,
    cache: Mutex<LruChunkCache>,
    disk_reads: AtomicU64,
    failed_reads: AtomicU64,
}

impl SourceChunkStore {
    /// Smallest RAM budget honoured. The chunker tops out at a 16 MiB chunk
    /// ([`ChunkerConfig::HUGE`](crate::ChunkerConfig)), and [`LruChunkCache`]
    /// refuses a chunk larger than its whole budget, so the floor leaves
    /// headroom for the largest possible chunk to always be cacheable.
    pub const MIN_BUDGET_BYTES: u64 = 32 * 1024 * 1024;

    /// `root` is the share's source folder; `locations` comes from
    /// [`index_dir`](crate::index_dir); `ram_budget_bytes` is clamped up to
    /// [`MIN_BUDGET_BYTES`](Self::MIN_BUDGET_BYTES).
    pub fn new(
        root: impl Into<PathBuf>,
        locations: HashMap<Hash, ChunkLocation>,
        ram_budget_bytes: u64,
    ) -> Self {
        Self {
            root: root.into(),
            locations,
            cache: Mutex::new(LruChunkCache::new(ram_budget_bytes.max(Self::MIN_BUDGET_BYTES))),
            disk_reads: AtomicU64::new(0),
            failed_reads: AtomicU64::new(0),
        }
    }

    /// Occupancy + hit rate of the hot-chunk cache.
    pub fn cache_stats(&self) -> CacheStats {
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).stats()
    }

    /// Chunks served by actually reading the source files (i.e. cache misses).
    pub fn disk_reads(&self) -> u64 {
        self.disk_reads.load(Ordering::Relaxed)
    }

    /// `get`s that could not be served because the source file was unreadable
    /// or no longer held the expected bytes (it changed since the scan).
    pub fn failed_reads(&self) -> u64 {
        self.failed_reads.load(Ordering::Relaxed)
    }

    fn read_range(&self, loc: &ChunkLocation) -> io::Result<Vec<u8>> {
        let mut file = File::open(self.root.join(&loc.path))?;
        file.seek(SeekFrom::Start(loc.offset))?;
        let mut buf = vec![0u8; loc.len as usize];
        file.read_exact(&mut buf)?;
        Ok(buf)
    }
}

impl ChunkStore for SourceChunkStore {
    fn contains(&self, hash: &Hash) -> bool {
        self.locations.contains_key(hash)
    }

    fn get(&self, hash: &Hash) -> Option<Vec<u8>> {
        if let Some(hit) =
            self.cache.lock().unwrap_or_else(|e| e.into_inner()).get_refreshing(hash)
        {
            return Some(hit);
        }
        let loc = self.locations.get(hash)?;
        let bytes = match self.read_range(loc) {
            Ok(b) if Hash::of(&b) == *hash => b,
            _ => {
                // Unreadable, or the source file changed since the scan — never
                // serve bytes that don't match the requested content address.
                self.failed_reads.fetch_add(1, Ordering::Relaxed);
                return None;
            }
        };
        self.disk_reads.fetch_add(1, Ordering::Relaxed);
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .put(*hash, bytes.clone());
        Some(bytes)
    }

    /// A streaming seed never absorbs chunks — it only serves what it indexed.
    fn put(&mut self, _hash: Hash, _data: Vec<u8>) -> bool {
        false
    }

    fn len(&self) -> usize {
        self.locations.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(bytes: &[u8]) -> (Hash, Vec<u8>) {
        (Hash::of(bytes), bytes.to_vec())
    }

    #[test]
    fn put_reports_new_vs_duplicate() {
        let mut store = MemoryChunkStore::new();
        let (h, d) = chunk(b"hello world");

        assert!(store.put(h, d.clone()));
        assert!(!store.put(h, d.clone()));
        assert_eq!(store.len(), 1);
        assert!(store.contains(&h));
        assert_eq!(store.get(&h).as_deref(), Some(&b"hello world"[..]));
    }

    #[test]
    fn stats_track_dedup() {
        let mut store = MemoryChunkStore::new();
        let (h1, d1) = chunk(&[1u8; 1000]);
        let (h2, d2) = chunk(&[2u8; 500]);

        store.put(h1, d1.clone());
        store.put(h2, d2);
        store.put(h1, d1); // duplicate 1000 bytes

        let s = store.stats();
        assert_eq!(s.unique_chunks, 2);
        assert_eq!(s.unique_bytes, 1500);
        assert_eq!(s.duplicate_bytes, 1000);
        assert_eq!(s.logical_bytes, 2500);
        assert_eq!(s.total_puts, 3);
        assert!((s.dedup_ratio() - 0.4).abs() < 1e-9);
    }

    #[test]
    fn empty_store_ratio_is_zero() {
        assert_eq!(MemoryChunkStore::new().stats().dedup_ratio(), 0.0);
    }

    // --- LruChunkCache -----------------------------------------------------

    #[test]
    fn lru_evicts_the_coldest_chunk_when_full() {
        let (h1, d1) = chunk(&[1u8; 100]);
        let (h2, d2) = chunk(&[2u8; 100]);
        let (h3, d3) = chunk(&[3u8; 100]);

        let mut cache = LruChunkCache::new(250); // room for two 100-byte chunks
        assert!(cache.put(h1, d1));
        assert!(cache.put(h2, d2));
        assert_eq!(cache.used_bytes(), 200);

        // Touch h1 so h2 is now the least-recently-used.
        assert_eq!(cache.get_refreshing(&h1).as_deref(), Some(&[1u8; 100][..]));

        assert!(cache.put(h3, d3));
        assert!(cache.contains(&h1), "h1 was refreshed, should survive");
        assert!(!cache.contains(&h2), "h2 was coldest, should be evicted");
        assert!(cache.contains(&h3));
        assert_eq!(cache.used_bytes(), 200);
        assert_eq!(cache.stats().evictions, 1);
    }

    #[test]
    fn lru_refuses_a_chunk_larger_than_the_budget() {
        let (h_big, d_big) = chunk(&[7u8; 500]);
        let (h_ok, d_ok) = chunk(&[8u8; 50]);
        let mut cache = LruChunkCache::new(100);

        assert!(cache.put(h_ok, d_ok));
        assert!(!cache.put(h_big, d_big), "oversized chunk is refused");
        assert!(cache.contains(&h_ok), "and does not thrash out what already fit");
        assert_eq!(cache.used_bytes(), 50);
    }

    #[test]
    fn lru_tracks_hits_and_misses() {
        let (h, d) = chunk(b"hot");
        let mut cache = LruChunkCache::new(1024);
        assert!(cache.get_refreshing(&h).is_none());
        cache.put(h, d);
        assert!(cache.get_refreshing(&h).is_some());
        assert!(cache.get_refreshing(&h).is_some());
        let s = cache.stats();
        assert_eq!((s.hits, s.misses), (2, 1));
    }

    #[test]
    fn lru_put_of_present_chunk_refreshes_without_growing() {
        let (h1, d1) = chunk(&[1u8; 100]);
        let (h2, d2) = chunk(&[2u8; 100]);
        let (h3, d3) = chunk(&[3u8; 100]);
        let mut cache = LruChunkCache::new(250);
        cache.put(h1, d1.clone());
        cache.put(h2, d2);
        assert!(!cache.put(h1, d1), "re-put is a no-op returning false");
        // h1 is now freshest; adding h3 evicts h2.
        cache.put(h3, d3);
        assert!(cache.contains(&h1));
        assert!(!cache.contains(&h2));
    }

    // --- DiskChunkStore --------------------------------------------------

    #[test]
    fn disk_store_round_trips_and_dedups() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = DiskChunkStore::open(dir.path()).unwrap();
        let (h, d) = chunk(b"durable bytes");

        assert!(store.put(h, d.clone()));
        assert!(!store.put(h, d.clone()), "second put is a dedup no-op");
        assert!(store.contains(&h));
        assert_eq!(store.len(), 1);
        assert_eq!(store.get(&h).as_deref(), Some(&b"durable bytes"[..]));
        assert!(store.get(&Hash::of(b"absent")).is_none());
    }

    #[test]
    fn disk_store_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let (h1, d1) = chunk(b"one");
        let (h2, d2) = chunk(&[9u8; 4096]);

        {
            let mut store = DiskChunkStore::open(dir.path()).unwrap();
            store.put(h1, d1.clone());
            store.put(h2, d2.clone());
        }

        let reopened = DiskChunkStore::open(dir.path()).unwrap();
        assert_eq!(reopened.len(), 2);
        assert!(reopened.contains(&h1) && reopened.contains(&h2));
        assert_eq!(reopened.get(&h1).as_deref(), Some(&b"one"[..]));
        assert_eq!(reopened.get(&h2), Some(d2));
        assert_eq!(reopened.io_errors(), 0);
    }

    #[test]
    fn disk_store_shards_by_hash_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = DiskChunkStore::open(dir.path()).unwrap();
        let (h, d) = chunk(b"shard me");
        store.put(h, d).then_some(()).unwrap();

        let shard = dir.path().join(&h.to_hex()[..2]);
        assert!(shard.join(h.to_hex()).is_file(), "chunk lands in its prefix shard");
    }

    #[test]
    fn shared_store_reads_writes_through_one_backing_store() {
        let mut writer = SharedChunkStore::new(MemoryChunkStore::new());
        let reader = writer.clone();
        let (h, d) = chunk(b"shared");

        assert!(writer.put(h, d.clone()));
        // The clone sees the write immediately — same backing store.
        assert!(reader.contains(&h));
        assert_eq!(reader.get(&h), Some(d));
        assert_eq!(reader.len(), 1);

        // The inner store can only be recovered once every clone is gone.
        let writer = match writer.into_inner() {
            Err(handle) => handle,
            Ok(_) => panic!("into_inner must fail while a clone is live"),
        };
        drop(reader);
        assert_eq!(writer.into_inner().ok().expect("sole owner now").len(), 1);
    }

    #[test]
    fn disk_store_try_put_reports_new_then_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = DiskChunkStore::open(dir.path()).unwrap();
        let (h, d) = chunk(b"x");
        assert!(store.try_put(h, &d).unwrap());
        assert!(!store.try_put(h, &d).unwrap());
    }

    #[test]
    fn disk_store_compresses_when_it_shrinks_the_chunk() {
        let dir = tempfile::tempdir().unwrap();
        // Highly compressible payload, well above the store's raw size.
        let (h_squish, d_squish) = chunk(&[0u8; 64 * 1024]);
        // Random payload zstd can't shrink.
        let mut noise = vec![0u8; 4096];
        getrandom::getrandom(&mut noise).unwrap();
        let (h_noise, d_noise) = chunk(&noise);

        {
            let mut store = DiskChunkStore::open_with_opts(dir.path(), true).unwrap();
            assert!(store.try_put(h_squish, &d_squish).unwrap());
            assert!(store.try_put(h_noise, &d_noise).unwrap());
            assert_eq!(store.compressed_chunks(), 1, "only the squishable chunk is stored .zst");
            assert!(store.size_on_disk().unwrap() < (d_squish.len() + d_noise.len()) as u64);
            // Reads decompress transparently.
            assert_eq!(store.get(&h_squish), Some(d_squish.clone()));
            assert_eq!(store.get(&h_noise), Some(d_noise.clone()));
        }

        // The .zst lands in its prefix shard; the noise chunk stays raw.
        let squish_shard = dir.path().join(&h_squish.to_hex()[..2]);
        assert!(squish_shard.join(format!("{}.zst", h_squish.to_hex())).is_file());
        let noise_shard = dir.path().join(&h_noise.to_hex()[..2]);
        assert!(noise_shard.join(h_noise.to_hex()).is_file());
    }

    #[test]
    fn disk_store_reopen_reads_mixed_forms_and_keeps_going() {
        let dir = tempfile::tempdir().unwrap();
        let (h_raw, d_raw) = chunk(b"written before compression was turned on");
        let (h_zst, d_zst) = chunk(&[7u8; 32 * 1024]);

        {
            let mut store = DiskChunkStore::open(dir.path()).unwrap(); // raw
            store.try_put(h_raw, &d_raw).unwrap();
        }
        {
            // Reopen with compression on: the old raw chunk still reads, the
            // new one is stored compressed.
            let mut store = DiskChunkStore::open_with_opts(dir.path(), true).unwrap();
            assert_eq!(store.len(), 1);
            assert_eq!(store.get(&h_raw), Some(d_raw.clone()));
            store.try_put(h_zst, &d_zst).unwrap();
        }

        let reopened = DiskChunkStore::open_with_opts(dir.path(), true).unwrap();
        assert_eq!(reopened.len(), 2);
        assert_eq!(reopened.compressed_chunks(), 1);
        assert_eq!(reopened.get(&h_raw), Some(d_raw));
        assert_eq!(reopened.get(&h_zst), Some(d_zst));
        assert_eq!(reopened.io_errors(), 0);
    }

    // --- SourceChunkStore ----------------------------------------------------

    fn locmap(entries: &[(Hash, &str, u64, u32)]) -> HashMap<Hash, ChunkLocation> {
        entries
            .iter()
            .map(|(h, p, off, len)| {
                (*h, ChunkLocation { path: (*p).to_string(), offset: *off, len: *len })
            })
            .collect()
    }

    #[test]
    fn source_store_reads_ranges_from_the_tree_and_caches_them() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.bin"), b"hello world, this is a file").unwrap();
        let (h_hello, _) = chunk(b"hello ");
        let (h_rest, _) = chunk(b"world, this is a file");

        let store = SourceChunkStore::new(
            dir.path(),
            locmap(&[(h_hello, "a.bin", 0, 6), (h_rest, "a.bin", 6, 21)]),
            0, // clamped up to MIN_BUDGET_BYTES
        );

        assert!(store.contains(&h_hello));
        assert_eq!(store.get(&h_hello).as_deref(), Some(&b"hello "[..]));
        assert_eq!(store.get(&h_rest).as_deref(), Some(&b"world, this is a file"[..]));
        assert_eq!(store.disk_reads(), 2);

        // Second read is a cache hit — no extra disk read.
        assert_eq!(store.get(&h_hello).as_deref(), Some(&b"hello "[..]));
        assert_eq!(store.disk_reads(), 2);
        assert!(store.cache_stats().hits >= 1);
    }

    #[test]
    fn source_store_put_is_a_noop_and_unknown_hashes_miss() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SourceChunkStore::new(dir.path(), HashMap::new(), 1 << 20);
        let (h, d) = chunk(b"nope");
        assert!(!store.put(h, d));
        assert!(!store.contains(&h));
        assert!(store.get(&h).is_none());
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn source_store_refuses_bytes_that_no_longer_match() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.bin"), b"original").unwrap();
        let (h, _) = chunk(b"original");
        let store =
            SourceChunkStore::new(dir.path(), locmap(&[(h, "a.bin", 0, 8)]), 1 << 20);
        assert_eq!(store.get(&h).as_deref(), Some(&b"original"[..]));

        std::fs::write(dir.path().join("a.bin"), b"tampered").unwrap();
        // Not cached yet after a rewrite? It is cached from the first get; but a
        // fresh store (post-rescan the manager builds one) must not serve stale.
        let fresh =
            SourceChunkStore::new(dir.path(), locmap(&[(h, "a.bin", 0, 8)]), 1 << 20);
        assert!(fresh.get(&h).is_none());
        assert_eq!(fresh.failed_reads(), 1);
    }
}
