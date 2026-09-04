# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Gaggle is a hybrid-P2P application for sharing very large folders (100GB+, e.g. modded
game installs) over private, invite-based swarms. Peers exchange content-addressed
chunks directly; optional always-on **accelerator nodes** improve availability and
throughput. The full design and the milestone roadmap live in `notes/plan.md` (that
directory is git-ignored — read it, don't rely on it being present for others).

Milestone 1 (the `gaggle-core` data model — chunking, Merkle trees, manifest, dedup)
is implemented and tested. `gaggle-core` ships four `ChunkStore`s: `MemoryChunkStore`
(a plain map), `LruChunkCache` (byte-budgeted, LRU eviction — the relay's hot cache),
`DiskChunkStore` (durable, one sharded file per chunk — the NAS replica), and
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
  per-connection and dropped on disconnect. A scoped download passes
  `SwarmConfig::allowed_paths` so `fetch_share_from_swarm` narrows the manifest to the
  granted files up front and never asks a source for one it would refuse; `app-state`
  derives it from the subscription's credential.
- **`control-plane`** gets its first real code: `invite::{InviteRegistry, router,
  InviteClient}` — an in-memory HTTP service to `POST /invites` (rejecting
  bad-signature invites) and `GET /invites/{code}`.
- **`accelerator` binary** is a config-file + admin-API daemon (see "Remote,
  multi-share accelerators" below): `accelerator run [--role relay|nas]
  [--cache-mib N] [--dir <path>] [--admin-listen host:port] [--listen <maddr>]`,
  plus `accelerator identity` / `authorize <hex>` / `share {add|rm|ls}`. State
  lives under `--home` / `$GAGGLE_ACCEL_HOME` / the per-OS data dir (`dirs::data_dir()`
  — `~/.local/share` Linux, `~/Library/Application Support` macOS, `%APPDATA%` Windows)
  `+ /gaggle/accelerator` (`identity.key` + `config.toml`).

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
  `App::benchmark()` measures sequential write throughput to the download volume plus
  free space (`statvfs` on unix) and suggests a role.
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
  `GET|POST /admin/shares`, `DELETE /admin/shares/{id}`. Every request is signed
  by the operator's `AgentKeypair` (canonical `METHOD\nPATH\nTS\nNONCE\nblake3(body)`,
  headers `x-gaggle-agent|timestamp|nonce|signature`, ±60 s skew, checked against
  `AdminState.authorized`); every response is signed by the daemon key
  (`x-gaggle-daemon[-signature]`) so `AdminClient` TOFU-pins it. Mutations go out
  an `mpsc<AdminCommand>`; status comes in a `watch<DaemonStatus>` — no `net` dep.
- **`accelerator` daemon** — `config.rs` (`AcceleratorConfig` toml: role, listen,
  admin_listen, cache_mib, replica_dir, `authorized_keys`, `shares`),
  `supervisor.rs` (`Supervisor` owns a multi-share `RelayNode` **or** a
  `HashMap<Hash, Node>` of per-share replicas; applies `AdminCommand`s and
  rewrites `config.toml`), `run.rs` (identity + config + supervisor + admin
  server). Prints its peer id + public key on every start.
- **`app-state`** — `App` keeps a persistent operator `AgentKeypair` at
  `operator.key` (`App::operator_public_key()`). `AcceleratorRequest::{Relay,Nas}`
  take `shares: Vec<ShareLink>`; `AcceleratorState.shares: Vec<AccelShareRow>`;
  `App::accel_add_share` / `accel_remove_share` mutate a *running* local
  accelerator. `Settings.remote_accelerators: Vec<RemoteAccelerator>` (label +
  admin URL + pinned `daemon_key`); `App::{add,remove}_remote_accelerator` /
  `remote_{add,remove}_share`; the manager polls each every ~10 s via `AdminClient`
  into `AppState.remote_accelerators: Vec<RemoteAccelState>`.
- **`ShareLink`** moved from `app-state` to **`net`** (`net::ShareLink`, re-exported
  by `app-state`); `into_request()` is now `From<ShareLink> for SubscribeRequest`
  in `app-state`.
- **`gui`** Accelerator tab: an operator-key card (copy → `accelerator authorize
  <key>`), the local form now taking one link per line + a running-status card
  listing every carried share with Remove and an "Add share" field, and a
  "Remote accelerators" section (reachable dot, role, per-share rows, "Add
  remote" form + per-remote "Add share").

- **`gui`** — a gpui shell over `App`: Shares (add public / private folder, copy link,
  rescan, per-row ▸ panel with the invite form), Transfers (progress bars,
  pause/resume/remove, check-updates/resync, `update vN` badge, per-row ▸ swarm
  inspector = per-source chunk/byte breakdown), Accelerator (benchmark → suggested role
  → start relay / NAS → live status), and an editable Settings form. It polls
  `App::snapshot()` on a 200 ms `Timer` and re-renders; it never touches `net`. Raw gpui
  for layout/interaction, plus `gpui_component::{init, v_flex, window_border}` and
  `gpui_component::input::{Input, InputState}` for the form fields.

Tests: `app-state/tests/transfer_manager.rs` runs real loopback transfers through two
`App`s — share→subscribe→complete with byte-exact output, incremental progress events,
pause keeps partial + resume finishes, settings survive a restart, removing a seed makes
later subscribers fail, **rescan→check_updates→resync applies only the delta** (added
file arrives, removed file is deleted, `version` bumps), **a private share refuses a
strangers then admits a minted invite**, **benchmark reports throughput + free space**,
**a NAS accelerator replicates a share**, **a seed streams a folder several times its
`seed_cache_bytes` budget from disk and still serves every chunk**. `app-state` unit
tests cover `Settings`
persistence, `ShareLink` round trips, and name sanitizing. **`relay_accelerator_carries_multiple_shares`**
starts a local relay with two `ShareLink`s and drops one live; **remote-accelerator
tests** cover `Settings` round-tripping `remote_accelerators` and a registered
remote reporting unreachable + persisting. `crates/net/tests/accelerator.rs` adds
one-relay-two-shares, mixed public/private on one relay, `remove_share`, and
persistent-identity tests. `crates/control-plane/tests/admin.rs` round-trips a
signed request through the admin router (authorised ok; bad key / stale ts → 401;
add-share reaches the supervisor channel). `crates/core/tests/snapshot.rs` adds a
`sync_share` delta-apply test and an `index_dir` test (locations cover every chunk; a
`SourceChunkStore` over them rebuilds the tree byte-for-byte). `store.rs` unit-tests
`SourceChunkStore` read-through + caching, its no-op `put`, and its refusal to serve a
source file that changed after the scan.

## Commands

```bash
cargo build --workspace                     # build everything
cargo build -p <crate>                      # build one crate
cargo test --workspace                      # run all tests
cargo test -p gaggle-core                   # test one crate (unit + tests/snapshot.rs)
cargo test -p gaggle-core merkle::tests::every_proof_verifies   # single test
cargo clippy --workspace --all-targets      # lint (CI-level check)

cargo run -p accelerator -- run --role relay   # run the daemon (relay role)
cargo run -p accelerator -- run --role nas     # ... or the cache/NAS replica role
cargo run -p accelerator -- identity           # print its persistent public key
cargo run -p accelerator -- authorize <hex>    # let an operator drive the admin API
cargo run -p gui                            # run the desktop app (binary: gaggle-gui)
cargo run -p launcher                       # run the installer/updater (binary: gaggle-launcher)
cargo run -p launcher -- check              # headless: is an update available? (exit 10 = yes)
cargo run -p launcher -- --channel beta     # track pre-release builds (remembered in launcher.json)
```

`RUST_LOG=info` (or `debug`, `trace`) controls the `accelerator` daemon's logging via
`tracing-subscriber`'s `EnvFilter`; it defaults to `info` when unset.

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
and publishes a GitHub Release:

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

## Workspace layout & dependency graph

Cargo virtual workspace (`resolver = "2"`, `edition = "2024"`), eight members under
`crates/`:

| Crate (package name) | Kind | Role |
|---|---|---|
| `crates/core` → **`gaggle-core`** | lib | Manifest format, chunking, merkle trees, dedup. Pure logic, no async, dependency-light. |
| `crates/net` → `net` | lib | libp2p swarm: QUIC transport, Kademlia DHT, relay + dcutr NAT traversal. |
| `crates/control-plane` → `control-plane` | lib | `axum` server + `reqwest` client: invite exchange, and the signed accelerator **admin API** (`admin::{router, AdminClient, AdminState, DaemonStatus}`). |
| `crates/app-state` → `app-state` | lib | UI-framework-agnostic application state + transfer manager. Testable headless. |
| `crates/ui-kit` → `gaggle-ui-kit` | lib | Shared `gpui` look: the colour `theme` (`Palette`, `DARK`/`LIGHT`, `active()`) + stateless `widgets`. Depends only on `gpui` + `gpui-component`. Used by `gui` and `launcher`. |
| `crates/accelerator` → `accelerator` | **bin** | Headless daemon; `--role relay\|nas` selects bandwidth-heavy vs storage-heavy behaviour. |
| `crates/gui` → `gui` (binary `gaggle-gui`) | **bin** | `gpui` + `gpui-component` desktop frontend. |
| `crates/launcher` → `launcher` (binary `gaggle-launcher`) | **bin** | Installer / updater / launcher: fetches `latest.json`, installs `gaggle-gui` under the per-user data dir, launches it. `gpui` window styled from `gaggle-ui-kit`; `updater.rs` is the headless engine. |

Dependency direction: `gaggle-core` is the leaf. `net` → core. `control-plane` → core.
`app-state` → core + net + control-plane (the `AdminClient` for remote accelerators).
`accelerator` → core + net + control-plane. `gaggle-ui-kit` → (gpui only).
`gui` → app-state + gaggle-ui-kit. `launcher` → gaggle-ui-kit. `ShareLink` lives in
`net` so the daemon config and the admin API can both round-trip its tokens.

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
  serializing; `validate` rejects unsorted entries and unsafe paths — `..`, absolute,
  `\`, NUL, **and Windows traps** (`:` drive/ADS, trailing dot/space, `CON`/`NUL`/`COM1`…
  device names) so `write_share` can't escape its target dir on any OS. `Manifest::diff`
  classifies files added/removed/changed/unchanged by comparing roots.
- **`store`** — `ChunkStore` trait + four impls: `MemoryChunkStore` (plain map,
  `DedupStats`), `LruChunkCache` (byte-budgeted, LRU eviction, `CacheStats` — relay
  hot cache), `DiskChunkStore` (durable, sharded one-file-per-chunk, `try_get` /
  `try_put` for explicit `io::Result` — NAS replica), and `SourceChunkStore` (a
  `root` + a `Hash -> ChunkLocation` index + a bounded `LruChunkCache`; `get` reads
  the range from the source file, re-hashes it — a since-changed file yields `None` —
  and caches it; `put` is a no-op — the streaming seed). Dedup is just content
  addressing: `put` is a no-op if the hash is already present.
- **`snapshot`** — ties it together: `snapshot_dir` walks a dir → chunks each file into
  a `ChunkStore` → `Snapshot { manifest, chunk_lists, skipped }`. `index_dir` is the
  same walk but records a `Hash -> ChunkLocation` map instead of storing bytes →
  `IndexedSnapshot` (for a `SourceChunkStore`). `write_share` is the inverse: rebuild
  the files under a root dir from a store (used by the NAS accelerator and the loopback
  demo).
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
four tabs + expandable detail panels) · `theme.rs` (swappable
`Palette`s — `DARK`, `LIGHT`; every widget paints from `theme::active()`, a
thread-local) · `clipboard.rs` · `util.rs`. Add a theme = one more `static Palette` +
a branch in `theme::activate`. The window is decorationless (`WindowDecorations::Client`
+ `window_border()`); the header draws its own min/max/close and drag-to-move.

## GUI dependency note

`gpui` and `gpui-component` are pulled from **crates.io** (`gpui = "0.2"`,
`gpui-component = "0.5"`), not from the `zed-industries/zed` git repo. `gpui` now
publishes releases and `gpui-component` tracks a matching `gpui` version, so no git-rev
pinning is needed. `gpui::hsla` is **not** `const` — build palette constants with a
`const fn` wrapping the `Hsla { h, s, l, a }` literal. A future-incompat warning from
`proc-macro-error2` (transitive via `gpui-component`'s macros) is expected and not
actionable here.
