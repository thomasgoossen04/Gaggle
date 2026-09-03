//! [`Catalog`] — everything the serving side of a share can answer with.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use gaggle_core::{ChunkList, ChunkStore, Hash, Manifest, Scope};

use crate::proto::{Request, Response};

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

        Self { manifest, lists_by_root, path_by_root, paths_by_chunk, store: Box::new(store) }
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
            Request::GetManifest => Response::Manifest(self.manifest.clone()),
            Request::GetChunkList(root) => self
                .lists_by_root
                .get(root)
                .cloned()
                .map_or(Response::NotFound, Response::ChunkList),
            Request::GetChunk(hash) => {
                self.store.get(hash).map_or(Response::NotFound, Response::Chunk)
            }
            Request::GetInventory => Response::Inventory(self.inventory()),
        }
    }
}
