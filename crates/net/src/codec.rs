//! Length-prefixed framing for [`Request`]/[`Response`] on a request-response
//! substream.
//!
//! Each message is a 4-byte big-endian length followed by a 1-byte tag and a
//! tag-specific body. Control fields (`Manifest`, `ChunkList`) travel as JSON —
//! they are tiny and already `serde`-derived in `gaggle-core`. Chunk bytes are
//! written raw, so a 16 MiB chunk costs 16 MiB on the wire rather than ~1.4x
//! that as a JSON byte array.

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

    pub const RES_NOT_FOUND: u8 = 0;
    pub const RES_MANIFEST: u8 = 1;
    pub const RES_CHUNK_LIST: u8 = 2;
    pub const RES_CHUNK: u8 = 3;
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
        decode_response(&read_frame(io).await?)
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
        write_frame(io, &encode_response(&res)?).await
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
    let mut buf = vec![0u8; len];
    io.read_exact(&mut buf).await?;
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
        Request::GetManifest => vec![tag::REQ_MANIFEST],
        Request::GetChunkList(h) => hash_frame(tag::REQ_CHUNK_LIST, h),
        Request::GetChunk(h) => hash_frame(tag::REQ_CHUNK, h),
    }
}

fn decode_request(bytes: &[u8]) -> io::Result<Request> {
    let (tag, rest) = bytes.split_first().ok_or_else(|| bad("empty request frame"))?;
    match *tag {
        tag::REQ_MANIFEST => Ok(Request::GetManifest),
        tag::REQ_CHUNK_LIST => Ok(Request::GetChunkList(read_hash(rest)?)),
        tag::REQ_CHUNK => Ok(Request::GetChunk(read_hash(rest)?)),
        other => Err(bad(format!("unknown request tag {other}"))),
    }
}

fn encode_response(res: &Response) -> io::Result<Vec<u8>> {
    Ok(match res {
        Response::NotFound => vec![tag::RES_NOT_FOUND],
        Response::Manifest(m) => json_frame(tag::RES_MANIFEST, m)?,
        Response::ChunkList(l) => json_frame(tag::RES_CHUNK_LIST, l)?,
        Response::Chunk(data) => {
            let mut v = Vec::with_capacity(1 + data.len());
            v.push(tag::RES_CHUNK);
            v.extend_from_slice(data);
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
        tag::RES_CHUNK => Ok(Response::Chunk(rest.to_vec())),
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
    use gaggle_core::Manifest;

    #[test]
    fn request_frames_round_trip() {
        let h = Hash::of(b"x");
        for req in [Request::GetManifest, Request::GetChunkList(h), Request::GetChunk(h)] {
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
