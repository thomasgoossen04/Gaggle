//! The serving side of a share: a [`Catalog`] of content plus a background swarm
//! task ([`ServerHandle`]) that answers [`Request`]s over loopback QUIC.
//!
//! Milestone 2 only needs the origin peer to hand its whole share to one
//! subscriber. Peer discovery (Kademlia), NAT traversal (relay/dcutr) and
//! multi-peer swarming land in later milestones on top of this same behaviour.

use std::collections::{BTreeMap, HashMap};

use gaggle_core::{ChunkList, ChunkStore, Hash, Manifest, MemoryChunkStore};
use libp2p::futures::StreamExt;
use libp2p::swarm::SwarmEvent;
use libp2p::{Multiaddr, PeerId, request_response};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::build_swarm;
use crate::proto::{Request, Response};

/// Everything the serving side can answer with: the manifest, a root-indexed set
/// of chunk lists, and the chunk bytes themselves.
pub struct Catalog {
    manifest: Manifest,
    lists_by_root: HashMap<Hash, ChunkList>,
    store: MemoryChunkStore,
}

impl Catalog {
    /// Build a catalog from a [`gaggle_core::Snapshot`]'s parts. The `store` must
    /// already hold every chunk referenced by `chunk_lists` (as it does straight
    /// out of [`gaggle_core::snapshot_dir`]).
    pub fn new(
        mut manifest: Manifest,
        chunk_lists: BTreeMap<String, ChunkList>,
        store: MemoryChunkStore,
    ) -> Self {
        manifest.canonicalize();
        let lists_by_root =
            chunk_lists.into_values().map(|list| (list.root(), list)).collect();
        Self { manifest, lists_by_root, store }
    }

    fn answer(&self, request: &Request) -> Response {
        match request {
            Request::GetManifest => Response::Manifest(self.manifest.clone()),
            Request::GetChunkList(root) => self
                .lists_by_root
                .get(root)
                .cloned()
                .map_or(Response::NotFound, Response::ChunkList),
            Request::GetChunk(hash) => self
                .store
                .get(hash)
                .map_or(Response::NotFound, Response::Chunk),
        }
    }
}

/// Handle to a running server task. Dropping it stops the task; [`shutdown`] does
/// so gracefully and waits for it to unwind.
///
/// [`shutdown`]: ServerHandle::shutdown
pub struct ServerHandle {
    /// This node's libp2p identity.
    pub peer_id: PeerId,
    /// A dialable loopback address, `/ip4/127.0.0.1/udp/<port>/quic-v1/p2p/<id>`.
    pub listen_addr: Multiaddr,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl ServerHandle {
    /// Start listening on an ephemeral loopback QUIC port and spawn the task that
    /// serves `catalog`. Returns once the listen address is known.
    pub async fn spawn(catalog: Catalog) -> anyhow::Result<Self> {
        let mut swarm = build_swarm()?;
        let peer_id = *swarm.local_peer_id();
        swarm.listen_on("/ip4/127.0.0.1/udp/0/quic-v1".parse()?)?;

        let listen_addr = loop {
            match swarm.select_next_some().await {
                SwarmEvent::NewListenAddr { address, .. } => break address.with_p2p(peer_id).expect("peer id fits the multiaddr"),
                SwarmEvent::ListenerError { error, .. } => {
                    return Err(anyhow::anyhow!("listener failed before it came up: {error}"));
                }
                _ => {}
            }
        };

        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    event = swarm.select_next_some() => {
                        if let SwarmEvent::Behaviour(request_response::Event::Message {
                            message: request_response::Message::Request { request, channel, .. },
                            peer,
                            ..
                        }) = event
                        {
                            let response = catalog.answer(&request);
                            tracing::debug!(%peer, ?request, kind = response.kind(), "served request");
                            if swarm.behaviour_mut().send_response(channel, response).is_err() {
                                tracing::warn!(%peer, "subscriber hung up before the response was sent");
                            }
                        }
                    }
                }
            }
        });

        Ok(Self { peer_id, listen_addr, shutdown: Some(shutdown_tx), task: Some(task) })
    }

    /// Signal the task to stop and wait for it.
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}
