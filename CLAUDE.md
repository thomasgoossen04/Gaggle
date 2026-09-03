# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Gaggle is a hybrid-P2P application for sharing very large folders (100GB+, e.g. modded
game installs) over private, invite-based swarms. Peers exchange content-addressed
chunks directly; optional always-on **accelerator nodes** improve availability and
throughput. The full design and the milestone roadmap live in `notes/plan.md` (that
directory is git-ignored — read it, don't rely on it being present for others).

Milestone 1 (the `gaggle-core` data model — chunking, Merkle trees, manifest, dedup)
is implemented and tested. `gaggle-core` ships three `ChunkStore`s: `MemoryChunkStore`
(a plain map), `LruChunkCache` (byte-budgeted, LRU eviction — the relay's hot cache),
and `DiskChunkStore` (durable, one sharded file per chunk — the NAS replica).
`snapshot::write_share` is `snapshot_dir`'s inverse: materialize a share's files from
any store back onto disk.

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
  reports what it has via `Request::GetInventory`.
- **`Node`** — a standard peer: the chunk protocol wired together with a **Kademlia**
  DHT (`ShareKey` = `Manifest::id`; `provide` / `find_providers`), **identify**, a
  **relay client** and **dcutr**. It resolves a route to a discovered peer (routing
  table → learned addresses → `get_closest_peers`) before requesting, and reaches
  NAT'd peers through a relay circuit, upgrading to a direct connection when dcutr's
  hole-punch lands.
- **Multi-peer swarming** (`swarm::fetch_share_from_swarm`, `Node::download_share_multi`)
  — pulls one share from several sources at once. Queries each source's inventory,
  builds a per-chunk availability map, and schedules chunk requests **rarest-first**
  (fewest holders first) with a per-peer concurrency cap so load spreads across
  sources. Every chunk is still verified against the manifest; a failed or
  chunk-less source is re-routed around (transport failures drop the source
  entirely, a `NotFound` only for that chunk). `SwarmConfig::prefer` /
  `Node::download_share_multi_preferring` biases the scheduler toward given sources
  first — the NAS's "LAN-priority" knob. `SwarmDownload` reports per-source chunk
  counts and fetch order. `pick_next` / `requeue` are pure and unit-tested.
- **`RelayNode`** — the accelerator's relay role (milestones 3 & 5): a libp2p relay
  server + a Kademlia bootstrap server, **plus a read-through hot-chunk cache**. After
  `cache_share(manifest, lists, upstreams)` it answers `GetChunk` from an
  `LruChunkCache`; on a miss it fetches from an upstream seed, verifies, caches
  (evicting the coldest chunk if over `RelayConfig::cache_capacity_bytes`), and
  forwards — so N downloaders cost the origin one fetch per hot chunk. `cache_stats()`
  exposes hits/misses/evictions/bytes.
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
  per-connection and dropped on disconnect.
- **`control-plane`** gets its first real code: `invite::{InviteRegistry, router,
  InviteClient}` — an in-memory HTTP service to `POST /invites` (rejecting
  bad-signature invites) and `GET /invites/{code}`.
- **`accelerator` binary** wires everything: `--role relay [--upstream <addr>]...
  [--cache-mib N] [--restrict <sharepub-hex>]` and `--role nas --dir <path>
  --source <addr>... [--materialize <path>] [--invite <gaggle1…>]`.

Tests: `crates/net/tests/loopback.rs` (direct transfer), `discovery.rs` (DHT +
relay/dcutr), `swarm.rs` (multi-seed load spread, partial-seed stitching, dead-source
re-routing, re-seeding), `accelerator.rs` (relay cache shields the origin / evicts under
budget; NAS durability across restart, resumed replication, LAN priority),
`private.rs` (no-invite refusal, whole-share download, per-file scope, expiry, wrong
key, invite-URL round trip, private relay). `control-plane/tests/invite_exchange.rs`
round-trips an invite through a live server. Plus the
`cargo run -p net --example loopback_transfer` demo
(`serve [seed]` / `mint-invite` / `fetch [invite]` / `fetch-swarm`).
Peer `Node`s deliberately do **not** `add_external_address` their own loopback addr —
that would make the swarm swallow the identify address candidates dcutr needs.

Milestone 8 (GUI v1) is implemented and tested:

- **`app-state`** — the headless, UI-framework-agnostic transfer manager. `App` is a
  sync, thread-safe handle (callable from a GUI thread with no tokio runtime); all the
  async lives on a background task `App::new` spawns. It owns the `net` nodes: one
  shared downloading `Node`, one serving `Node` per local share. `App::add_local_share`
  snapshots a folder (off-thread) and seeds it; `App::subscribe(SubscribeRequest)`
  pulls a remote share into a `DiskChunkStore` under the download dir (so pause = abort,
  resume = top up), then `write_share`s the tree out. Progress rides
  `Node::download_share_multi_with_progress` (new `SwarmProgress` per chunk). Callers
  read `App::snapshot()` (a cloneable `AppState { transfers, settings, swarm }`) or
  listen on `App::events()`. `Settings` persist to a JSON path. `ShareLink` is the
  copy-paste `gaggleshare1…` token (addr(s) + manifest id + optional invite) that turns
  into a `SubscribeRequest`.
- **`gui`** — a gpui shell over `App`: a Shares view (add folder, copy link, remove), a
  Transfers view (progress bars, pause/resume/remove, paste-a-link to subscribe), and a
  Settings view. It polls `App::snapshot()` on a 200 ms `Timer` and re-renders; it never
  touches `net`. Raw gpui for layout/interaction; `gpui_component::{init, v_flex}` only.

Tests: `app-state/tests/transfer_manager.rs` runs real loopback transfers through two
`App`s — share→subscribe→complete with byte-exact output, incremental progress events,
pause keeps partial + resume finishes, settings survive a restart, removing a seed makes
later subscribers fail. `app-state` unit tests cover `Settings` persistence, `ShareLink`
round trips, and name sanitizing.

Milestone 9 (GUI v2 — accelerator setup wizard, swarm inspector, invite dialog, theming
polish) and milestone 10 (delta sync) are next.

## Commands

```bash
cargo build --workspace                     # build everything
cargo build -p <crate>                      # build one crate
cargo test --workspace                      # run all tests
cargo test -p gaggle-core                   # test one crate (unit + tests/snapshot.rs)
cargo test -p gaggle-core merkle::tests::every_proof_verifies   # single test
cargo clippy --workspace --all-targets      # lint (CI-level check)

cargo run -p accelerator -- --role relay    # run the daemon as a relay node
cargo run -p accelerator -- --role nas      # ... or as a cache/NAS replica node
cargo run -p gui                            # run the desktop app
```

`RUST_LOG=info` (or `debug`, `trace`) controls the `accelerator` daemon's logging via
`tracing-subscriber`'s `EnvFilter`; it defaults to `info` when unset.

First build of `-p gui` is slow: it pulls the full `gpui` graphics stack, and on Linux
needs the usual system libs for a windowed GPU app (Vulkan/`libxkbcommon`/Wayland/X11,
fontconfig). The other five crates build in seconds.

## Workspace layout & dependency graph

Cargo virtual workspace (`resolver = "2"`, `edition = "2024"`), six members under
`crates/`:

| Crate (package name) | Kind | Role |
|---|---|---|
| `crates/core` → **`gaggle-core`** | lib | Manifest format, chunking, merkle trees, dedup. Pure logic, no async, dependency-light. |
| `crates/net` → `net` | lib | libp2p swarm: QUIC transport, Kademlia DHT, relay + dcutr NAT traversal. |
| `crates/control-plane` → `control-plane` | lib | `axum` server + `reqwest` client for bootstrap, invite exchange, accelerator registration, admin/status. |
| `crates/app-state` → `app-state` | lib | UI-framework-agnostic application state + transfer manager. Testable headless. |
| `crates/accelerator` → `accelerator` | **bin** | Headless daemon; `--role relay\|nas` selects bandwidth-heavy vs storage-heavy behaviour. |
| `crates/gui` → `gui` | **bin** | `gpui` + `gpui-component` desktop frontend. |

Dependency direction: `gaggle-core` is the leaf. `net` → core. `control-plane` → core.
`app-state` → core + net. `accelerator` → core + net + control-plane. `gui` → app-state.

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
  accelerator registration, admin. Deliberately kept off QUIC/libp2p.

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
  serializing; `validate` rejects unsafe paths and unsorted entries. `Manifest::diff`
  classifies files added/removed/changed/unchanged by comparing roots.
- **`store`** — `ChunkStore` trait + three impls: `MemoryChunkStore` (plain map,
  `DedupStats`), `LruChunkCache` (byte-budgeted, LRU eviction, `CacheStats` — relay
  hot cache), `DiskChunkStore` (durable, sharded one-file-per-chunk, `try_get` /
  `try_put` for explicit `io::Result` — NAS replica). Dedup is just content
  addressing: `put` is a no-op if the hash is already present.
- **`snapshot`** — ties it together: `snapshot_dir` walks a dir → chunks each file into
  a `ChunkStore` → `Snapshot { manifest, chunk_lists, skipped }`. `write_share` is the
  inverse: rebuild the files under a root dir from a store (used by the NAS accelerator
  and the loopback demo).
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

## GUI dependency note

`gpui` and `gpui-component` are pulled from **crates.io** (`gpui = "0.2"`,
`gpui-component = "0.5"`), not from the `zed-industries/zed` git repo. `gpui` now
publishes releases and `gpui-component` tracks a matching `gpui` version, so no git-rev
pinning is needed. `gpui::hsla` is **not** `const` — build palette constants with a
`const fn` wrapping the `Hsla { h, s, l, a }` literal. A future-incompat warning from
`proc-macro-error2` (transitive via `gpui-component`'s macros) is expected and not
actionable here.
