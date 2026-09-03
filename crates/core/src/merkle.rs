//! Per-file binary Merkle tree over chunk hashes.
//!
//! Leaves and internal nodes are domain-separated (a one-byte prefix) so an
//! internal node's preimage can never be reinterpreted as a leaf. A level with
//! an odd node count promotes the lone node to the next level unchanged (no
//! duplication). The verifier reconstructs the expected tree shape from
//! `num_leaves` alone, which it learns from the trusted manifest / chunk list —
//! so a forged proof cannot lie about the tree's size or shape.

use serde::{Deserialize, Serialize};

use crate::hash::Hash;

const LEAF_PREFIX: u8 = 0x00;
const NODE_PREFIX: u8 = 0x01;

/// Root of a file with zero chunks. A fixed, distinct constant so an empty file
/// still has a well-defined content identity.
fn empty_root() -> Hash {
    let mut h = blake3::Hasher::new();
    h.update(b"gaggle/merkle/empty-file/v1");
    Hash::from(h.finalize())
}

fn hash_leaf(chunk_hash: &Hash) -> Hash {
    let mut h = blake3::Hasher::new();
    h.update(&[LEAF_PREFIX]);
    h.update(chunk_hash.as_bytes());
    Hash::from(h.finalize())
}

fn hash_node(left: &Hash, right: &Hash) -> Hash {
    let mut h = blake3::Hasher::new();
    h.update(&[NODE_PREFIX]);
    h.update(left.as_bytes());
    h.update(right.as_bytes());
    Hash::from(h.finalize())
}

/// Fold one level into its parent level, promoting a trailing lone node.
fn fold_level(level: &[Hash]) -> Vec<Hash> {
    let mut next = Vec::with_capacity(level.len().div_ceil(2));
    let mut pairs = level.chunks_exact(2);
    for pair in &mut pairs {
        next.push(hash_node(&pair[0], &pair[1]));
    }
    if let [lone] = pairs.remainder() {
        next.push(*lone);
    }
    next
}

/// Merkle root over an ordered list of chunk hashes.
pub fn merkle_root(chunk_hashes: &[Hash]) -> Hash {
    if chunk_hashes.is_empty() {
        return empty_root();
    }
    let mut level: Vec<Hash> = chunk_hashes.iter().map(hash_leaf).collect();
    while level.len() > 1 {
        level = fold_level(&level);
    }
    level[0]
}

/// A fully materialised Merkle tree — keeps every level so it can emit proofs.
#[derive(Debug, Clone)]
pub struct MerkleTree {
    /// `levels[0]` is the leaf hashes; the last level holds exactly the root.
    levels: Vec<Vec<Hash>>,
    num_leaves: usize,
}

impl MerkleTree {
    pub fn build(chunk_hashes: &[Hash]) -> Self {
        if chunk_hashes.is_empty() {
            return Self { levels: vec![vec![empty_root()]], num_leaves: 0 };
        }
        let mut levels = vec![chunk_hashes.iter().map(hash_leaf).collect::<Vec<_>>()];
        while levels.last().unwrap().len() > 1 {
            let folded = fold_level(levels.last().unwrap());
            levels.push(folded);
        }
        Self { num_leaves: chunk_hashes.len(), levels }
    }

    pub fn root(&self) -> Hash {
        self.levels.last().unwrap()[0]
    }

    pub fn num_leaves(&self) -> usize {
        self.num_leaves
    }

    /// Sibling path proving that the chunk hash at `index` is committed under
    /// [`root`](Self::root). `None` if `index` is out of range (including any
    /// index for an empty tree).
    pub fn proof(&self, index: usize) -> Option<MerkleProof> {
        if index >= self.num_leaves {
            return None;
        }
        let mut siblings = Vec::new();
        let mut idx = index;
        for level in &self.levels[..self.levels.len() - 1] {
            let is_last_and_odd = idx == level.len() - 1 && !level.len().is_multiple_of(2);
            if !is_last_and_odd {
                let sib = if idx.is_multiple_of(2) {
                    ProofStep { hash: level[idx + 1], side: Side::Right }
                } else {
                    ProofStep { hash: level[idx - 1], side: Side::Left }
                };
                siblings.push(sib);
            }
            idx /= 2;
        }
        Some(MerkleProof { leaf_index: index, num_leaves: self.num_leaves, siblings })
    }
}

/// Which side of the concatenation a proof sibling sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProofStep {
    pub hash: Hash,
    pub side: Side,
}

/// An inclusion proof for a single chunk against a file's Merkle root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MerkleProof {
    pub leaf_index: usize,
    /// Total number of chunks in the file. Fixes the tree shape.
    pub num_leaves: usize,
    pub siblings: Vec<ProofStep>,
}

impl MerkleProof {
    /// Recompute the root from `chunk_hash` and the sibling path and compare it
    /// to `root`. Returns `false` on any inconsistency (bad index, wrong number
    /// of siblings for the claimed `num_leaves`, root mismatch).
    pub fn verify(&self, root: &Hash, chunk_hash: &Hash) -> bool {
        if self.num_leaves == 0 || self.leaf_index >= self.num_leaves {
            return false;
        }
        let mut acc = hash_leaf(chunk_hash);
        let mut idx = self.leaf_index;
        let mut level_len = self.num_leaves;
        let mut steps = self.siblings.iter();
        while level_len > 1 {
            let is_last_and_odd = idx == level_len - 1 && !level_len.is_multiple_of(2);
            if !is_last_and_odd {
                let Some(step) = steps.next() else { return false };
                acc = match step.side {
                    Side::Left => hash_node(&step.hash, &acc),
                    Side::Right => hash_node(&acc, &step.hash),
                };
            }
            idx /= 2;
            level_len = level_len.div_ceil(2);
        }
        steps.next().is_none() && acc == *root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaves(n: usize) -> Vec<Hash> {
        (0..n).map(|i| Hash::of(format!("chunk-{i}").as_bytes())).collect()
    }

    #[test]
    fn empty_root_is_stable_and_distinct() {
        assert_eq!(merkle_root(&[]), empty_root());
        assert_eq!(MerkleTree::build(&[]).root(), empty_root());
        assert_ne!(empty_root(), Hash::of(b""));
    }

    #[test]
    fn single_leaf_root_is_the_leaf_hash() {
        let l = leaves(1);
        assert_eq!(merkle_root(&l), hash_leaf(&l[0]));
    }

    #[test]
    fn tree_root_matches_streaming_root() {
        for n in 0..=33 {
            let l = leaves(n);
            assert_eq!(MerkleTree::build(&l).root(), merkle_root(&l), "n = {n}");
        }
    }

    #[test]
    fn distinct_inputs_give_distinct_roots() {
        assert_ne!(merkle_root(&leaves(4)), merkle_root(&leaves(5)));
        let mut swapped = leaves(4);
        swapped.swap(0, 1);
        assert_ne!(merkle_root(&leaves(4)), merkle_root(&swapped));
    }

    #[test]
    fn every_proof_verifies() {
        for n in 1..=33 {
            let l = leaves(n);
            let tree = MerkleTree::build(&l);
            let root = tree.root();
            for (i, leaf) in l.iter().enumerate() {
                let proof = tree.proof(i).expect("in range");
                assert!(proof.verify(&root, leaf), "n = {n}, i = {i}");
            }
            assert!(tree.proof(n).is_none());
        }
    }

    #[test]
    fn tampered_proofs_are_rejected() {
        let l = leaves(9);
        let tree = MerkleTree::build(&l);
        let root = tree.root();
        let proof = tree.proof(3).unwrap();

        // wrong chunk hash
        assert!(!proof.verify(&root, &l[4]));
        // wrong root
        assert!(!proof.verify(&Hash::of(b"not the root"), &l[3]));

        // flipped sibling
        let mut bad = proof.clone();
        if let Some(step) = bad.siblings.first_mut() {
            step.hash = Hash::of(b"evil");
        }
        assert!(!bad.verify(&root, &l[3]));

        // lied-about tree size
        let mut wrong_size = proof.clone();
        wrong_size.num_leaves = 8;
        assert!(!wrong_size.verify(&root, &l[3]));

        // extra sibling
        let mut extra = proof.clone();
        extra.siblings.push(ProofStep { hash: Hash::of(b"x"), side: Side::Left });
        assert!(!extra.verify(&root, &l[3]));
    }
}
