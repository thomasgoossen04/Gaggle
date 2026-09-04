//! Pulling one share from many peers at once, rarest chunk first.
//!
//! [`fetch_share_from_swarm`] is the multi-source counterpart of
//! [`fetch_share`](crate::transfer::fetch_share). Given a set of source peers it
//!
//! 1. fetches and validates the manifest and every chunk list (from whichever
//!    source answers first),
//! 2. asks each source for its [inventory](crate::proto::Request::GetInventory)
//!    and builds a per-chunk availability map,
//! 3. schedules chunk requests **rarest-first** — the chunk held by the fewest
//!    sources goes out first, so a chunk that lives on a single flaky peer is
//!    not left until the end — running several requests per peer concurrently
//!    and spreading load across sources,
//! 4. verifies every chunk against its list entry as it lands, and re-routes a
//!    chunk to another holder if a source fails or turns out not to have it.
//!
//! The transport is abstracted to one async call, exactly as in
//! [`fetch_share`](crate::transfer::fetch_share), so this works for any
//! `Fn(PeerId, Request) -> Future<Result<Response>>` — the real one is
//! [`Node::download_share_multi`](crate::Node::download_share_multi).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;

use anyhow::Context;
use gaggle_core::{ChunkList, ChunkStore, Hash};
use libp2p::PeerId;
use libp2p::futures::StreamExt;
use libp2p::futures::stream::FuturesUnordered;

use crate::proto::{Request, Response};
use crate::transfer::DownloadedShare;

/// Tuning knobs for [`fetch_share_from_swarm`].
#[derive(Debug, Clone)]
pub struct SwarmConfig {
    /// How many chunk requests may be in flight to a single source at once.
    /// Total concurrency is at most `per_peer_parallelism * sources`. `0` is
    /// treated as `1`.
    pub per_peer_parallelism: usize,
    /// Sources to lean on first: while any preferred source that holds a chunk
    /// has spare capacity, the chunk is fetched from it rather than from an
    /// ordinary source. This is the NAS accelerator's "LAN-priority serving"
    /// — a downloader with a fast local replica in `prefer` pulls
    /// almost everything from it and only spills to other peers when it
    /// saturates.
    pub prefer: Vec<PeerId>,
    /// Restrict the download to these manifest paths (a scoped invite). `None`
    /// pulls the whole share; `Some(paths)` fetches chunk lists and chunks only
    /// for those files, and the returned [`DownloadedShare::manifest`] is
    /// narrowed to them — so a request for a file the capability excludes is
    /// never made.
    pub allowed_paths: Option<Vec<String>>,
    /// Which share to ask each source for. `Some(manifest_id)` is required when a
    /// source serves several shares at once (a multi-share relay accelerator);
    /// `None` asks for "the one share you serve".
    pub manifest_id: Option<Hash>,
}

impl Default for SwarmConfig {
    fn default() -> Self {
        Self {
            per_peer_parallelism: 4,
            prefer: Vec::new(),
            allowed_paths: None,
            manifest_id: None,
        }
    }
}

impl SwarmConfig {
    /// [`SwarmConfig::default`] but pulling from `prefer` first.
    pub fn preferring(prefer: impl IntoIterator<Item = PeerId>) -> Self {
        Self { prefer: prefer.into_iter().collect(), ..Self::default() }
    }
}

/// An incremental progress report, delivered once per chunk stored by
/// [`fetch_share_from_swarm_with_progress`].
#[derive(Debug, Clone, Copy)]
pub struct SwarmProgress {
    /// The source that supplied the chunk just stored.
    pub from: PeerId,
    /// Length in bytes of the chunk just stored.
    pub chunk_len: u64,
    /// Chunks verified and stored so far this run.
    pub chunks_done: usize,
    /// Chunks this run has to fetch (already-present chunks are not counted).
    pub chunks_total: usize,
    /// Bytes verified and stored so far this run.
    pub bytes_done: u64,
    /// Bytes this run has to fetch — the sum of the needed chunks' lengths.
    pub bytes_total: u64,
}

/// The outcome of a [`fetch_share_from_swarm`] run.
pub struct SwarmDownload {
    /// The verified manifest and chunk lists. Chunk bytes are in the store that
    /// was passed in.
    pub share: DownloadedShare,
    /// How many chunks each source actually supplied. Its length is the number
    /// of sources that contributed at least one chunk.
    pub chunks_per_source: HashMap<PeerId, usize>,
    /// The order chunks were verified and stored in — rare chunks land near the
    /// front. Useful for tests and progress UIs.
    pub fetch_order: Vec<Hash>,
}

/// Pull the share served by `sources` into `store`, fetching chunks concurrently
/// from all of them and preferring the rarest chunk at each step.
///
/// Chunks already in `store` are not re-fetched, so a partial `store` just tops
/// itself up. Fails only if no source can supply some needed chunk, or if the
/// manifest / a chunk list cannot be obtained or does not verify.
pub async fn fetch_share_from_swarm<F, Fut, S>(
    sources: &[PeerId],
    request: F,
    store: &mut S,
    config: SwarmConfig,
) -> anyhow::Result<SwarmDownload>
where
    F: Fn(PeerId, Request) -> Fut,
    Fut: Future<Output = anyhow::Result<Response>>,
    S: ChunkStore + ?Sized,
{
    fetch_share_from_swarm_with_progress(sources, request, store, config, |_| {}).await
}

/// [`fetch_share_from_swarm`] that also calls `on_progress` once per chunk as it
/// is verified and stored — for driving a progress bar.
pub async fn fetch_share_from_swarm_with_progress<F, Fut, S, P>(
    sources: &[PeerId],
    request: F,
    store: &mut S,
    config: SwarmConfig,
    mut on_progress: P,
) -> anyhow::Result<SwarmDownload>
where
    F: Fn(PeerId, Request) -> Fut,
    Fut: Future<Output = anyhow::Result<Response>>,
    S: ChunkStore + ?Sized,
    P: FnMut(SwarmProgress),
{
    anyhow::ensure!(!sources.is_empty(), "a swarm download needs at least one source");
    let max_load = config.per_peer_parallelism.max(1);
    let prefer: HashSet<PeerId> = config.prefer.iter().copied().collect();

    tracing::info!(sources = sources.len(), "fetching manifest");
    // 1. Manifest and chunk lists, from whichever source answers.
    let mut manifest = match from_any(sources, &request, Request::GetManifest(config.manifest_id))
        .await?
    {
        Response::Manifest(m) => m,
        other => anyhow::bail!("asked for the manifest, got {}", other.kind()),
    };
    manifest.validate().context("a source sent an invalid manifest")?;
    if let Some(want) = config.manifest_id
        && manifest.id() != want
    {
        anyhow::bail!("a source returned a different manifest than the one requested");
    }

    // A scoped invite: narrow the manifest to the files it grants, so we never
    // ask a source for a file it will refuse.
    if let Some(allowed) = &config.allowed_paths {
        let allow: HashSet<&str> = allowed.iter().map(String::as_str).collect();
        manifest.files.retain(|f| allow.contains(f.path.as_str()));
        manifest
            .dirs
            .retain(|d| manifest.files.iter().any(|f| f.path.starts_with(&format!("{d}/"))));
    }

    tracing::info!(files = manifest.files.len(), "fetching chunk lists");
    let mut chunk_lists: BTreeMap<String, ChunkList> = BTreeMap::new();
    for file in &manifest.files {
        let list = match from_any(sources, &request, Request::GetChunkList(file.root)).await? {
            Response::ChunkList(list) => list,
            Response::NotFound => anyhow::bail!("no source has the chunk list for {}", file.path),
            other => {
                anyhow::bail!("asked for the chunk list of {}, got {}", file.path, other.kind())
            }
        };
        list.verify(&file.root, file.size)
            .with_context(|| format!("chunk list for {} failed verification", file.path))?;
        chunk_lists.insert(file.path.clone(), list);
    }

    // 2. The de-duplicated set of chunks we still need, and each chunk's length.
    let mut chunk_len: HashMap<Hash, u32> = HashMap::new();
    let mut needed: Vec<Hash> = Vec::new();
    let mut seen: HashSet<Hash> = HashSet::new();
    for list in chunk_lists.values() {
        for c in &list.chunks {
            chunk_len.insert(c.hash, c.len);
            if seen.insert(c.hash) && !store.contains(&c.hash) {
                needed.push(c.hash);
            }
        }
    }

    let chunks_total = needed.len();
    let bytes_total: u64 = needed.iter().map(|h| u64::from(chunk_len[h])).sum();

    if needed.is_empty() {
        return Ok(SwarmDownload {
            share: DownloadedShare { manifest, chunk_lists },
            chunks_per_source: HashMap::new(),
            fetch_order: Vec::new(),
        });
    }

    // 3. Per-chunk availability from each source's inventory. A source whose
    //    inventory we cannot read is assumed to hold everything (optimistic — a
    //    wrong guess only costs one `NotFound` round trip, after which it is
    //    dropped for that chunk).
    tracing::info!(chunks_needed = needed.len(), "querying source inventories");
    let mut holders: HashMap<Hash, Vec<PeerId>> = HashMap::new();
    for &peer in sources {
        match request(peer, Request::GetInventory).await {
            Ok(Response::Inventory(list)) => {
                let held: HashSet<Hash> = list.into_iter().collect();
                for &hash in &needed {
                    if held.contains(&hash) {
                        holders.entry(hash).or_default().push(peer);
                    }
                }
            }
            other => {
                tracing::warn!(
                    %peer,
                    result = ?other.as_ref().map(Response::kind),
                    "could not read inventory; assuming it holds everything needed"
                );
                for &hash in &needed {
                    holders.entry(hash).or_default().push(peer);
                }
            }
        }
    }
    tracing::info!(sources = sources.len(), chunks = needed.len(), "starting chunk transfer");
    for &hash in &needed {
        if holders.get(&hash).is_none_or(|h| h.is_empty()) {
            anyhow::bail!("no source holds chunk {hash}");
        }
    }

    // Rarest-first: keep `pending` ordered by ascending holder count so
    // `pick_next` finds the rarest assignable chunk near the front.
    let mut pending = needed;
    pending.sort_by_key(|h| holders[h].len());

    // 4. Drive the fetches.
    let mut load: HashMap<PeerId, usize> = sources.iter().map(|&p| (p, 0)).collect();
    let mut in_flight = FuturesUnordered::new();
    let mut chunks_per_source: HashMap<PeerId, usize> = HashMap::new();
    let mut fetch_order: Vec<Hash> = Vec::new();
    let mut bytes_done: u64 = 0;
    let request = &request;

    loop {
        while let Some((idx, hash, peer)) =
            pick_next(&pending, &holders, &load, max_load, &prefer)
        {
            pending.remove(idx);
            *load.get_mut(&peer).unwrap() += 1;
            in_flight.push(async move {
                let result = request(peer, Request::GetChunk(hash)).await;
                (peer, hash, result)
            });
        }

        let Some((peer, hash, result)) = in_flight.next().await else {
            if pending.is_empty() {
                break;
            }
            anyhow::bail!(
                "swarm stalled: {} chunk(s) left but every holder is exhausted",
                pending.len()
            );
        };
        *load.get_mut(&peer).unwrap() -= 1;

        match verify_chunk(result, hash, chunk_len[&hash]) {
            Ok(data) => {
                let chunk_len = data.len() as u64;
                bytes_done += chunk_len;
                store.put(hash, data);
                *chunks_per_source.entry(peer).or_default() += 1;
                fetch_order.push(hash);
                on_progress(SwarmProgress {
                    from: peer,
                    chunk_len,
                    chunks_done: fetch_order.len(),
                    chunks_total,
                    bytes_done,
                    bytes_total,
                });
            }
            Err(ChunkError { reason, drop_source }) => {
                tracing::debug!(%peer, %hash, %reason, drop_source, "chunk fetch failed; re-routing");
                if drop_source {
                    // A transport-level failure: this source is unusable, not
                    // just missing one chunk. Forget it everywhere.
                    for list in holders.values_mut() {
                        list.retain(|p| *p != peer);
                    }
                    if let Some(orphan) =
                        pending.iter().copied().find(|h| holders[h].is_empty())
                    {
                        anyhow::bail!("source {peer} died and no other source has chunk {orphan}");
                    }
                } else {
                    holders.get_mut(&hash).expect("holder list for a needed chunk").retain(|p| *p != peer);
                }
                if holders.get(&hash).is_none_or(|l| l.is_empty()) {
                    anyhow::bail!("every source failed for chunk {hash}: {reason}");
                }
                requeue(&mut pending, &holders, hash);
            }
        }
    }

    Ok(SwarmDownload {
        share: DownloadedShare { manifest, chunk_lists },
        chunks_per_source,
        fetch_order,
    })
}

/// Pick the rarest chunk that can be assigned right now and the best source that
/// holds it. `pending` is kept sorted by holder count, so the first assignable
/// entry is the rarest one; ties break toward the earlier entry. Among a
/// chunk's holders a source in `prefer` always wins over one that is not, then
/// the least-loaded, then the lowest peer id (for determinism).
///
/// Returns `(index into pending, chunk, source)`, or `None` when nothing can be
/// assigned without exceeding `max_load` on some source.
fn pick_next(
    pending: &[Hash],
    holders: &HashMap<Hash, Vec<PeerId>>,
    load: &HashMap<PeerId, usize>,
    max_load: usize,
    prefer: &HashSet<PeerId>,
) -> Option<(usize, Hash, PeerId)> {
    let rank = |p: &PeerId| {
        let not_preferred = usize::from(!prefer.contains(p));
        (not_preferred, load.get(p).copied().unwrap_or(0), *p)
    };
    for (idx, &hash) in pending.iter().enumerate() {
        let peer = holders
            .get(&hash)
            .into_iter()
            .flatten()
            .filter(|p| load.get(*p).copied().unwrap_or(0) < max_load)
            .min_by_key(|p| rank(p));
        if let Some(&peer) = peer {
            return Some((idx, hash, peer));
        }
    }
    None
}

/// Re-insert `hash` into `pending` keeping the ascending-holder-count order.
fn requeue(pending: &mut Vec<Hash>, holders: &HashMap<Hash, Vec<PeerId>>, hash: Hash) {
    let rarity = holders.get(&hash).map_or(0, Vec::len);
    let pos = pending.partition_point(|h| holders.get(h).map_or(0, Vec::len) <= rarity);
    pending.insert(pos, hash);
}

/// Why a chunk fetch did not yield a usable chunk.
struct ChunkError {
    reason: String,
    /// `true` when the whole source should be abandoned (a transport failure or
    /// a source that served corrupt data), `false` when it just does not have
    /// this one chunk.
    drop_source: bool,
}

fn verify_chunk(
    result: anyhow::Result<Response>,
    want: Hash,
    want_len: u32,
) -> Result<Vec<u8>, ChunkError> {
    let bad = |reason: String, drop_source: bool| ChunkError { reason, drop_source };
    let data = match result {
        Ok(Response::Chunk(data)) => data,
        Ok(Response::NotFound) => {
            return Err(bad("source does not have the chunk".into(), false));
        }
        Ok(other) => return Err(bad(format!("expected a chunk, got {}", other.kind()), true)),
        Err(e) => return Err(bad(format!("request failed: {e}"), true)),
    };
    let got = Hash::of(&data);
    if got != want {
        return Err(bad(format!("content-addresses to {got}, expected {want}"), true));
    }
    if data.len() as u64 != u64::from(want_len) {
        return Err(bad(format!("is {} bytes, the list expects {want_len}", data.len()), true));
    }
    Ok(data)
}

/// Try `req` against each source in turn, returning the first `Ok` response.
async fn from_any<F, Fut>(
    sources: &[PeerId],
    request: &F,
    req: Request,
) -> anyhow::Result<Response>
where
    F: Fn(PeerId, Request) -> Fut,
    Fut: Future<Output = anyhow::Result<Response>>,
{
    let mut last_err: Option<anyhow::Error> = None;
    for &peer in sources {
        match request(peer, req.clone()).await {
            Ok(Response::NotFound) => last_err = Some(anyhow::anyhow!("{peer} answered NotFound")),
            Ok(response) => return Ok(response),
            Err(e) => last_err = Some(e.context(format!("asking {peer}"))),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("no sources to ask")))
}

/// Assemble a serving [`Catalog`](crate::Catalog) from a finished
/// [`SwarmDownload`] and the store it filled, so a peer can immediately start
/// re-seeding what it just pulled. `store` may be an in-RAM
/// [`MemoryChunkStore`](gaggle_core::MemoryChunkStore) or an on-disk
/// [`DiskChunkStore`](gaggle_core::DiskChunkStore).
pub fn catalog_from_download(
    share: DownloadedShare,
    store: impl ChunkStore + Send + 'static,
) -> crate::Catalog {
    crate::Catalog::new(share.manifest, share.chunk_lists, store)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(n: u8) -> Hash {
        Hash::of(&[n])
    }

    fn peer(n: u8) -> PeerId {
        // Deterministic distinct peer ids.
        libp2p::identity::Keypair::ed25519_from_bytes([n; 32]).unwrap().public().to_peer_id()
    }

    fn no_prefer() -> HashSet<PeerId> {
        HashSet::new()
    }

    #[test]
    fn pick_next_prefers_the_rarest_chunk() {
        let common = h(1);
        let rare = h(2);
        // `pending` is sorted rarest-first by the caller.
        let pending = vec![rare, common];
        let holders = HashMap::from([
            (common, vec![peer(1), peer(2), peer(3)]),
            (rare, vec![peer(1)]),
        ]);
        let load = HashMap::from([(peer(1), 0), (peer(2), 0), (peer(3), 0)]);

        let (idx, hash, _) = pick_next(&pending, &holders, &load, 4, &no_prefer()).unwrap();
        assert_eq!((idx, hash), (0, rare));
    }

    #[test]
    fn pick_next_skips_a_chunk_whose_only_holder_is_saturated() {
        let rare = h(1);
        let common = h(2);
        let pending = vec![rare, common];
        let holders = HashMap::from([
            (rare, vec![peer(1)]),
            (common, vec![peer(1), peer(2)]),
        ]);
        // peer(1) is at capacity, so `rare` cannot go out this round.
        let load = HashMap::from([(peer(1), 4), (peer(2), 0)]);

        let (_, hash, chosen) = pick_next(&pending, &holders, &load, 4, &no_prefer()).unwrap();
        assert_eq!(hash, common);
        assert_eq!(chosen, peer(2));
    }

    #[test]
    fn pick_next_load_balances_across_holders() {
        let chunk = h(1);
        let pending = vec![chunk];
        let holders = HashMap::from([(chunk, vec![peer(1), peer(2)])]);
        let load = HashMap::from([(peer(1), 3), (peer(2), 1)]);

        let (_, _, chosen) = pick_next(&pending, &holders, &load, 4, &no_prefer()).unwrap();
        assert_eq!(chosen, peer(2), "should pick the less-loaded holder");
    }

    #[test]
    fn pick_next_returns_none_when_every_holder_is_saturated() {
        let chunk = h(1);
        let pending = vec![chunk];
        let holders = HashMap::from([(chunk, vec![peer(1), peer(2)])]);
        let load = HashMap::from([(peer(1), 4), (peer(2), 4)]);

        assert!(pick_next(&pending, &holders, &load, 4, &no_prefer()).is_none());
    }

    #[test]
    fn pick_next_takes_from_a_preferred_source_even_when_it_is_busier() {
        let chunk = h(1);
        let pending = vec![chunk];
        let holders = HashMap::from([(chunk, vec![peer(1), peer(2)])]);
        // peer(2) is idle, peer(1) is loaded — but peer(1) is the LAN replica.
        let load = HashMap::from([(peer(1), 3), (peer(2), 0)]);
        let prefer = HashSet::from([peer(1)]);

        let (_, _, chosen) = pick_next(&pending, &holders, &load, 4, &prefer).unwrap();
        assert_eq!(chosen, peer(1), "preferred source wins until it saturates");

        // Once peer(1) is at capacity the fetch spills to peer(2).
        let load = HashMap::from([(peer(1), 4), (peer(2), 0)]);
        let (_, _, chosen) = pick_next(&pending, &holders, &load, 4, &prefer).unwrap();
        assert_eq!(chosen, peer(2));
    }

    #[test]
    fn requeue_keeps_pending_sorted_by_rarity() {
        let a = h(1); // 1 holder
        let b = h(2); // 2 holders
        let c = h(3); // 3 holders
        let holders = HashMap::from([
            (a, vec![peer(1)]),
            (b, vec![peer(1), peer(2)]),
            (c, vec![peer(1), peer(2), peer(3)]),
        ]);
        let mut pending = vec![a, c];
        requeue(&mut pending, &holders, b);
        assert_eq!(pending, vec![a, b, c]);
    }
}
