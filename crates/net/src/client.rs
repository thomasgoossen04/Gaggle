//! The subscribing side: dial a server over loopback QUIC, then pull and verify
//! a whole share.
//!
//! [`Client`] owns a background swarm task and exposes a simple async
//! request/response call. [`download_share`] drives that call to fetch the
//! manifest, every chunk list, and every chunk, checking each against the
//! manifest root before it is kept — the server is trusted for availability
//! only.

use std::collections::{BTreeMap, HashMap};

use anyhow::Context;
use gaggle_core::{ChunkList, ChunkStore, Hash, Manifest, MemoryChunkStore};
use libp2p::futures::StreamExt;
use libp2p::multiaddr::Protocol;
use libp2p::request_response::{self, OutboundRequestId};
use libp2p::swarm::SwarmEvent;
use libp2p::{Multiaddr, PeerId};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::build_swarm;
use crate::proto::{Request, Response};

type Reply = oneshot::Sender<anyhow::Result<Response>>;

enum Command {
    Request { request: Request, reply: Reply },
}

/// A live connection to one serving peer.
pub struct Client {
    commands: mpsc::Sender<Command>,
    server: PeerId,
    task: JoinHandle<()>,
}

impl Client {
    /// Dial `server_addr` (which must carry a `/p2p/<peer-id>` component) and
    /// return once the QUIC connection is established.
    pub async fn connect(server_addr: Multiaddr) -> anyhow::Result<Self> {
        let server = server_addr
            .iter()
            .find_map(|p| match p {
                Protocol::P2p(id) => Some(id),
                _ => None,
            })
            .with_context(|| {
                format!("server address {server_addr} has no /p2p/<peer-id> component")
            })?;

        let mut swarm = build_swarm()?;
        swarm.dial(server_addr.clone()).with_context(|| format!("dialing {server_addr}"))?;

        loop {
            match swarm.select_next_some().await {
                SwarmEvent::ConnectionEstablished { peer_id, .. } if peer_id == server => break,
                SwarmEvent::OutgoingConnectionError { peer_id, error, .. }
                    if peer_id.is_none() || peer_id == Some(server) =>
                {
                    return Err(anyhow::anyhow!("could not connect to {server}: {error}"));
                }
                _ => {}
            }
        }

        let (tx, mut rx) = mpsc::channel::<Command>(32);
        let task = tokio::spawn(async move {
            let mut pending: HashMap<OutboundRequestId, Reply> = HashMap::new();
            loop {
                tokio::select! {
                    command = rx.recv() => match command {
                        None => break,
                        Some(Command::Request { request, reply }) => {
                            let id = swarm.behaviour_mut().send_request(&server, request);
                            pending.insert(id, reply);
                        }
                    },
                    event = swarm.select_next_some() => match event {
                        SwarmEvent::Behaviour(request_response::Event::Message {
                            message: request_response::Message::Response { request_id, response },
                            ..
                        }) => {
                            if let Some(reply) = pending.remove(&request_id) {
                                let _ = reply.send(Ok(response));
                            }
                        }
                        SwarmEvent::Behaviour(request_response::Event::OutboundFailure {
                            request_id,
                            error,
                            ..
                        }) => {
                            if let Some(reply) = pending.remove(&request_id) {
                                let _ = reply.send(Err(anyhow::anyhow!("request failed: {error}")));
                            }
                        }
                        SwarmEvent::ConnectionClosed { peer_id, .. } if peer_id == server => {
                            for (_, reply) in pending.drain() {
                                let _ = reply.send(Err(anyhow::anyhow!("connection to {server} closed")));
                            }
                            break;
                        }
                        _ => {}
                    }
                }
            }
        });

        Ok(Self { commands: tx, server, task })
    }

    /// The peer this client is connected to.
    pub fn server(&self) -> PeerId {
        self.server
    }

    /// Send one request and await its response.
    pub async fn request(&self, request: Request) -> anyhow::Result<Response> {
        let (reply, rx) = oneshot::channel();
        self.commands
            .send(Command::Request { request, reply })
            .await
            .map_err(|_| anyhow::anyhow!("client task has stopped"))?;
        rx.await.map_err(|_| anyhow::anyhow!("client task dropped the request"))?
    }

    /// Stop the background task and wait for it.
    pub async fn shutdown(self) {
        drop(self.commands);
        let _ = self.task.await;
    }
}

/// A share pulled in full by [`download_share`].
pub struct DownloadedShare {
    pub manifest: Manifest,
    /// Chunk lists keyed by the manifest's relative file path. Each is verified
    /// against its [`FileEntry::root`](gaggle_core::FileEntry::root).
    pub chunk_lists: BTreeMap<String, ChunkList>,
}

/// Pull `client`'s whole share into `store`, verifying every piece against the
/// manifest. Chunks already present in `store` are not re-fetched.
pub async fn download_share(
    client: &Client,
    store: &mut MemoryChunkStore,
) -> anyhow::Result<DownloadedShare> {
    let manifest = match client.request(Request::GetManifest).await? {
        Response::Manifest(m) => m,
        other => anyhow::bail!("asked for the manifest, got {}", other.kind()),
    };
    manifest.validate().context("server sent an invalid manifest")?;

    let mut chunk_lists = BTreeMap::new();
    for file in &manifest.files {
        let list = match client.request(Request::GetChunkList(file.root)).await? {
            Response::ChunkList(list) => list,
            Response::NotFound => anyhow::bail!("server has no chunk list for {}", file.path),
            other => anyhow::bail!("asked for the chunk list of {}, got {}", file.path, other.kind()),
        };
        list.verify(&file.root, file.size)
            .with_context(|| format!("chunk list for {} failed verification", file.path))?;

        for chunk in &list.chunks {
            if store.contains(&chunk.hash) {
                continue;
            }
            let data = match client.request(Request::GetChunk(chunk.hash)).await? {
                Response::Chunk(data) => data,
                Response::NotFound => {
                    anyhow::bail!("server is missing chunk {} of {}", chunk.hash, file.path)
                }
                other => anyhow::bail!("asked for a chunk of {}, got {}", file.path, other.kind()),
            };
            let got = Hash::of(&data);
            if got != chunk.hash {
                anyhow::bail!(
                    "{}: a chunk hashed to {got} but the list expects {}",
                    file.path,
                    chunk.hash
                );
            }
            if data.len() as u64 != u64::from(chunk.len) {
                anyhow::bail!(
                    "{}: chunk {} is {} bytes, the list expects {}",
                    file.path,
                    chunk.hash,
                    data.len(),
                    chunk.len
                );
            }
            store.put(chunk.hash, data);
        }
        chunk_lists.insert(file.path.clone(), list);
    }

    Ok(DownloadedShare { manifest, chunk_lists })
}
