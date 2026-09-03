//! Wire protocol for the milestone-2 chunk exchange.
//!
//! A single libp2p request-response protocol carries three request kinds. Every
//! answer a subscriber gets back is verifiable against the manifest it already
//! trusts (see [`crate::download_share`]), so the serving side is never trusted
//! for anything but availability.

use gaggle_core::{ChunkList, Hash, Manifest};

/// libp2p [`StreamProtocol`](libp2p::StreamProtocol) name for the chunk exchange.
pub const PROTOCOL: &str = "/gaggle/chunk/1.0.0";

/// What a subscriber asks the origin (or, later, an accelerator) for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// The share's manifest — the small document everything else is checked
    /// against.
    GetManifest,
    /// The ordered chunk list for the file whose Merkle root is this hash.
    GetChunkList(Hash),
    /// The bytes of one content-addressed chunk.
    GetChunk(Hash),
}

/// The serving side's answer to a [`Request`].
#[derive(Clone, PartialEq, Eq)]
pub enum Response {
    /// The requested chunk list or chunk is not held here.
    NotFound,
    Manifest(Manifest),
    ChunkList(ChunkList),
    Chunk(Vec<u8>),
}

impl Response {
    /// Short label for error messages — avoids `Debug`-printing a whole chunk.
    pub fn kind(&self) -> &'static str {
        match self {
            Response::NotFound => "NotFound",
            Response::Manifest(_) => "Manifest",
            Response::ChunkList(_) => "ChunkList",
            Response::Chunk(_) => "Chunk",
        }
    }
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Response::NotFound => f.write_str("NotFound"),
            Response::Manifest(m) => write!(f, "Manifest({:?}, {} files)", m.name, m.files.len()),
            Response::ChunkList(l) => {
                write!(f, "ChunkList({} chunks, {} bytes)", l.len(), l.total_size)
            }
            Response::Chunk(d) => write!(f, "Chunk({} bytes)", d.len()),
        }
    }
}
