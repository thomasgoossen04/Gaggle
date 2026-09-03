# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Gaggle is a hybrid-P2P application for sharing very large folders (100GB+, e.g. modded
game installs) over private, invite-based swarms. Peers exchange content-addressed
chunks directly; optional always-on **accelerator nodes** improve availability and
throughput. The full design and the milestone roadmap live in `notes/plan.md` (that
directory is git-ignored — read it, don't rely on it being present for others).

Milestone 1 (the `gaggle-core` data model — chunking, Merkle trees, manifest, dedup)
is implemented and tested. Milestone 2 (loopback QUIC transfer) is implemented: `net`
runs a libp2p **request-response over QUIC** protocol where one process serves a share
(`Catalog` + `ServerHandle`) and another pulls and verifies it (`Client` +
`download_share`); see `crates/net/tests/loopback.rs` and the
`cargo run -p net --example loopback_transfer` two-process demo. `control-plane`,
`app-state`, and the GUI views are still stubs; milestone 3 (DHT + NAT traversal) is
next.

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
- **`store`** — `ChunkStore` trait + `MemoryChunkStore`. Dedup is just content
  addressing: `put` is a no-op if the hash is already present. `DedupStats` tracks
  unique vs. duplicate bytes.
- **`snapshot`** — ties it together: walk dir → chunk each file into a `ChunkStore` →
  `Snapshot { manifest, chunk_lists, skipped }`.

Trust flow: the manifest is authenticated out of band (invite token, milestone 7);
everything else — chunk lists, chunk bytes — is verified against the manifest root, so
it can come from any untrusted peer or accelerator.

## GUI dependency note

`gpui` and `gpui-component` are pulled from **crates.io** (`gpui = "0.2"`,
`gpui-component = "0.5"`), not from the `zed-industries/zed` git repo. `gpui` now
publishes releases and `gpui-component` tracks a matching `gpui` version, so no git-rev
pinning is needed. A future-incompat warning from `proc-macro-error2` (transitive via
`gpui-component`'s macros) is expected and not actionable here.
