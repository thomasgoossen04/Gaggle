//! Content-addressed chunk storage and dedup accounting.
//!
//! Dedup is not a separate pass — it falls out of content addressing: [`put`]
//! keys on `blake3(data)`, so a chunk that already exists (a shared DLL across
//! two modded installs, an unchanged region of an updated archive) is stored
//! once no matter how many files or folders reference it.
//!
//! [`put`]: ChunkStore::put

use std::collections::HashMap;

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

/// In-memory [`ChunkStore`] with dedup accounting. The durable on-disk store
/// arrives with the NAS accelerator (milestone 6).
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
}
