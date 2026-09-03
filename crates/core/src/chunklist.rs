//! [`ChunkList`] — the ordered chunk sequence for a single file.
//!
//! The manifest commits to a file by its Merkle root only. To actually fetch a
//! file you need its `ChunkList`; because it is verifiable against the trusted
//! root, it can be downloaded from any untrusted peer and checked before a
//! single byte of chunk data is requested.

use serde::{Deserialize, Serialize};

use crate::chunk::Chunk;
use crate::error::{Error, Result};
use crate::hash::Hash;
use crate::merkle::{MerkleProof, MerkleTree, merkle_root};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkRef {
    pub hash: Hash,
    pub len: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkList {
    /// Sum of `chunks[*].len`. Redundant with the manifest's file size; carried
    /// here so the list is self-checkable.
    pub total_size: u64,
    pub chunks: Vec<ChunkRef>,
}

impl ChunkList {
    pub fn from_chunks(chunks: &[Chunk]) -> Self {
        Self {
            total_size: chunks.iter().map(|c| u64::from(c.len)).sum(),
            chunks: chunks
                .iter()
                .map(|c| ChunkRef { hash: c.hash, len: c.len })
                .collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    pub fn len(&self) -> usize {
        self.chunks.len()
    }

    pub fn hashes(&self) -> Vec<Hash> {
        self.chunks.iter().map(|c| c.hash).collect()
    }

    /// Merkle root over the chunk hashes — this is what the manifest stores.
    pub fn root(&self) -> Hash {
        merkle_root(&self.hashes())
    }

    pub fn tree(&self) -> MerkleTree {
        MerkleTree::build(&self.hashes())
    }

    /// Inclusion proof for chunk `index` against [`root`](Self::root).
    pub fn proof(&self, index: usize) -> Option<MerkleProof> {
        self.tree().proof(index)
    }

    /// Check the list against the trusted `expected_root` and `expected_size`
    /// from a manifest, and check its own internal consistency.
    pub fn verify(&self, expected_root: &Hash, expected_size: u64) -> Result<()> {
        let summed: u64 = self.chunks.iter().map(|c| u64::from(c.len)).sum();
        if summed != self.total_size {
            return Err(Error::Verify(format!(
                "chunk lengths sum to {summed} but total_size is {}",
                self.total_size
            )));
        }
        if self.total_size != expected_size {
            return Err(Error::Verify(format!(
                "chunk list is {} bytes, manifest says {expected_size}",
                self.total_size
            )));
        }
        if self.chunks.iter().any(|c| c.len == 0) {
            return Err(Error::Verify("chunk list contains a zero-length chunk".into()));
        }
        if self.root() != *expected_root {
            return Err(Error::Verify("merkle root does not match manifest".into()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{ChunkerConfig, chunk_slice};

    fn sample() -> (Vec<u8>, ChunkList) {
        let mut data = Vec::new();
        for i in 0..400_000u32 {
            data.extend_from_slice(&i.to_le_bytes());
        }
        let cfg = ChunkerConfig { min: 8 * 1024, avg: 16 * 1024, max: 64 * 1024 };
        let list = ChunkList::from_chunks(&chunk_slice(&data, cfg).unwrap());
        (data, list)
    }

    #[test]
    fn totals_and_root() {
        let (data, list) = sample();
        assert_eq!(list.total_size, data.len() as u64);
        assert!(list.len() > 1);
        assert_eq!(list.root(), merkle_root(&list.hashes()));
    }

    #[test]
    fn verify_accepts_matching_and_rejects_mismatches() {
        let (data, list) = sample();
        let root = list.root();
        let size = data.len() as u64;

        assert!(list.verify(&root, size).is_ok());
        assert!(list.verify(&Hash::of(b"wrong"), size).is_err());
        assert!(list.verify(&root, size + 1).is_err());

        let mut broken = list.clone();
        broken.total_size += 1;
        assert!(broken.verify(&root, size).is_err());
    }

    #[test]
    fn proofs_line_up_with_the_root() {
        let (_data, list) = sample();
        let root = list.root();
        for i in 0..list.len() {
            let proof = list.proof(i).unwrap();
            assert!(proof.verify(&root, &list.chunks[i].hash));
        }
    }

    #[test]
    fn empty_file_list() {
        let list = ChunkList::from_chunks(&[]);
        assert!(list.is_empty());
        assert_eq!(list.total_size, 0);
        assert_eq!(list.root(), merkle_root(&[]));
        assert!(list.verify(&merkle_root(&[]), 0).is_ok());
    }
}
