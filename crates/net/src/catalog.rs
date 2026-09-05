//! [`Catalog`] — everything the serving side of a share can answer with.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};

use gaggle_core::{ChunkList, ChunkStore, Hash, Manifest, Scope};

use crate::proto::{Request, Response};

/// Cumulative count of chunk bytes a [`Catalog`] has actually handed out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServeStats {
    /// Bytes of `Response::Chunk` payloads served.
    pub bytes_served: u64,
    /// Number of `Request::GetChunk` requests answered with data.
    pub chunks_served: u64,
}

/// The manifest, a root-indexed set of chunk lists, and the chunk bytes
/// themselves. Cheap to answer [`Request`]s from.
///
/// The store is type-erased so a normal peer can serve from an in-RAM
/// [`MemoryChunkStore`](gaggle_core::MemoryChunkStore) while a NAS accelerator
/// serves the same way from a [`DiskChunkStore`](gaggle_core::DiskChunkStore).
/// It need not be complete: a peer that has only partially downloaded a share
/// can still serve what it holds. [`answer`](Self::answer) returns
/// [`Response::NotFound`] for a missing chunk, and [`Request::GetInventory`]
/// reports exactly the subset present so a multi-peer downloader can route
/// around the gaps.
///
/// For a private share the serving node also consults
/// [`path_for_root`](Self::path_for_root) / [`paths_for_chunk`](Self::paths_for_chunk)
/// to enforce a capability's per-file [`Scope`].
pub struct Catalog {
    manifest: Manifest,
    lists_by_root: HashMap<Hash, ChunkList>,
    /// Manifest path for each file's Merkle root.
    path_by_root: HashMap<Hash, String>,
    /// Manifest paths of every file a chunk appears in (usually one; more when a
    /// chunk is shared between files by dedup).
    paths_by_chunk: HashMap<Hash, Vec<String>>,
    store: Box<dyn ChunkStore + Send>,
    /// Cumulative bytes / chunks served through [`answer`](Self::answer). Read
    /// back with [`serve_stats`](Self::serve_stats) — the "upload throughput"
    /// signal the Stats view samples.
    bytes_served: AtomicU64,
    chunks_served: AtomicU64,
}

impl Catalog {
    /// Build a catalog from a [`gaggle_core::Snapshot`]'s parts. Straight out of
    /// [`gaggle_core::snapshot_dir`] the `store` holds every chunk referenced by
    /// `chunk_lists`; a partial `store` is allowed and serves a partial share.
    pub fn new(
        mut manifest: Manifest,
        chunk_lists: BTreeMap<String, ChunkList>,
        store: impl ChunkStore + Send + 'static,
    ) -> Self {
        manifest.canonicalize();

        let mut lists_by_root = HashMap::new();
        let mut path_by_root = HashMap::new();
        let mut paths_by_chunk: HashMap<Hash, Vec<String>> = HashMap::new();
        for (path, list) in chunk_lists {
            let root = list.root();
            path_by_root.insert(root, path.clone());
            for chunk in &list.chunks {
                let entry = paths_by_chunk.entry(chunk.hash).or_default();
                if !entry.contains(&path) {
                    entry.push(path.clone());
                }
            }
            lists_by_root.insert(root, list);
        }

        Self {
            manifest,
            lists_by_root,
            path_by_root,
            paths_by_chunk,
            store: Box::new(store),
            bytes_served: AtomicU64::new(0),
            chunks_served: AtomicU64::new(0),
        }
    }

    /// Cumulative bytes / chunks this catalog has served to downloaders.
    pub fn serve_stats(&self) -> ServeStats {
        ServeStats {
            bytes_served: self.bytes_served.load(Ordering::Relaxed),
            chunks_served: self.chunks_served.load(Ordering::Relaxed),
        }
    }

    /// The share's identity — its DHT discovery key lives at
    /// [`ShareKey::from_manifest`](crate::ShareKey::from_manifest).
    pub fn manifest_id(&self) -> Hash {
        self.manifest.id()
    }

    /// The chunk hashes this catalog can actually serve: every chunk referenced
    /// by the share that is present in the store, de-duplicated. This is the
    /// answer to [`Request::GetInventory`].
    pub fn inventory(&self) -> Vec<Hash> {
        self.inventory_scoped(&Scope::All)
    }

    /// [`inventory`](Self::inventory) restricted to chunks that belong to a file
    /// `scope` allows.
    pub fn inventory_scoped(&self, scope: &Scope) -> Vec<Hash> {
        self.lists_by_root
            .values()
            .flat_map(|list| list.chunks.iter().map(|c| c.hash))
            .filter(|hash| self.store.contains(hash) && self.chunk_in_scope(hash, scope))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// Manifest path of the file whose chunk list has this Merkle `root`.
    pub fn path_for_root(&self, root: &Hash) -> Option<&str> {
        self.path_by_root.get(root).map(String::as_str)
    }

    /// Manifest paths of every file this chunk appears in.
    pub fn paths_for_chunk(&self, hash: &Hash) -> &[String] {
        self.paths_by_chunk.get(hash).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Is `hash` part of at least one file `scope` allows?
    pub fn chunk_in_scope(&self, hash: &Hash, scope: &Scope) -> bool {
        match scope {
            Scope::All => true,
            Scope::Files(_) => self.paths_for_chunk(hash).iter().any(|p| scope.allows(p)),
        }
    }

    pub(crate) fn answer(&self, request: &Request) -> Response {
        match request {
            Request::Hello(_) => Response::Welcome,
            Request::GetManifest(None) => Response::Manifest(self.manifest.clone()),
            Request::GetManifest(Some(id)) if *id == self.manifest_id() => {
                Response::Manifest(self.manifest.clone())
            }
            Request::GetManifest(Some(_)) => Response::NotFound,
            Request::GetChunkList(root) => self
                .lists_by_root
                .get(root)
                .cloned()
                .map_or(Response::NotFound, Response::ChunkList),
            Request::GetChunk(hash) => match self.store.get(hash) {
                Some(bytes) => {
                    self.bytes_served.fetch_add(bytes.len() as u64, Ordering::Relaxed);
                    self.chunks_served.fetch_add(1, Ordering::Relaxed);
                    Response::Chunk(bytes)
                }
                None => Response::NotFound,
            },
            Request::GetInventory => Response::Inventory(self.inventory()),
        }
    }
}

#[cfg(test)]
mod tests {
    use gaggle_core::{MemoryChunkStore, snapshot_dir};

    use super::*;
    use crate::proto::{Request, Response};

    #[test]
    fn serve_stats_count_only_chunks_actually_handed_out() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.bin"), vec![7u8; 40_000]).unwrap();

        let mut store = MemoryChunkStore::new();
        let snap = snapshot_dir(dir.path(), "s", 1, &mut store).unwrap();
        let list = snap.chunk_lists.values().next().unwrap().clone();
        let catalog = Catalog::new(snap.manifest, snap.chunk_lists, store);

        assert_eq!(catalog.serve_stats(), ServeStats::default());

        // A metadata request moves no chunk bytes.
        catalog.answer(&Request::GetManifest(None));
        assert_eq!(catalog.serve_stats(), ServeStats::default());

        // A miss counts nothing.
        catalog.answer(&Request::GetChunk(Hash::of(b"nope")));
        assert_eq!(catalog.serve_stats(), ServeStats::default());

        let mut expected = 0u64;
        for chunk in &list.chunks {
            let Response::Chunk(bytes) = catalog.answer(&Request::GetChunk(chunk.hash)) else {
                panic!("expected the chunk to be served");
            };
            expected += bytes.len() as u64;
        }
        let stats = catalog.serve_stats();
        assert_eq!(stats.bytes_served, expected);
        assert_eq!(stats.chunks_served, list.chunks.len() as u64);
    }
}
