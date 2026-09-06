//! Length-prefixed framing for [`Request`]/[`Response`] on a request-response
//! substream.
//!
//! Each message is a 4-byte big-endian length followed by a 1-byte tag and a
//! tag-specific body. Control fields (`Manifest`, `ChunkList`) travel as JSON —
//! they are tiny and already `serde`-derived in `gaggle-core`. A `Chunk`
//! response's bytes are passed through [`crate::wire_crypto`] — compressed
//! (if that helps) and encrypted — before being written, and reversed on read,
//! so the rest of the codec (and everything above it) only ever sees the
//! chunk's plaintext.

use std::io;

use async_trait::async_trait;
use gaggle_core::Hash;
use libp2p::StreamProtocol;
use libp2p::futures::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use libp2p::request_response;

use crate::proto::{Request, Response};

/// Hard ceiling on a single framed message. The largest legitimate payload is a
/// chunk (`ChunkerConfig::HUGE` caps chunks at 16 MiB) plus a one-byte tag; the
/// rest is slack for manifests of very large shares.
const MAX_FRAME: usize = 48 * 1024 * 1024;

mod tag {
    pub const REQ_MANIFEST: u8 = 1;
    pub const REQ_CHUNK_LIST: u8 = 2;
    pub const REQ_CHUNK: u8 = 3;
    pub const REQ_INVENTORY: u8 = 4;
    pub const REQ_HELLO: u8 = 5;

    pub const RES_NOT_FOUND: u8 = 0;
    pub const RES_MANIFEST: u8 = 1;
    pub const RES_CHUNK_LIST: u8 = 2;
    pub const RES_CHUNK: u8 = 3;
    pub const RES_INVENTORY: u8 = 4;
    pub const RES_WELCOME: u8 = 5;
    pub const RES_UNAUTHORIZED: u8 = 6;
}

#[derive(Debug, Clone, Default)]
pub struct GaggleCodec;

#[async_trait]
impl request_response::Codec for GaggleCodec {
    type Protocol = StreamProtocol;
    type Request = Request;
    type Response = Response;

    async fn read_request<T>(&mut self, _: &StreamProtocol, io: &mut T) -> io::Result<Request>
    where
        T: AsyncRead + Unpin + Send,
    {
        decode_request(&read_frame(io).await?)
    }

    async fn read_response<T>(&mut self, _: &StreamProtocol, io: &mut T) -> io::Result<Response>
    where
        T: AsyncRead + Unpin + Send,
    {
        let frame = read_frame(io).await?;
        // A chunk's payload is decrypted + decompressed (`wire_crypto::open`) —
        // real per-chunk CPU work. Run it on a blocking thread instead of the
        // libp2p swarm task, which otherwise serializes every landing chunk (and
        // every other connection this node is driving) onto one core. `read_frame`
        // already bounded the size. Non-chunk replies are tiny — decode inline.
        if frame.first() == Some(&tag::RES_CHUNK) {
            return tokio::task::spawn_blocking(move || {
                crate::wire_crypto::open(&frame[1..]).map(Response::Chunk).map_err(bad)
            })
            .await
            .map_err(|e| bad(format!("chunk-open task failed: {e}")))?;
        }
        decode_response(&frame)
    }

    async fn write_request<T>(
        &mut self,
        _: &StreamProtocol,
        io: &mut T,
        req: Request,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        write_frame(io, &encode_request(&req)).await
    }

    async fn write_response<T>(
        &mut self,
        _: &StreamProtocol,
        io: &mut T,
        res: Response,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        // Sealing a chunk (compress-if-smaller + XChaCha20Poly1305, via
        // `wire_crypto::seal`) is the CPU cost of serving one. Off-load it to a
        // blocking thread so the swarm task keeps multiplexing every other peer
        // while it runs — one slow core must not be a whole accelerator's upload
        // ceiling. Everything else framed here is small; encode it inline.
        let body = match res {
            Response::Chunk(data) => tokio::task::spawn_blocking(move || {
                let sealed = crate::wire_crypto::seal(&data);
                let mut v = Vec::with_capacity(1 + sealed.len());
                v.push(tag::RES_CHUNK);
                v.extend_from_slice(&sealed);
                v
            })
            .await
            .map_err(|e| bad(format!("chunk-seal task failed: {e}")))?,
            other => encode_response(&other)?,
        };
        write_frame(io, &body).await
    }
}

async fn read_frame<T: AsyncRead + Unpin + Send>(io: &mut T) -> io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    io.read_exact(&mut len).await?;
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("framed message of {len} bytes exceeds the {MAX_FRAME}-byte limit"),
        ));
    }
    // Grow the buffer as bytes actually arrive instead of trusting `len` for one
    // up-front allocation: otherwise a peer costs us up to `MAX_FRAME` of memory
    // per connection with a 4-byte header and no body. `read_to_end` doubles
    // geometrically, so a real 16 MiB chunk still lands in ~4 reallocs — nothing
    // next to hashing it.
    let mut buf = Vec::with_capacity(len.min(1 << 20));
    let read = AsyncReadExt::take(io, len as u64).read_to_end(&mut buf).await?;
    if read != len {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!("framed message ended after {read} of {len} bytes"),
        ));
    }
    Ok(buf)
}

async fn write_frame<T: AsyncWrite + Unpin + Send>(io: &mut T, body: &[u8]) -> io::Result<()> {
    let len = u32::try_from(body.len()).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidData, "message larger than u32::MAX bytes")
    })?;
    io.write_all(&len.to_be_bytes()).await?;
    io.write_all(body).await?;
    io.flush().await
}

fn encode_request(req: &Request) -> Vec<u8> {
    match req {
        Request::GetManifest(None) => vec![tag::REQ_MANIFEST],
        Request::GetManifest(Some(h)) => hash_frame(tag::REQ_MANIFEST, h),
        Request::GetChunkList(h) => hash_frame(tag::REQ_CHUNK_LIST, h),
        Request::GetChunk(h) => hash_frame(tag::REQ_CHUNK, h),
        Request::GetInventory => vec![tag::REQ_INVENTORY],
        Request::Hello(cred) => json_frame(tag::REQ_HELLO, cred).expect("SignedCapability serializes"),
    }
}

fn decode_request(bytes: &[u8]) -> io::Result<Request> {
    let (tag, rest) = bytes.split_first().ok_or_else(|| bad("empty request frame"))?;
    match *tag {
        tag::REQ_MANIFEST if rest.is_empty() => Ok(Request::GetManifest(None)),
        tag::REQ_MANIFEST => Ok(Request::GetManifest(Some(read_hash(rest)?))),
        tag::REQ_CHUNK_LIST => Ok(Request::GetChunkList(read_hash(rest)?)),
        tag::REQ_CHUNK => Ok(Request::GetChunk(read_hash(rest)?)),
        tag::REQ_INVENTORY => Ok(Request::GetInventory),
        tag::REQ_HELLO => Ok(Request::Hello(serde_json::from_slice(rest)?)),
        other => Err(bad(format!("unknown request tag {other}"))),
    }
}

fn encode_response(res: &Response) -> io::Result<Vec<u8>> {
    Ok(match res {
        Response::NotFound => vec![tag::RES_NOT_FOUND],
        Response::Manifest(m) => json_frame(tag::RES_MANIFEST, m)?,
        Response::ChunkList(l) => json_frame(tag::RES_CHUNK_LIST, l)?,
        Response::Chunk(data) => {
            let sealed = crate::wire_crypto::seal(data);
            let mut v = Vec::with_capacity(1 + sealed.len());
            v.push(tag::RES_CHUNK);
            v.extend_from_slice(&sealed);
            v
        }
        Response::Inventory(hashes) => json_frame(tag::RES_INVENTORY, hashes)?,
        Response::Welcome => vec![tag::RES_WELCOME],
        Response::Unauthorized(why) => {
            let mut v = vec![tag::RES_UNAUTHORIZED];
            v.extend_from_slice(why.as_bytes());
            v
        }
    })
}

fn decode_response(bytes: &[u8]) -> io::Result<Response> {
    let (tag, rest) = bytes.split_first().ok_or_else(|| bad("empty response frame"))?;
    match *tag {
        tag::RES_NOT_FOUND => Ok(Response::NotFound),
        tag::RES_MANIFEST => Ok(Response::Manifest(serde_json::from_slice(rest)?)),
        tag::RES_CHUNK_LIST => Ok(Response::ChunkList(serde_json::from_slice(rest)?)),
        tag::RES_CHUNK => Ok(Response::Chunk(crate::wire_crypto::open(rest).map_err(bad)?)),
        tag::RES_INVENTORY => Ok(Response::Inventory(serde_json::from_slice(rest)?)),
        tag::RES_WELCOME => Ok(Response::Welcome),
        tag::RES_UNAUTHORIZED => Ok(Response::Unauthorized(
            String::from_utf8(rest.to_vec()).map_err(|e| bad(format!("bad reason string: {e}")))?,
        )),
        other => Err(bad(format!("unknown response tag {other}"))),
    }
}

fn hash_frame(tag: u8, hash: &Hash) -> Vec<u8> {
    let mut v = Vec::with_capacity(1 + Hash::LEN);
    v.push(tag);
    v.extend_from_slice(hash.as_bytes());
    v
}

fn json_frame<T: serde::Serialize>(tag: u8, value: &T) -> io::Result<Vec<u8>> {
    let mut v = vec![tag];
    serde_json::to_writer(&mut v, value)?;
    Ok(v)
}

fn read_hash(bytes: &[u8]) -> io::Result<Hash> {
    let arr: [u8; Hash::LEN] =
        bytes.try_into().map_err(|_| bad("expected a 32-byte hash after the tag"))?;
    Ok(Hash::from_bytes(arr))
}

fn bad(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gaggle_core::{Capability, Manifest, ShareKeypair};

    fn sample_credential() -> gaggle_core::SignedCapability {
        let kp = ShareKeypair::from_seed([5u8; 32]);
        kp.issue(Capability::new(kp.public(), Hash::of(b"m")))
    }

    #[test]
    fn request_frames_round_trip() {
        let h = Hash::of(b"x");
        for req in [
            Request::GetManifest(None),
            Request::GetManifest(Some(Hash::of(b"share"))),
            Request::GetChunkList(h),
            Request::GetChunk(h),
            Request::GetInventory,
            Request::Hello(sample_credential()),
        ] {
            assert_eq!(decode_request(&encode_request(&req)).unwrap(), req);
        }
    }

    #[test]
    fn response_frames_round_trip() {
        let manifest = Manifest::new("s", 1);
        let cases = [
            Response::NotFound,
            Response::Manifest(manifest),
            Response::Chunk(vec![7u8; 1024]),
            Response::Inventory(vec![Hash::of(b"a"), Hash::of(b"b"), Hash::of(b"c")]),
            Response::Welcome,
            Response::Unauthorized("present an invite first".into()),
        ];
        for res in cases {
            let bytes = encode_response(&res).unwrap();
            assert_eq!(decode_response(&bytes).unwrap(), res);
        }
    }

    #[tokio::test]
    async fn oversized_length_prefix_is_rejected() {
        let mut framed = ((MAX_FRAME as u32) + 1).to_be_bytes().to_vec();
        framed.push(tag::REQ_MANIFEST);
        let mut reader = framed.as_slice();
        let err = read_frame(&mut reader).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }
}
