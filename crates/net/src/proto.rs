//! Wire protocol for the milestone-2 chunk exchange.
//!
//! A single libp2p request-response protocol carries three request kinds. Every
//! answer a subscriber gets back is verifiable against the manifest it already
//! trusts (see [`crate::download_share`]), so the serving side is never trusted
//! for anything but availability.

use gaggle_core::{ChunkList, Hash, Manifest, SignedCapability};

/// libp2p [`StreamProtocol`](libp2p::StreamProtocol) name for the chunk exchange.
pub const PROTOCOL: &str = "/gaggle/chunk/1.0.0";

/// What a subscriber asks the origin (or, later, an accelerator) for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Present a capability token for this connection (milestone 7). On a
    /// private share every other request is refused with
    /// [`Response::Unauthorized`] until a `Hello` carrying a valid token has
    /// been answered with [`Response::Welcome`]. Harmless on a public share.
    Hello(SignedCapability),
    /// The share's manifest — the small document everything else is checked
    /// against.
    GetManifest,
    /// The ordered chunk list for the file whose Merkle root is this hash.
    GetChunkList(Hash),
    /// The bytes of one content-addressed chunk.
    GetChunk(Hash),
    /// Which of the share's chunks this peer can currently serve. Lets a
    /// multi-peer downloader (milestone 4) compute per-chunk availability and
    /// pull the rarest chunks first. A full seed answers with every chunk it
    /// holds; a peer still downloading answers with its partial set.
    GetInventory,
}

/// The serving side's answer to a [`Request`].
#[derive(Clone, PartialEq, Eq)]
pub enum Response {
    /// The requested chunk list or chunk is not held here.
    NotFound,
    Manifest(Manifest),
    ChunkList(ChunkList),
    Chunk(Vec<u8>),
    /// The chunk hashes this peer can serve, in answer to
    /// [`Request::GetInventory`].
    Inventory(Vec<Hash>),
    /// The [`Request::Hello`] token was accepted; this connection may now make
    /// requests (within the token's scope).
    Welcome,
    /// The request was refused: no token presented, an invalid/expired token,
    /// or a request outside the token's per-file scope. The string is a
    /// human-readable reason.
    Unauthorized(String),
}

impl Response {
    /// Short label for error messages — avoids `Debug`-printing a whole chunk.
    pub fn kind(&self) -> &'static str {
        match self {
            Response::NotFound => "NotFound",
            Response::Manifest(_) => "Manifest",
            Response::ChunkList(_) => "ChunkList",
            Response::Chunk(_) => "Chunk",
            Response::Inventory(_) => "Inventory",
            Response::Welcome => "Welcome",
            Response::Unauthorized(_) => "Unauthorized",
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
            Response::Inventory(h) => write!(f, "Inventory({} hashes)", h.len()),
            Response::Welcome => f.write_str("Welcome"),
            Response::Unauthorized(why) => write!(f, "Unauthorized({why:?})"),
        }
    }
}
