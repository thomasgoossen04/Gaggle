//! Content-defined chunking (FastCDC v2020).
//!
//! Boundaries are chosen from local content, so inserting or removing bytes only
//! disturbs the chunks around the edit — the rest keep their identity. That is
//! what makes cross-file dedup and delta sync effective; a
//! fixed-size split would re-hash everything after any insertion.

use std::io::Read;

use fastcdc::v2020::StreamCDC;

use crate::error::{Error, Result};
use crate::hash::Hash;

const KIB: usize = 1024;
const MIB: usize = 1024 * 1024;

// FastCDC v2020 accepts min ∈ [64, 64 MiB], avg ∈ [256, 256 MiB], max ∈ [1 KiB, 1 GiB].
const FASTCDC_MIN_FLOOR: usize = 64;
const FASTCDC_AVG_FLOOR: usize = 256;
const FASTCDC_MAX_CEIL: usize = 1 << 30;

/// Target chunk-size parameters. `avg` is the statistical target; real chunks
/// land in `[min, max]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkerConfig {
    pub min: usize,
    pub avg: usize,
    pub max: usize,
}

impl ChunkerConfig {
    /// Files up to ~2 GiB. ~1 MiB chunks → a 2 GiB file is ~2k chunks.
    pub const DEFAULT: Self = Self { min: 512 * KIB, avg: MIB, max: 4 * MIB };
    /// ~2–32 GiB files.
    pub const LARGE: Self = Self { min: MIB, avg: 2 * MIB, max: 8 * MIB };
    /// Huge single archives (game installs). Keeps chunk counts in the tens of
    /// thousands rather than the millions.
    pub const HUGE: Self = Self { min: 2 * MIB, avg: 4 * MIB, max: 16 * MIB };

    /// First-cut size heuristic (see `notes/plan.md` open questions): scale the
    /// target chunk size with the file so the per-file chunk count — and hence
    /// the Merkle tree and chunk list — stays manageable as files grow.
    pub fn for_file_size(size: u64) -> Self {
        const GIB: u64 = 1 << 30;
        match size {
            s if s <= 2 * GIB => Self::DEFAULT,
            s if s <= 32 * GIB => Self::LARGE,
            _ => Self::HUGE,
        }
    }

    fn validate(&self) -> Result<()> {
        if !(self.min <= self.avg && self.avg <= self.max) {
            return Err(Error::Chunker(format!(
                "sizes must satisfy min <= avg <= max, got {}/{}/{}",
                self.min, self.avg, self.max
            )));
        }
        if self.min < FASTCDC_MIN_FLOOR
            || self.avg < FASTCDC_AVG_FLOOR
            || self.max > FASTCDC_MAX_CEIL
        {
            return Err(Error::Chunker(format!(
                "sizes {}/{}/{} outside supported range [{FASTCDC_MIN_FLOOR}, .., {FASTCDC_MAX_CEIL}]",
                self.min, self.avg, self.max
            )));
        }
        Ok(())
    }
}

/// One content-defined slice of a file: where it sits and what it hashes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chunk {
    /// Content address, `blake3(bytes)`.
    pub hash: Hash,
    /// Byte offset of this chunk within its file.
    pub offset: u64,
    /// Length in bytes. Always `>= 1`; only the final chunk may be `< cfg.min`.
    pub len: u32,
}

/// A [`Chunk`] together with its bytes, as produced by [`chunk_reader`].
#[derive(Debug, Clone)]
pub struct ChunkWithData {
    pub chunk: Chunk,
    pub data: Vec<u8>,
}

/// Chunk an in-memory buffer. Convenient for tests and small blobs; use
/// [`chunk_reader`] for real files so memory stays bounded.
pub fn chunk_slice(data: &[u8], cfg: ChunkerConfig) -> Result<Vec<Chunk>> {
    cfg.validate()?;
    if data.is_empty() {
        return Ok(Vec::new());
    }
    let chunker = fastcdc::v2020::FastCDC::new(data, cfg.min, cfg.avg, cfg.max);
    Ok(chunker
        .map(|c| {
            let end = c.offset + c.length;
            Chunk {
                hash: Hash::of(&data[c.offset..end]),
                offset: c.offset as u64,
                len: c.length as u32,
            }
        })
        .collect())
}

/// Streaming chunker: yields each chunk with its bytes, reading `source`
/// incrementally. The caller decides what to do with the data (store it, write
/// it, drop it).
pub fn chunk_reader<R: Read>(source: R, cfg: ChunkerConfig) -> Result<ChunkReader<R>> {
    cfg.validate()?;
    Ok(ChunkReader {
        inner: StreamCDC::new(source, cfg.min, cfg.avg, cfg.max),
        done: false,
    })
}

/// Iterator returned by [`chunk_reader`]. Item type is `Result<ChunkWithData>`.
pub struct ChunkReader<R: Read> {
    inner: StreamCDC<R>,
    done: bool,
}

impl<R: Read> Iterator for ChunkReader<R> {
    type Item = Result<ChunkWithData>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.inner.next() {
            None | Some(Err(fastcdc::v2020::Error::Empty)) => {
                self.done = true;
                None
            }
            Some(Err(fastcdc::v2020::Error::IoError(e))) => {
                self.done = true;
                Some(Err(Error::Io(e)))
            }
            Some(Err(e)) => {
                self.done = true;
                Some(Err(Error::Chunker(format!("{e:?}"))))
            }
            Some(Ok(cd)) => {
                let chunk = Chunk {
                    hash: Hash::of(&cd.data),
                    offset: cd.offset,
                    len: cd.length as u32,
                };
                Some(Ok(ChunkWithData { chunk, data: cd.data }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic pseudo-random bytes (splitmix64) so chunk boundaries are
    /// content-defined rather than all landing on `max`.
    fn pattern(len: usize, mut seed: u64) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            seed = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = seed;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^= z >> 31;
            out.extend_from_slice(&z.to_le_bytes());
        }
        out.truncate(len);
        out
    }

    const TEST_CFG: ChunkerConfig = ChunkerConfig {
        min: 8 * KIB,
        avg: 16 * KIB,
        max: 64 * KIB,
    };

    #[test]
    fn empty_input_has_no_chunks() {
        assert!(chunk_slice(b"", TEST_CFG).unwrap().is_empty());
        let v: Vec<_> = chunk_reader(&b""[..], TEST_CFG).unwrap().collect();
        assert!(v.is_empty());
    }

    #[test]
    fn small_input_is_one_chunk() {
        let data = b"a short file";
        let chunks = chunk_slice(data, TEST_CFG).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].offset, 0);
        assert_eq!(chunks[0].len as usize, data.len());
        assert_eq!(chunks[0].hash, Hash::of(data));
    }

    #[test]
    fn chunks_are_contiguous_and_cover_input() {
        let data = pattern(2 * 1024 * 1024, 1);
        let chunks = chunk_slice(&data, TEST_CFG).unwrap();
        assert!(chunks.len() > 1, "expected a multi-chunk split");

        let mut cursor = 0u64;
        let mut rebuilt = Vec::new();
        for c in &chunks {
            assert_eq!(c.offset, cursor);
            let end = (c.offset + c.len as u64) as usize;
            rebuilt.extend_from_slice(&data[c.offset as usize..end]);
            assert_eq!(c.hash, Hash::of(&data[c.offset as usize..end]));
            cursor += c.len as u64;
        }
        assert_eq!(cursor as usize, data.len());
        assert_eq!(rebuilt, data);
    }

    #[test]
    fn deterministic_and_reader_matches_slice() {
        let data = pattern(3 * 1024 * 1024, 42);
        let a = chunk_slice(&data, TEST_CFG).unwrap();
        let b = chunk_slice(&data, TEST_CFG).unwrap();
        assert_eq!(a, b);

        let streamed: Vec<Chunk> = chunk_reader(&data[..], TEST_CFG)
            .unwrap()
            .map(|r| r.unwrap().chunk)
            .collect();
        assert_eq!(a, streamed);
    }

    #[test]
    fn rejects_bad_config() {
        let bad = ChunkerConfig { min: 100, avg: 50, max: 200 };
        assert!(chunk_slice(b"x", bad).is_err());
    }
}
