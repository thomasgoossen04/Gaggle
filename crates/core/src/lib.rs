//! Core data model for Gaggle shares.
//!
//! Content-defined chunking, per-file Merkle trees, the folder
//! manifest format, and content-addressed deduplication. Pure logic — no
//! networking, no async, no filesystem watching. [`snapshot_dir`] is the single
//! filesystem entry point and it only *reads*.
//!
//! ## How the pieces fit
//!
//! - [`chunk`] splits a file into content-defined chunks (FastCDC). Each chunk's
//!   content address is `blake3(chunk_bytes)`.
//! - [`merkle`] builds a domain-separated binary Merkle tree over a file's chunk
//!   hashes. The root is the file's identity.
//! - [`ChunkList`] is the ordered chunk sequence for one file; given a trusted
//!   root it can be fully verified before any chunk data is fetched, and it can
//!   emit per-chunk [`MerkleProof`]s.
//! - [`Manifest`] is the small, shareable "torrent file": one root per file plus
//!   directory structure. It does *not* embed chunk lists.
//! - [`ChunkStore`] is content-addressed storage; putting a chunk that is already
//!   present is a no-op — that is the dedup mechanism.
#![forbid(unsafe_code)]

pub mod agent;
pub mod chunk;
pub mod chunklist;
pub mod error;
pub mod hash;
pub mod identity;
pub mod invite;
pub mod manifest;
pub mod merkle;
pub mod snapshot;
pub mod store;

pub use agent::{AgentId, AgentKeypair};
pub use chunk::{Chunk, ChunkWithData, ChunkerConfig, chunk_reader, chunk_slice};
pub use chunklist::{ChunkList, ChunkRef};
pub use error::{Error, Result};
pub use hash::Hash;
pub use identity::{ShareKeypair, SharePublicKey, Signature};
pub use invite::{Capability, Invite, Scope, SignedCapability};
pub use manifest::{FileEntry, Manifest, ManifestDiff};
pub use merkle::{MerkleProof, MerkleTree, Side, merkle_root};
pub use snapshot::{
    IndexedSnapshot, Snapshot, SyncOutcome, index_dir, snapshot_dir, sync_share, write_share,
};
pub use store::{
    CacheStats, ChunkLocation, ChunkStore, DedupStats, DiskChunkStore, LruChunkCache,
    MemoryChunkStore, SourceChunkStore,
};
