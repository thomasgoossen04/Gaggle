# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

**Keep `README.md` current.** It's the user-facing overview + quick-start guide (what
Gaggle is, its feature list, install/build instructions, and short usage walkthroughs for
sharing/joining/accelerating). Whenever a change here would change what a user reads
there — a new or renamed feature, a changed install/release flow, a different GUI
workflow for something the README walks through — update `README.md` in the same change.
It stays high-level and user-facing; this file stays the detailed dev-facing one.

## What this is

Gaggle is a hybrid-P2P application for sharing very large folders (100GB+, e.g. modded
game installs) over private, invite-based swarms. Peers exchange content-addressed
chunks directly; optional always-on **accelerator nodes** improve availability and
throughput. The full design and the milestone roadmap live in `notes/plan.md` (that
directory is git-ignored — read it, don't rely on it being present for others).

Milestone 1 (the `gaggle-core` data model — chunking, Merkle trees, manifest, dedup)
is implemented and tested. `gaggle-core` ships four `ChunkStore`s: `MemoryChunkStore`
(a plain map), `LruChunkCache` (byte-budgeted, LRU eviction — the relay's hot cache),
`DiskChunkStore` (durable, one sharded file per chunk — the NAS replica;
`open_with_opts(dir, compress)` stores each chunk zstd-compressed when that
shrinks it, as `<hex>.zst`, so a store is self-describing per chunk and raw +
compressed chunks coexist with no migration), and
`SourceChunkStore` (streaming seed — no bytes retained: it reads each requested chunk
from the original files on disk, verifies it, and holds it in a bounded `LruChunkCache`).
`snapshot::index_dir` is `snapshot_dir` without the `store.put`: it returns the same
manifest + chunk lists plus a `Hash -> ChunkLocation {path, offset, len}` map to feed a
`SourceChunkStore`, so seeding a 100 GB folder costs a bounded RAM cache and no second
on-disk copy.
`snapshot::write_share` is `snapshot_dir`'s inverse: materialize a share's files from
any store back onto disk. `snapshot::sync_share` is its delta form (milestone 10):
given the old + new manifests it rebuilds only added/changed files, deletes removed
ones, and prunes emptied dirs.

The scan (`snapshot::scan_tree`, behind `snapshot_dir` / `index_dir`) runs the
CPU-bound work — FastCDC + BLAKE3 over every file — on the **rayon** pool: files
in parallel via `par_iter`, and each chunk-sized buffer tree-hashed in parallel
too (`Hash::of` switches to `blake3::Hasher::update_rayon` at/above a 128 KiB
`RAYON_HASH_THRESHOLD`, and degrades to inline recursion when the pool is already
busy, so the two levels compose without oversubscription — one huge file fills
the pool per-chunk, a folder of many files fills it per-file). Results funnel
back to the calling thread through a **bounded `sync_channel`** (`~2 ×
current_num_threads` in-flight chunks — peak RAM stays bounded regardless of
folder size), where the caller's non-`Send` sinks (`ChunkStore::put`, the
progress callback, the `Hash -> ChunkLocation` map) stay single-threaded. Files
finish out of walk order, so `manifest.files` is pushed unsorted and
`canonicalize()` sorts it; `files_done` / `bytes_done` are still monotonic
because they are counted on the collecting thread. For a deduped chunk shared
across files, *which* file's `ChunkLocation` is recorded is now
order-nondeterministic — fine, any occurrence serves identical bytes.

Milestone 7 (private swarms) adds `identity` + `invite` to `gaggle-core`: a per-share
Ed25519 `ShareKeypair` / `SharePublicKey`, and a bearer `Capability` (`Scope::All` or
`Scope::Files`, optional expiry) the origin signs into a `SignedCapability`. An `Invite`
bundles the share key, manifest id, name and credential and round-trips through a
`gaggle1<base64url>` string.

Milestones 2–7 (`net` + `control-plane` + `accelerator`) are implemented and tested:

- **Chunk transfer** — a libp2p **request-response over QUIC** protocol (`proto`,
  `codec`, `transfer::fetch_share`) that pulls a share and verifies every chunk
  against the manifest root. `transfer::fetch_manifest_and_lists` fetches just the
  metadata. The serving side is `Catalog` (store type-erased, so a peer serves from
  RAM and a NAS from disk through the same type); it may hold a *partial* store and
  reports what it has via `Request::GetInventory`. Every `Response::Chunk`'s bytes
  pass through `wire_crypto::{seal, open}` right where `codec.rs` frames them —
  compressed with `lz4_flex` when that shrinks the chunk, always sealed with
  `XChaCha20Poly1305` under a fixed, binary-embedded key — one chunk at a time, so
  this never needs a whole-share pass before streaming starts. It sits on top of,
  not instead of, QUIC's own TLS 1.3; the key is not a secret from anyone who has
  the binary and does not gate private shares (that's still `invite`/`Capability`) —
  it only keeps raw file bytes off the wire in the clear and shrinks compressible
  chunks. Every other layer (verification, the relay cache, `DiskChunkStore`) only
  ever sees plaintext, since `codec.rs` reverses it on read. `codec.rs` runs the
  chunk `seal` / `open` on `tokio::task::spawn_blocking`, not inline on the libp2p
  swarm task — that per-chunk CPU pass would otherwise serialize every peer a node
  serves (and every landing chunk a node downloads) onto the one core the swarm
  event loop runs on; the tiny non-chunk frames still encode inline.
- **`Node`** — a standard peer: the chunk protocol wired together with a **Kademlia**
  DHT (`ShareKey` = `Manifest::id`; `provide` / `find_providers`), **identify**, **mDNS**,
  **UPnP**, a **relay client** and **dcutr**. mDNS (`libp2p::mdns::tokio`, deliberately
  skips loopback interfaces) finds same-LAN peers within milliseconds with zero DHT
  round trip and no NAT/relay concerns — the fastest and most reliable path when it
  applies. It resolves a route to a discovered peer (routing
  table → learned addresses → `get_closest_peers`) before requesting, and reaches
  NAT'd peers through a relay circuit, upgrading to a direct connection when dcutr's
  hole-punch lands. The UPnP behaviour (`libp2p::upnp::tokio`) tries to map the QUIC
  port on the gateway as soon as it listens; a successful mapping becomes a confirmed
  external address (`NodeEvent::ExternalAddressConfirmed`) that identify then reports
  to peers, so two UPnP-capable devices on plain home routers connect directly with
  **no relay involved at all**. It's opportunistic, not a replacement for relay/dcutr —
  it does nothing behind a router with UPnP disabled, double NAT, or CGNAT, so those
  cases still need the relay fallback.
- **Multi-peer swarming** (`swarm::fetch_share_from_swarm`, `Node::download_share_multi`)
  — pulls one share from several sources at once. Queries each source's inventory,
  builds a per-chunk availability map, and schedules chunk requests **rarest-first**
  (fewest holders first) with a per-peer concurrency cap so load spreads across
  sources. Every chunk is still verified against the manifest; a failed or
  chunk-less source is re-routed around (transport failures drop the source
  entirely, a `NotFound` only for that chunk). The per-chunk `verify_chunk`
  (`Hash::of` over up to 16 MiB) runs on `tokio::task::spawn_blocking` inside each
  in-flight fetch future, so many chunks content-address in parallel while the
  driver loop keeps issuing requests — instead of each landing chunk stalling the
  next on one core. `SwarmConfig::prefer` /
  `Node::download_share_multi_preferring` biases the scheduler toward given sources
  first — the NAS's "LAN-priority" knob. `SwarmDownload` reports per-source chunk
  counts and fetch order. `pick_next` / `requeue` are pure and unit-tested.
- **`RelayNode`** — the accelerator's relay role (milestones 3 & 5): a libp2p relay
  server + a Kademlia bootstrap server, **plus a read-through hot-chunk cache**. After
  `cache_share(manifest, lists, upstreams)` it answers `GetChunk` from an
  `LruChunkCache`; on a miss it fetches from an upstream seed, verifies, caches
  (evicting the coldest chunk if over `RelayConfig::cache_capacity_bytes`), and
  forwards — so N downloaders cost the origin one fetch per hot chunk. `cache_stats()`
  exposes hits/misses/evictions/bytes, plus `bytes_served` — cumulative chunk bytes
  forwarded to downloaders (cache hit *or* miss-then-fill), the relay's upload-throughput
  signal. A plain `Node` counts the same thing through its `Catalog` (`ServeStats {
  bytes_served, chunks_served }`, read back with `Node::serve_stats()`); `Catalog::answer`
  bumps the counters at the single `GetChunk` choke point every served chunk passes.
- **NAS replica** (milestone 6) — no new node type: `Node::download_share_multi` into a
  `DiskChunkStore`, then `Node::serve(Catalog::new(manifest, lists, disk))`. The disk
  store dedups and skips chunks already present, so replication resumes after a
  restart, and the replica keeps serving with the origin offline.
- **Private swarms** (milestone 7) — a new `Request::Hello(SignedCapability)` /
  `Response::Welcome` / `Response::Unauthorized`. `Node::restrict_to_invite_holders` /
  `RelayNode::restrict_to_invite_holders` flip a served share private: every request is
  refused until a connection has presented a valid capability (checked against the
  share key + current manifest id + expiry), and `GetChunkList` / `GetChunk` /
  `GetInventory` are then filtered by the capability's `Scope`. The downloader side is
  `Node::authenticate` / `authenticate_all` before a `download_*`. Grants are
  per-connection and dropped on disconnect. A scoped download passes
  `SwarmConfig::allowed_paths` so `fetch_share_from_swarm` narrows the manifest to the
  granted files up front and never asks a source for one it would refuse; `app-state`
  derives it from the subscription's credential.
- **`control-plane`** gets its first real code: `invite::{InviteRegistry, router,
  InviteClient}` — an in-memory HTTP service to `POST /invites` (rejecting
  bad-signature invites) and `GET /invites/{code}`.
- **`accelerator` binary** is a config-file + admin-API daemon (see "Remote,
  multi-share accelerators" below): `accelerator run [--role relay|nas]
  [--cache-mib N] [--dir <path>] [--no-compress-replica] [--admin-listen host:port]
  [--listen <maddr>]`,
  plus `accelerator identity` / `authorize <hex>` / `share {add|rm|ls}`. State
  lives under `--home` / `$GAGGLE_ACCEL_HOME` / the per-OS data dir (`dirs::data_dir()`
  — `~/.local/share` Linux, `~/Library/Application Support` macOS, `%APPDATA%` Windows)
  `+ /gaggle/accelerator` (`identity.key` + `config.toml`).

Tests: `crates/net/tests/loopback.rs` (direct transfer), `discovery.rs` (DHT +
relay/dcutr), `swarm.rs` (multi-seed load spread, partial-seed stitching, dead-source
re-routing, re-seeding), `accelerator.rs` (relay cache shields the origin / evicts under
budget; NAS durability across restart, resumed replication, LAN priority),
`private.rs` (no-invite refusal, whole-share download, per-file scope, expiry, wrong
key, invite-URL round trip, private relay). `wire_crypto`'s own unit tests cover the
compress-or-not fallback, a fresh nonce per seal, and tamper/truncation rejection —
every other `net` test above already exercises it implicitly, since every chunk
that crosses the wire goes through it. `control-plane/tests/invite_exchange.rs`
round-trips an invite through a live server. Plus the
`cargo run -p net --example loopback_transfer` demo
(`serve [seed]` / `mint-invite` / `fetch [invite]` / `fetch-swarm`).
Peer `Node`s deliberately do **not** `add_external_address` their own loopback addr —
that would make the swarm swallow the identify address candidates dcutr needs.
`Node`/`RelayNode::reachable_addrs` drop any wildcard `0.0.0.0`/`::` entry
(`net::addr_is_unspecified`) — a node listening on `/ip4/0.0.0.0/...` can surface
the bind address itself on kernels where `if-watch` doesn't enumerate concrete
interfaces (containers, some NAS boxes), and libp2p-quic rejects a dial to it with
`MultiaddrNotSupported`. The accelerator daemon additionally strips loopback
(`net::addr_is_loopback`) from what it announces to an *external* tracker /
publishes in a rendezvous answer (kept only for its own in-process tracker, which
a same-machine downloader may use); `app-state::merge_tracked_sources` also skips
unspecified addresses a tracker hands back.

Milestones 8–10 (GUI v1/v2 + delta sync) are implemented and tested:

- **`app-state`** — the headless, UI-framework-agnostic transfer manager. `App` is a
  sync, thread-safe handle (callable from a GUI thread with no tokio runtime); all the
  async lives on a background task `App::new` spawns. It owns the `net` nodes: one
  shared downloading `Node`, one serving `Node` per local share (`Arc<Node>`, so a
  rescan can re-`serve` in place). `App::add_local_share` indexes a folder (off-thread,
  `index_dir`) and seeds it through a `SourceChunkStore` — chunks stream from the source
  files on demand, capped by `Settings::seed_cache_bytes` (default 256 MiB, floor 32 MiB)
  of hot-chunk cache; `App::add_private_share` also mints a per-share `ShareKeypair` and calls
  `restrict_to_invite_holders`, and `App::mint_invite(id, Scope, expiry)` hands back a
  `gaggleshare1…` token in `AppState::minted_invite`. `App::subscribe(SubscribeRequest)`
  pulls a remote share into a `DiskChunkStore` under the download dir (so pause = abort,
  resume = top up), then `write_share`s the tree out. Progress rides
  `Node::download_share_multi_with_progress` (`SwarmProgress` per chunk).
- **Seed-after-download** — when `Settings::seed_after_download` is set (default true),
  a finished download does not go idle: `Command::DownloadDone` indexes the just-written
  output tree (`index_dir` + a streaming `SourceChunkStore`, same as a local seed) and
  stands up its own serving `Node`, so the peer contributes upload back to the swarm and
  shows up in the seeder tracker. State lives on `SubEntry::seed` (`Option<CompletedSeed>`)
  and `TransferRow::seeding`; `Manager::tick`'s tracker-announce / rendezvous-answer loops
  and `sample_local_stats` all fold these nodes in alongside origin seeds. `App::pause(id)`
  on a completed download stops its serving node, best-effort withdraws it from the tracker
  (`DELETE /tracker/{id}/{peer}` — otherwise it lingers a full entry TTL), and records the
  manifest id in `Manager::paused_seeds` (persisted in `shares.json` as `paused_seeds`);
  `App::resume(id)` re-serves. Flipping `seed_after_download` off stops every completed
  seed; flipping it on starts every one not individually paused. The GUI Transfers tab
  gets a per-row Start/Pause-seeding button and a `seeding` chip; Settings → Startup gets
  the global toggle. A restart re-runs the subscription, which re-completes and re-seeds
  (unless paused) — no separate persisted seed record needed.
- `Manager::remove` / a stopped local NAS / `accel_remove_share` now also best-effort
  withdraw from the seeder tracker (`Manager::withdraw_from_tracker`), and
  `announce_to_tracker` strips loopback from what it publishes (matching the standalone
  daemon) — the tracker is the cross-machine discovery path, so a `127.0.0.1` entry there
  is a dead dial for other peers and points at the wrong process on the announcing machine.
- **Delta sync (milestone 10)** — `App::rescan_share(id)` re-indexes a seeded folder
  (`index_dir`, same streaming `SourceChunkStore`), bumps `Manifest::version`, and re-serves. `App::check_updates(id)` fetches just the
  remote manifest (`Node::fetch_manifest`) and flags `TransferRow::update_available` when
  it is newer; `App::resync(id)` re-chunks the existing output tree into a store, tops it
  up with only the delta chunks, then applies `gaggle_core::sync_share` (writes
  added/changed files, deletes removed ones, prunes emptied dirs). `Settings::auto_resync_secs`
  turns on a background poll that only *flags* newer versions. A private share pins its
  manifest id in every capability, so a rescanned private share needs a fresh invite.
- **Accelerator control (milestone 9)** — `App::start_accelerator(AcceleratorRequest::{Relay,Nas})`
  runs an in-process `RelayNode` or NAS replica set carrying a *list* of `ShareLink`s
  (see "Remote, multi-share accelerators" below), surfaced as `AppState::accelerator`
  (role, peer id, listen addr, live `CacheStats` / replica chunk count, per-share rows).
  Each `AccelShareRow` also carries `disk_bytes` (replica footprint, refreshed by a
  ~10 s off-thread walk in `Manager::tick`), `replica_path` (local NAS only), and
  `seeding`. `App::accel_set_seeding(id, on)` pauses/resumes one NAS share —
  pause shuts its serving `Node` down but keeps the replica dir + token + update
  polling (persisted as `PersistedAccelerator::paused_shares` / restored via
  `AcceleratorRequest::Nas::paused`); resume re-runs the add path (a cheap top-up).
  `App::accel_remove_share(id)` now **deletes** a NAS share's on-disk replica dir
  (the GUI confirms first); the in-process NAS always stores the replica
  zstd-compressed (`manager::COMPRESS_REPLICA`). `App::benchmark()` measures
  sequential write throughput to the download volume plus free space (`statvfs` on
  unix) and suggests a role.
**Remote, multi-share accelerators** — accelerators are no longer bound to one
share, and can be driven remotely:

- **Multi-share `RelayNode`** — `cache_share` appends (keyed by manifest id),
  `remove_share` / `shares` manage the set, and `restrict_to_invite_holders(share,
  manifest_id)` now gates *one* cached share, so a relay carries any mix of public
  and private shares. `Request::GetManifest(Option<Hash>)` selects a share on a
  multi-share source; `Node::download_share_selecting` / `SwarmConfig::manifest_id`
  are the downloader side (a plain download still sends `None`). `net::accel::{relay_add_share,
  nas_add_share}` are the per-share start helpers shared by the daemon and `app-state`.
- **Persistent identity** — `net::load_or_create_identity(path)` +
  `Node::spawn_*_with_identity` / `RelayNode::spawn_with_opts` keep a stable
  `PeerId` across restarts. `net::{keypair_from_seed, identity_seed}` derive
  sibling identities (NAS uses one per share). `gaggle_core::{AgentKeypair,
  AgentId}` is a general Ed25519 signer (sibling of the per-share `identity` module).
- **`control-plane::admin`** — `router(AdminState)` serves `GET /admin/status`,
  `GET|POST /admin/shares`, `POST /admin/shares/{id}` (`{"seeding":bool}` —
  `AdminClient::set_share_seeding`, pause/resume serving a share without
  forgetting it: token + NAS replica kept), `DELETE /admin/shares/{id}` (add
  `?keep_data=1` to
  keep a NAS replica's bytes for a resume; the default and `AdminClient::remove_share`
  delete them — `remove_share_keep_data` opts out; the flag rides only the URL,
  not the signed canonical path). `ShareStatus.seeding: bool` (`#[serde(default)]`
  true) reports whether a share is currently served. Every request is signed
  by the operator's `AgentKeypair` (canonical `METHOD\nPATH\nTS\nNONCE\nblake3(body)`,
  headers `x-gaggle-agent|timestamp|nonce|signature`, ±60 s skew, checked against
  `AdminState.authorized`); every response is signed by the daemon key
  (`x-gaggle-daemon[-signature]`) so `AdminClient` TOFU-pins it. Mutations go out
  an `mpsc<AdminCommand>` (`RemoveShare { manifest_id, keep_data }`); status comes
  in a `watch<DaemonStatus>` — no `net` dep (`AdminCommand::{AddShare, RemoveShare,
  SetSeeding}`). `ShareStatus.disk_bytes` reports the replica's on-disk footprint.
  `DaemonStatus.bytes_served_total: Option<u64>` (additive, `skip_serializing_if`)
  carries the daemon's cumulative served bytes so a client can diff successive polls
  into an outbound-throughput graph.
- **`accelerator` daemon** — `config.rs` (`AcceleratorConfig` toml: role, listen,
  admin_listen, cache_mib, replica_dir, `compress_replica` (default true; `accelerator
  run --no-compress-replica` opts out — NAS stores the replica zstd-compressed on
  disk), `authorized_keys`, `shares`, `paused_shares` (manifest-id hex of shares
  kept in config + on disk but not served until resumed), `rendezvous_url` +
  `public_relay` (see below)),
  `supervisor.rs` (`Supervisor` owns a multi-share `RelayNode` **or** a
  `HashMap<Hash, Node>` of per-share replicas; applies `AdminCommand`s and
  rewrites `config.toml`; `RemoveShare` deletes the replica dir unless `keep_data`;
  `SetSeeding` moves a share to/from a `ShareRecord::Paused` (shuts its serving
  node / drops it from the relay cache, keeps the token + on-disk replica; a
  paused share isn't re-replicated on boot);
  `refresh_served()` reads the relay's `cache_stats().bytes_served`
  or the sum of each NAS node's `serve_stats()` into `last_served` (and each NAS
  replica's on-disk size into `disk_bytes`) before every
  `publish()`, incl. on the 30 s tracker-announce tick), `run.rs` (identity + config +
  supervisor + admin server). Prints its peer id + public key on every start.
  Offline `accelerator share rm <manifest-id>` also deletes the replica dir.
- **Daemon as a rendezvous/tracker/relay *client*** (`config.rendezvous_url` +
  `config.public_relay`, `accelerator run --rendezvous-url <url> --public-relay
  <maddr>`) — without these a standalone daemon only *hosts* rendezvous/tracker
  endpoints for others; it never registers itself, so a NAS replica behind NAT
  (Tailscale-only, no port-forward) is unreachable from a downloader that isn't
  on the same overlay, even with a public relay in the mix. With `rendezvous_url`
  set (point it at the *same* accelerator the downloaders use — typically the
  public relay's control-plane URL) `Supervisor` (a) answers NAT-rendezvous
  punch requests aimed at every served share on a fast 2 s tick
  (`answer_punch_requests`, NAS role only — a relay is a libp2p relay server,
  assumed publicly reachable), and (b) also announces every ready share to that
  *external* tracker over HTTP (`TrackerClient`) alongside its own in-process
  one, so `merge_tracked_sources` on a downloader pointed at the relay discovers
  the daemon. With `public_relay` set, each NAS serving node
  `reserve_relay_circuit`s a `/p2p-circuit/…` address on boot and advertises it
  in the tracker announce (`ShareRecord::Ready::circuit_addr`), so the replica is
  dialable through the relay with dcutr upgrading to direct. `app-state`'s
  `run_download` now merges tracked sources *before* the NAT punch and punches
  every distinct peer id it then has (not just the origin in the link), so a
  tracker-discovered NAS replica gets a punch too.
- **`app-state`** — `App` keeps a persistent operator `AgentKeypair` at
  `operator.key` (`App::operator_public_key()`). `AcceleratorRequest::{Relay,Nas}`
  take `shares: Vec<ShareLink>` (Nas also `paused: Vec<String>`);
  `AcceleratorState.shares: Vec<AccelShareRow>`;
  `App::accel_add_share` / `accel_remove_share` (deletes a NAS replica dir) /
  `accel_set_seeding` mutate a *running* local
  accelerator. `Settings.remote_accelerators: Vec<RemoteAccelerator>` (label +
  admin URL + pinned `daemon_key`); `App::{add,remove}_remote_accelerator` /
  `remote_{add,remove}_share` / `remote_set_share_seeding` (pause/resume one
  share on a remote daemon); the manager polls each every ~10 s via `AdminClient`
  into `AppState.remote_accelerators: Vec<RemoteAccelState>`.
- **Throughput history (`app-state/src/stats.rs`)** — `SpeedSample { at, down_bps,
  up_bps }` + a capped `SpeedHistory` ring (~1 h at the 2 s tick). `Manager::tick`
  samples the local rates: download = sum of active `TransferRow::speed_bps`; upload =
  a diff of the summed cumulative served-bytes counters across every seed `Node` + any
  in-process NAS node + a running `RelayNode` (gathered off-thread, fed back as
  `Command::ServedTotalSample`). Each remote's `Command::RemoteStatusRefresh` carries
  `DaemonStatus.bytes_served_total`, diffed per label into an `up_bps`-only history.
  All of it is exposed always-on (not gated on the GUI) via
  `AppState.stats: StatsSnapshot { local: Vec<SpeedSample>, accelerators: Vec<AccelStatsRow
  { label, history }> }`. `stats::rate_from_cumulative` is the pure diff helper (unit-tested).
  `stats::resample` (also pure/unit-tested) monotone-cubic-interpolates the raw ~2 s
  samples onto a fixed 90-point grid anchored to a live `now` — the Stats graphs call it
  every 200 ms redraw so the line glides between readings instead of freezing then
  jumping once per sample, and the fixed point count keeps the categorical x-axis from
  folding a wide window's repeated labels onto one position.
- **`ShareLink`** moved from `app-state` to **`net`** (`net::ShareLink`, re-exported
  by `app-state`); `into_request()` is now `From<ShareLink> for SubscribeRequest`
  in `app-state`.
- **`gui`** Accelerator tab: an operator-key card (copy → `accelerator authorize
  <key>`), the local form now taking one link per line + a running-status card
  listing every carried share with a right-aligned action cluster — a
  SEEDING/PAUSED toggle (local NAS *and* every remote share) + Remove — and an
  "Add share" field, and a "Remote accelerators" section (reachable dot, role,
  per-share rows, "Add remote" form + per-remote "Add share"). `accel_share_row`
  keeps its info column `flex_1 min_w_0` (truncating) so the actions never get
  pushed off the card and clipped.
- **NAT rendezvous ("ICE-lite")** — a relay-free path for two peers with no shared
  network path and no working UPnP: `control_plane::rendezvous` (`PeerInfo`,
  `RendezvousRegistry`, `router`, `RendezvousClient`) is a small, unauthenticated,
  in-memory HTTP mailbox keyed by the *origin*'s libp2p peer id — a subscriber
  `POST /rendezvous/{origin}`s its own candidate addresses and gets a `request_id`
  back, the origin polls `GET /rendezvous/{origin}/pending`, dials the subscriber's
  addresses itself (`Node::bootstrap`, timeout-bounded so a dead address can't stall
  the exchange) and `POST`s its own addresses as the answer, and the subscriber
  polls `GET /rendezvous/{origin}/{request_id}` until it sees that answer and dials
  it the same way — each side's outbound dial is what opens its own NAT pinhole for
  the other's inbound one, so no chunk data (or even a relay circuit) ever touches
  the accelerator. `control_plane::serve_daemon` merges this router onto the same
  listener as the signed admin API (unauthenticated — any subscriber may need it,
  not just the operator), so any already-running accelerator (relay or NAS) is a
  rendezvous point for free. `app-state`'s `Settings.rendezvous_url` points at one;
  `Manager::tick` answers pending requests for every locally-seeded share once per
  tick, and `run_download` tries a rendezvous punch (bounded by `RENDEZVOUS_TIMEOUT`,
  currently 8s) before falling back to whatever's already in the share link/relay.
  `net::peer_id_of` (made `pub`) and `Node::spawn_with` (the download-only sibling of
  `spawn_serving_with`, for a caller that wants an explicit listen address without
  serving anything) support this from the `net` side.

- **Seeder tracker** — `control_plane::tracker` (`TrackerRegistry`, `router`,
  `TrackerClient`, re-exported at the crate root) is the discovery half of the
  same idea: a small, unauthenticated, in-memory directory keyed by a share's
  **manifest id (hex)** that answers "who else is serving this?". A peer serving
  a share `POST /tracker/{manifest_id}`s a `SeederAnnounce` (its `PeerInfo`
  flattened + optional `name` + `private` flag; a bare `PeerInfo` still
  deserializes) (entry TTL 150s, so a
  gone seed drops itself); a downloader `GET /tracker/{manifest_id}` once and
  swarms across everyone it gets back plus the addresses in its share link; a
  clean shutdown can `DELETE /tracker/{manifest_id}/{peer_id}`. It fixes the
  "download only pulled from one source" case where a share link names just the
  origin even though a NAS replica (or a second origin) also has the whole share
  — the link is static, the tracker is live. `serve_daemon` merges this router
  onto the same listener(s) as `rendezvous` (unauthenticated, same trust model),
  so any running accelerator is a tracker for free; it never sees chunk data,
  and every discovered chunk is still verified against the manifest root.
  `app-state` reuses `Settings.rendezvous_url` as the tracker URL too:
  `Manager::tick` re-announces every locally-served share (origin seeds +
  in-process NAS replicas + relay-cached shares + **completed downloads that
  are still seeding**), now tagged with its name +
  private flag, every `TRACKER_ANNOUNCE_INTERVAL`
  (30s), and `run_download` / `run_resync` / `check_remote_version` merge the
  tracker's seeder list into their sources up front (`merge_tracked_sources`,
  bounded by `TRACKER_QUERY_TIMEOUT`, 4s). `announce_to_tracker` strips loopback
  addresses (`net::addr_is_loopback`) — the tracker is the cross-machine path, so
  a `127.0.0.1` entry is a dead dial for other peers and resolves to the wrong
  process on the announcing box. When a share stops being served — `Manager::remove`,
  a paused completed-download seed, `accel_remove_share` — `withdraw_from_tracker`
  fires a best-effort `DELETE /tracker/{id}/{peer}` so it drops out immediately
  instead of lingering a full entry TTL (150s). The standalone `accelerator` daemon's
  `Supervisor` shares one `TrackerRegistry` with its HTTP router and announces
  every ready share into it directly (in-process, no round trip — name/private
  from `link_meta(token)`), so a
  daemon-run relay/NAS is discoverable the same way. Keyed by manifest id, so a
  post-rescan share whose id changed simply returns nothing extra until the new
  id propagates — the share link's own source still carries it.
- **Open share directory** — `GET /tracker` (no id) lists every *public* share
  the tracker currently knows a live seeder for, as `ShareDirEntry { manifest_id,
  name, seeders }`, name-then-id ordered; `private: true` announces are tracked
  (invite holders still swarm across replicas via the keyed query) but never
  listed. `TrackerClient::directory()` is the client side. `App::refresh_directory()`
  fetches it into `AppState::discovered_shares: Vec<DiscoveredShare>` (empty when
  no `rendezvous_url`); `App::subscribe_discovered(manifest_id, name)` resolves
  that share's seeders from the tracker and hands the result to the normal
  subscribe path — a public share is joinable with **no share link at all**. The
  `gui` Transfers tab has a "Browse public shares" toggle
  (`Gaggle::show_directory`) that renders the directory with a per-row Download
  button (or a "joined" chip for shares already in the transfer list).

- **`gui`** — a gpui shell over `App`: Shares (add public / private folder, copy link,
  rescan, per-row ▸ panel with the invite form), Transfers (progress bars,
  pause/resume/remove, check-updates/resync, `update vN` badge, per-row ▸ swarm
  inspector = per-source chunk/byte breakdown, plus a "Browse public shares"
  toggle listing the tracker's open directory with per-row Download), Accelerator (benchmark → suggested role
  → start relay / NAS → live status), Stats (download/upload `gpui_component::chart::LineChart`s
  over a 1m/5m/15m/1h window — ephemeral `Gaggle::stats_window`; a "Local" / per-remote
  source dropdown — `Gaggle::stats_source: StatsSource`; reads `AppState.stats`, no polling
  of its own), and an editable Settings form. It polls
  `App::snapshot()` on a 200 ms `Timer` and re-renders; it never touches `net`. Raw gpui
  for layout/interaction, plus `gpui_component::{init, v_flex, window_border}` and
  `gpui_component::input::{Input, InputState}` for the form fields.
  **Advanced mode** (`Settings::advanced_ui`, `#[serde(default)]` false, toggled from the
  Settings → Interface card): off = only the Transfers / Shares / Stats / Settings tabs,
  and the Reachability card is a single "Paste reachability link" button
  (`Gaggle::paste_reachability`); on = also the Accelerator + Logs tabs
  (`chrome::header` + `views::body` both gate on the flag), and the Reachability card
  shows the editable `public_relay` / `rendezvous_url` fields plus a "Copy as link"
  button (`Gaggle::copy_reachability`). The link is `app_state::ReachLink` (`reach.rs`):
  `{public_relay, rendezvous_url}` postcard-encoded behind a `gagglenet1` prefix, same
  shape as `net::ShareLink` — the short, copy-pasteable way to move reachability config
  between devices.

Tests: `app-state/tests/transfer_manager.rs` runs real loopback transfers through two
`App`s — share→subscribe→complete with byte-exact output, incremental progress events,
pause keeps partial + resume finishes, settings survive a restart, removing a seed makes
later subscribers fail, **rescan→check_updates→resync applies only the delta** (added
file arrives, removed file is deleted, `version` bumps), **a private share refuses a
strangers then admits a minted invite**, **benchmark reports throughput + free space**,
**a NAS accelerator replicates a share**, **a NAS share pauses (replica kept on
disk) / resumes / and Remove deletes the replica dir**, **a seed streams a folder
several times its
`seed_cache_bytes` budget from disk and still serves every chunk**, **a finished
download keeps seeding — a third peer pulls the whole share from only the first
leech's node, and the per-row pause/resume control stops and restarts it**, **a real
loopback transfer leaves the seeder with a non-zero-`up_bps` `stats.local` history and
both ends with a growing sample count**. `app-state` unit
tests cover `Settings`
persistence, `ShareLink` round trips, name sanitizing, and `stats::{SpeedHistory,
rate_from_cumulative, resample}` (capping, windowing, counter/clock resets; and that
`resample` holds a fixed point count, stays within the sample range, and advances the
curve when only `now` moves).
`crates/net/src/catalog.rs` unit-tests that `ServeStats` counts only `GetChunk` hits;
`crates/net/tests/accelerator.rs`'s relay-cache test asserts `CacheStats::bytes_served`
tracks both forwarded and cache-hit chunks (doubling on a second full pull);
`crates/control-plane/tests/admin.rs` asserts `DaemonStatus.bytes_served_total`
round-trips the signed status response and reflects a later value. **`relay_accelerator_carries_multiple_shares`**
starts a local relay with two `ShareLink`s and drops one live; **remote-accelerator
tests** cover `Settings` round-tripping `remote_accelerators` and a registered
remote reporting unreachable + persisting. `crates/net/tests/accelerator.rs` adds
one-relay-two-shares, mixed public/private on one relay, `remove_share`, and
persistent-identity tests. `crates/control-plane/tests/admin.rs` round-trips a
signed request through the admin router (authorised ok; bad key / stale ts → 401;
add-share reaches the supervisor channel; `POST /admin/shares/{id}`
`{"seeding":false|true}` round-trips a pause then a resume through
`AdminClient::set_share_seeding` and flips `ShareStatus.seeding`;
`DELETE /admin/shares/{id}` reaches it
with `keep_data=false`, `?keep_data=1` with `true`). `crates/control-plane/tests/rendezvous.rs`
round-trips a subscriber/origin pair through a live rendezvous server, including two
subscribers waiting on the same origin at once. `a_share_reachable_only_through_nat_rendezvous_still_completes`
in `app-state/tests/transfer_manager.rs` subscribes with only a deliberately-bogus,
unreachable address for the origin (same peer id, garbage transport) and still
completes — proof the transfer's reachability came from the rendezvous exchange, not
the address in the link. `crates/control-plane/tests/tracker.rs` round-trips
announce / list / withdraw through a live seeder-tracker server (incl.
`the_open_directory_lists_public_shares_by_name` — a private announce is served
by the keyed query but kept out of `GET /tracker`), and
`admin_tls.rs` checks the tracker rides the same (unauthenticated) listener as
rendezvous, not the admin one. `a_download_swarms_across_tracker_discovered_replicas`
in `app-state/tests/transfer_manager.rs` runs an origin + a fully-replicated NAS
both announcing to one tracker, then a third `App` subscribes with only the
origin's address in its request and still completes with chunks credited to
**two** sources — the replica came from the tracker, not the link.
`a_public_share_is_joinable_from_the_tracker_directory_with_no_link` in the same
file has a second `App` `refresh_directory()` → find the share by name in
`discovered_shares` → `subscribe_discovered` → complete byte-exact, never
touching a share link; `a_private_share_stays_out_of_the_tracker_directory` is
its negative. `crates/net/tests/discovery.rs`'s dcutr test now pins every
node to `127.0.0.1` (`Node::spawn_with`/`spawn_serving_with` + a loopback-only
`RelayNode::spawn_with_opts`) — mDNS deliberately skips loopback, so without this a
same-host relay/dcutr test races against (and loses to) mDNS finding the peer
directly, which is correct behavior in production but starves the relay path this
test exists to cover. `crates/core/tests/snapshot.rs` adds a
`sync_share` delta-apply test and an `index_dir` test (locations cover every chunk; a
`SourceChunkStore` over them rebuilds the tree byte-for-byte). `store.rs` unit-tests
`SourceChunkStore` read-through + caching, its no-op `put`, and its refusal to serve a
source file that changed after the scan; and `DiskChunkStore` zstd compression —
a compressible chunk is stored `.zst` (footprint shrinks) while an incompressible
one falls back to raw, and a raw store reopened with compression on keeps reading
old chunks while writing new ones compressed.

## Commands

```bash
cargo build --workspace                     # build everything
cargo build -p <crate>                      # build one crate
cargo test -p gaggle-core                   # test one crate (unit + tests/snapshot.rs)
cargo test -p gaggle-core merkle::tests::every_proof_verifies   # single test
cargo test -p app-state --test transfer_manager -- <names> --test-threads=2   # targeted
cargo clippy --workspace --all-targets      # lint (CI-level check)

cargo run -p accelerator -- run --role relay   # run the daemon (relay role)
cargo run -p accelerator -- run --role nas     # ... or the cache/NAS replica role
cargo run -p accelerator -- identity           # print its persistent public key
cargo run -p accelerator -- authorize <hex>    # let an operator drive the admin API
cargo run -p gui                            # run the desktop app (binary: gaggle-gui)
cargo run -p launcher                       # run the installer/updater (binary: gaggle-launcher)
cargo run -p launcher -- check              # headless: is an update available? (exit 10 = yes)
cargo run -p launcher -- --channel beta     # track pre-release builds (remembered in launcher.json)

cargo run -p accelerator-launcher -- run -- run --role relay   # headless: update, then exec accelerator
cargo run -p accelerator-launcher -- check --channel beta      # same exit-code convention as `launcher check`
cargo run -p accelerator-launcher -- service --role nas        # print a systemd user-unit template
```

`RUST_LOG=info` (or `debug`, `trace`) controls the `accelerator` daemon's logging via
`tracing-subscriber`'s `EnvFilter`; it defaults to `info` when unset.

**Run only the tests your change touches — never `cargo test --workspace` or the whole
`app-state` suite.** `app-state/tests/transfer_manager.rs` spins up many real libp2p/QUIC
nodes per test; run in parallel the whole file trips its own `wait_for` timeouts on a
busy machine (several "failures" that pass fine in isolation). Pick the handful of tests
that exercise your change and pass their names plus `--test-threads=2`.

First build of `-p gui` / `-p launcher` is slow: they pull the full `gpui` graphics
stack, and on Linux need the usual system libs for a windowed GPU app
(Vulkan/`libxkbcommon`/Wayland/X11, fontconfig). The other crates build in seconds.

## Versioning & releases

`[workspace.package] version = "2.0.0"` in the root `Cargo.toml`; every member sets
`version.workspace = true` / `edition.workspace = true`. The **runtime** version string
is `2.0.<short-commit-hash>` (`…-beta` on the beta channel), emitted as `GAGGLE_VERSION`
by a `build.rs` in `crates/gui` and `crates/launcher` (falls back to `2.0.unknown` with
no git history).

**Two release channels**, both driven by `.github/workflows/release.yml` (push to
`main` *or* `beta`). Each push builds + zips `gaggle-gui` + `gaggle-launcher` for
linux-x86_64 / windows-x86_64 / macos-aarch64 / macos-x86_64 (the Intel macOS
build is cross-compiled on the Apple Silicon runner — no x86_64 macOS runners
exist), runs
`.github/scripts/make_latest.py <version> <tag> <channel> dist` to compose `latest.json`,
and publishes a GitHub Release. Alongside each `gaggle-<platform>.zip` (+ `.sha256`) the
release also carries three standalone binaries as their own assets (each
`<name>-<platform>[.exe]` + `.sha256`, so getting any one of them is a single direct
download, no unzip needed): the **launcher** (`gaggle-launcher-…`), the **accelerator
daemon** (`gaggle-accelerator-…`), and its **auto-updating headless launcher**
(`gaggle-accelerator-launcher-…`, see "Headless accelerator auto-update" below).
`make_latest.py` composes *one* `latest.json` two independent consumers read from: the
desktop launcher's own `platforms` map (the GUI+launcher zip, unaffected by any of
this) and a sibling `accelerator` map (the standalone daemon binary, keyed the same
way) that `accelerator-launcher` reads and the desktop launcher's `Manifest` type
simply ignores.

| Branch | Version | Tag | Release | Descriptor URL |
|---|---|---|---|---|
| `main` | `2.0.<hash>` | `v2.0.<hash>` | normal, `make_latest` | `.../releases/latest/download/latest.json` |
| `beta` | `2.0.<hash>-beta` | rolling `beta` (deleted + recreated each push) | **pre-release** | `.../releases/download/beta/latest.json` |

The launcher tracks a channel (`Channel::{Stable,Beta}` in `crates/launcher/src/channel.rs`),
persisted in `<data-dir>/Gaggle/launcher.json`, chosen via the in-window `CH: STABLE|BETA`
toggle or `gaggle-launcher --channel beta` (remembered) / `$GAGGLE_UPDATE_CHANNEL`.
`--manifest-url` / `$GAGGLE_UPDATE_URL` still override the URL entirely. `installed.json`
records the installed version **and** channel, so flipping channels always shows an
update. Branch setup: `git branch beta main && git push -u origin beta` once `main` exists.

**Native install (`crates/launcher/src/desktop.rs`)** — on every install/update the
launcher creates OS-native shortcuts that point at itself (`paths::installed_launcher()`),
so opening the app from a shortcut always re-checks for updates first: a Linux
`~/.local/share/applications/gaggle.desktop` apps-menu entry, a Windows Start Menu
`.lnk` (built via PowerShell's `WScript.Shell` COM object, no extra crate), and a real
`~/Applications/Gaggle.app` bundle on macOS (a small shim executable + `Info.plist` +
`.icns`, so the bundle survives self-updates without re-bundling). The apps-menu / Start
Menu entry is always created; a desktop shortcut is opt-in (`Updater::set_desktop_shortcut`,
a checkbox in the launcher window, or `gaggle-launcher --desktop-shortcut`). Icons are the
Gaggle goose mark under `crates/launcher/assets/` (`icon.{svg,png,ico,icns}`, traced from
`logo.jpg`; `include_bytes!`'d so the *standalone* launcher binary can create shortcuts
with no zip alongside it). `gaggle-launcher run` (the default, e.g. from a shortcut) silently hands off
straight to the installed GUI — no window — when it's already the latest version, or when
the update check fails but something is installed (`updater::wants_auto_launch`, pure and
unit-tested); otherwise it opens the window as before.

**Headless accelerator auto-update (`crates/accelerator-launcher`, binary
`gaggle-accelerator-launcher`)** — the headless, automatic counterpart of `launcher`, for
running the `accelerator` daemon as an always-current systemd service with no GUI and no
manual redeploys. `gaggle-accelerator-launcher run -- <accelerator args…>` best-effort
-updates the standalone `accelerator` binary (falling back to whatever's already installed
if the network check fails — a transient outage must never stop the daemon from starting),
then **execs** it (`std::os::unix::process::CommandExt::exec`, so systemd tracks the
daemon's own PID/exit code directly and `Restart=` policies apply to the real thing, not a
wrapper — a plain spawn-and-wait on non-Unix targets). `check` / `update` mirror
`gaggle-launcher`'s subcommands and exit-code convention (0 up to date, 10 update
available, 1 error); `--channel` / `$GAGGLE_UPDATE_CHANNEL` / `--manifest-url` /
`$GAGGLE_UPDATE_URL` work exactly like `gaggle-launcher`'s. It keeps entirely separate
state under `<data-dir>/Gaggle/accelerator-launcher/` (its own `installed.json` +
`launcher.json`) so it can never collide with a desktop-launcher install on the same
machine. `service [--role …] [--listen …] [--admin-listen …] [--install]` prints (or
writes to `~/.config/systemd/user/gaggle-accelerator.service`) a ready-to-use systemd
**user** unit whose `ExecStart=` is `<this binary> run -- run --role … …` — the doubled
`run` is intentional: the first is this launcher's own subcommand, the second is the
`accelerator` binary's (everything after the outer `--` is passed through verbatim, so
this launcher never needs to know the daemon's own CLI grammar).

## Workspace layout & dependency graph

Cargo virtual workspace (`resolver = "2"`, `edition = "2024"`), nine members under
`crates/`:

| Crate (package name) | Kind | Role |
|---|---|---|
| `crates/core` → **`gaggle-core`** | lib | Manifest format, chunking, merkle trees, dedup. Pure logic, no async; light on deps (rayon is the only heavy one — data-parallel folder scan). |
| `crates/net` → `net` | lib | libp2p swarm: QUIC transport, Kademlia DHT, relay + dcutr NAT traversal. |
| `crates/control-plane` → `control-plane` | lib | `axum` server + `reqwest` client: invite exchange, NAT rendezvous (`rendezvous::{router, RendezvousRegistry, RendezvousClient}`), the seeder tracker (`tracker::{router, TrackerRegistry, TrackerClient}` — who else is serving a share, plus `GET /tracker` = the open directory of public shares), and the signed accelerator **admin API** (`admin::{router, AdminClient, AdminState, DaemonStatus}`). `serve_daemon` merges admin + rendezvous + tracker routers onto one listener (or splits admin from the two unauthenticated ones). |
| `crates/app-state` → `app-state` | lib | UI-framework-agnostic application state + transfer manager. Testable headless. |
| `crates/ui-kit` → `gaggle-ui-kit` | lib | Shared `gpui` look: the colour `theme` (`Palette`, `DARK`/`LIGHT`, `active()`) + stateless `widgets`. Depends only on `gpui` + `gpui-component`. Used by `gui` and `launcher`. |
| `crates/accelerator` → `accelerator` | **bin** | Headless daemon; `--role relay\|nas` selects bandwidth-heavy vs storage-heavy behaviour. |
| `crates/gui` → `gui` (binary `gaggle-gui`) | **bin** | `gpui` + `gpui-component` desktop frontend. |
| `crates/launcher` → `launcher` (binary `gaggle-launcher`) | **bin** | Installer / updater / launcher: fetches `latest.json`, installs `gaggle-gui` under the per-user data dir, launches it. `gpui` window styled from `gaggle-ui-kit`; `updater.rs` is the headless engine. |
| `crates/accelerator-launcher` → `accelerator-launcher` (binary `gaggle-accelerator-launcher`) | **bin** | Headless, no-GUI counterpart of `launcher`: update-then-`exec`s the `accelerator` binary, for an always-current systemd service. No shared code with `launcher` (deliberately self-contained, like `launcher` itself) beyond reading the same `latest.json`. |

Dependency direction: `gaggle-core` is the leaf. `net` → core. `control-plane` → core.
`app-state` → core + net + control-plane (the `AdminClient` for remote accelerators).
`accelerator` → core + net + control-plane. `gaggle-ui-kit` → (gpui only).
`gui` → app-state + gaggle-ui-kit. `launcher` → gaggle-ui-kit. `accelerator-launcher` has
no workspace-internal dependencies at all — it never touches `net`/`accelerator`'s own
types, only the release descriptor JSON and a plain child-process exec. `ShareLink` lives
in `net` so the daemon config and the admin API can both round-trip its tokens.

**The `crates/core` directory holds a package named `gaggle-core`, imported as
`gaggle_core`.** Naming it `core` collides with Rust's built-in `core` crate — macro
expansions like `#[tokio::main]` emit `core::...` paths that then fail to resolve. Keep
the package name distinct from any std crate.

Shared third-party deps are pinned once in `[workspace.dependencies]` in the root
`Cargo.toml` and referenced from members as `foo.workspace = true`. Add a shared dep
there, not per-crate, to keep versions in lockstep. Crate-specific deps (`blake3`,
`libp2p`, `axum`/`reqwest`, `clap`, `gpui`) stay in the individual manifests.

`Cargo.lock` is committed on purpose — this workspace produces distributable binaries,
not a library.

## Architecture: the two-plane split

The core structural decision is that the network is split into two independent planes,
and this maps directly onto the crate boundaries:

- **Data plane** (`net`): peer↔peer and peer↔accelerator chunk transfer over
  `rust-libp2p` with its **QUIC** transport. Independent stream multiplexing (no
  head-of-line blocking across concurrent chunk pulls), TLS 1.3, 0-RTT reconnects,
  connection migration. Discovery via libp2p's Kademlia DHT; NAT hole-punching via
  `libp2p-relay` + `dcutr`. Relay accelerators *are* libp2p relay servers.
- **Control plane** (`control-plane`): plain HTTPS/REST (`axum` server, `reqwest`
  client) for low-volume, request/response traffic — bootstrap, invite tokens,
  accelerator registration, admin, NAT-rendezvous signaling, and the seeder
  tracker (peer discovery hints). Deliberately kept off QUIC/libp2p.

Everything async runs on **Tokio**, shared across `net` and `control-plane`. The GUI
runs `gpui`'s own executor; network code runs on separate Tokio tasks and the two are
bridged with `tokio::sync::mpsc` channels — network events are forwarded into `gpui`
via `Entity::update` / `cx.spawn` so the UI thread only ever touches `gpui`-owned
state, and user actions travel back out the same way. Keep `net`/`control-plane` free
of any `gpui` types.

## Data model (`gaggle-core`)

All BLAKE3. The one filesystem entry point is `snapshot::snapshot_dir`, and it only
reads. Module layout and how the pieces chain:

- **`hash`** — `Hash`, a 32-byte digest newtype. Serdes as hex in human-readable
  formats, raw bytes otherwise. Plain (non-constant-time) equality — content
  addresses aren't secrets. Downstream crates use `gaggle_core::Hash`, not `blake3`.
  `Hash::of` tree-hashes inputs ≥ 128 KiB (`RAYON_HASH_THRESHOLD`) across the
  global rayon pool (`blake3` `rayon` feature); smaller inputs (Merkle nodes,
  signing bytes) take the plain path.
- **`chunk`** — FastCDC v2020 content-defined chunking (`chunk_slice` for buffers,
  `chunk_reader` for streaming so a 100 GB folder never needs 100 GB of RAM). A
  chunk's content address is `blake3(bytes)`. `ChunkerConfig::for_file_size` scales
  the target chunk size by file size (see `notes/plan.md` open questions) — this is
  the "size-adaptive" heuristic, a first cut.
- **`merkle`** — binary Merkle tree over a file's chunk hashes. Leaves/nodes are
  domain-separated by a 1-byte prefix; a lone odd node is promoted unchanged (not
  duplicated); empty file → a fixed sentinel root. `MerkleProof::verify` rebuilds the
  expected tree shape from `num_leaves` alone, so a forged proof can't lie about size.
- **`chunklist`** — `ChunkList`, the ordered chunk sequence for one file. `verify`s
  against a trusted `(root, size)` from the manifest before any chunk data is fetched.
- **`manifest`** — `Manifest` = `format` + `version` + `name` + sorted `files`
  (`FileEntry { path, size, root, mode }`) + sorted `dirs`. Deliberately small: **one
  root per file, no embedded chunk lists**. `canonicalize` (sort + dedup) before
  serializing; `validate` rejects unsorted entries and unsafe paths — `..`, absolute,
  `\`, NUL, **and Windows traps** (`:` drive/ADS, trailing dot/space, `CON`/`NUL`/`COM1`…
  device names) so `write_share` can't escape its target dir on any OS. `Manifest::diff`
  classifies files added/removed/changed/unchanged by comparing roots.
- **`store`** — `ChunkStore` trait + four impls: `MemoryChunkStore` (plain map,
  `DedupStats`), `LruChunkCache` (byte-budgeted, LRU eviction, `CacheStats` — relay
  hot cache), `DiskChunkStore` (durable, sharded one-file-per-chunk, `try_get` /
  `try_put` for explicit `io::Result` — NAS replica; `open_with_opts(_, true)`
  stores each chunk zstd-compressed as `<hex>.zst` when it shrinks, self-describing
  per chunk so raw + compressed mix freely), and `SourceChunkStore` (a
  `root` + a `Hash -> ChunkLocation` index + a bounded `LruChunkCache`; `get` reads
  the range from the source file, re-hashes it — a since-changed file yields `None` —
  and caches it; `put` is a no-op — the streaming seed). Dedup is just content
  addressing: `put` is a no-op if the hash is already present.
- **`snapshot`** — ties it together: `snapshot_dir` walks a dir → chunks each file into
  a `ChunkStore` → `Snapshot { manifest, chunk_lists, skipped }`. `index_dir` is the
  same walk but records a `Hash -> ChunkLocation` map instead of storing bytes →
  `IndexedSnapshot` (for a `SourceChunkStore`). `write_share` is the inverse: rebuild
  the files under a root dir from a store (used by the NAS accelerator and the loopback
  demo). `scan_tree` (shared by `snapshot_dir` / `index_dir`) chunks + hashes files
  on the rayon pool and streams results to the caller's thread over a bounded
  channel — see the parallelism note near the top of this file.
- **`identity`** — per-share Ed25519 keypair. `ShareKeypair` (origin-only secret) /
  `SharePublicKey` (the share's authority, distinct from `Manifest::id`) / `Signature`,
  all with hex/base64url serde. `verify` uses `verify_strict` (no malleable sigs).
- **`invite`** — `Scope` (`All` | canonicalized `Files`), `Capability`
  (`share` + `manifest_id` + `scope` + optional `expires_at` + random `nonce`), signed
  by `ShareKeypair::issue` into a `SignedCapability` (`verify` / `verify_for` check sig,
  expiry, share, manifest). `Invite` wraps it with the manifest id + name and encodes
  to a `gaggle1<base64url>` token. Signing bytes are domain-tagged canonical JSON.

Trust flow: the manifest id is authenticated by the `Invite`'s signed `Capability`
(milestone 7); everything else — chunk lists, chunk bytes — is verified against the
manifest root, so it can come from any untrusted peer or accelerator. On a private
share the serving node also requires that signed capability per connection and enforces
its per-file `Scope`.

## The GUI / core split

`app-state` is where the testable logic lives: `App` + the transfer manager, headless,
driven by real `net` calls, covered by `app-state/tests/transfer_manager.rs`. `gui` is a
thin gpui renderer that only ever calls `App`'s sync methods and reads `App::snapshot()`
— it holds no `net` types and has no automated tests (a windowed GPU app can't run in
CI). Put behaviour in `app-state`, pixels in `gui`.

**Do not screenshot or launch the app to check visual changes.** After a GUI change,
confirm it builds (`cargo build -p gui`) and passes `cargo clippy -p gui --all-targets`,
then ask the user to run it and confirm the result looks right. Don't drive `spectacle`
/ `grim` / `cargo run -p gui` for verification.

`gui` module layout: `main.rs` (window + module wiring only) · `app.rs` (the one gpui
view — state snapshot, tab, per-row expansion set, the Settings/Accelerator/invite
`InputState` entities, actions) · `ui/` (`widgets.rs` themed primitives incl. `field()`
over `gpui_component::input::Input`, `chrome.rs` title bar + status bar, `views.rs` the
five tabs + expandable detail panels) · `theme.rs` (swappable
`Palette`s — `DARK`, `LIGHT`; every widget paints from `theme::active()`, a
thread-local) · `clipboard.rs` · `util.rs`. Add a theme = one more `static Palette` +
a branch in `theme::activate`. The window is decorationless (`WindowDecorations::Client`
+ `window_border()`); the header draws its own min/max/close and drag-to-move, except on
macOS, where those are the real (still-native) traffic lights repositioned under the
header by `WindowOptions::titlebar` — a bare `titlebar: None` there silently drops
`NSResizableWindowMask`/`NSClosableWindowMask`/`NSMiniaturizableWindowMask`, so the window
can't be resized or closed/minimized/maximized at all on macOS; it must stay
`Some(TitlebarOptions { title: None, appears_transparent: true, traffic_light_position })`.
The launcher's splash window (`launcher/src/main.rs`, `ui.rs`) follows the same pattern.

## GUI dependency note

`gpui` and `gpui-component` are pulled from **crates.io** (`gpui = "0.2"`,
`gpui-component = "0.5"`), not from the `zed-industries/zed` git repo. `gpui` now
publishes releases and `gpui-component` tracks a matching `gpui` version, so no git-rev
pinning is needed. `gpui::hsla` is **not** `const` — build palette constants with a
`const fn` wrapping the `Hsla { h, s, l, a }` literal. A future-incompat warning from
`proc-macro-error2` (transitive via `gpui-component`'s macros) is expected and not
actionable here.
