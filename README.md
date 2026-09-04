# Gaggle

## Download the launcher

Direct links to the standalone `gaggle-launcher` from the latest release of each channel.
Run it and it installs the rest itself (see [Getting started](#getting-started) below).

| Platform | Stable | Beta |
|---|---|---|
| Linux (x86_64) | [Download](https://github.com/thomasgoossen04/Gaggle/releases/latest/download/gaggle-launcher-linux-x86_64) | [Download](https://github.com/thomasgoossen04/Gaggle/releases/download/beta/gaggle-launcher-linux-x86_64) |
| Windows (x86_64) | [Download](https://github.com/thomasgoossen04/Gaggle/releases/latest/download/gaggle-launcher-windows-x86_64.exe) | [Download](https://github.com/thomasgoossen04/Gaggle/releases/download/beta/gaggle-launcher-windows-x86_64.exe) |
| macOS (Apple Silicon) | [Download](https://github.com/thomasgoossen04/Gaggle/releases/latest/download/gaggle-launcher-macos-aarch64) | [Download](https://github.com/thomasgoossen04/Gaggle/releases/download/beta/gaggle-launcher-macos-aarch64) |
| macOS (Intel) | [Download](https://github.com/thomasgoossen04/Gaggle/releases/latest/download/gaggle-launcher-macos-x86_64) | [Download](https://github.com/thomasgoossen04/Gaggle/releases/download/beta/gaggle-launcher-macos-x86_64) |

Stable tracks the latest non-prerelease build off `main`; beta is a rolling, less-tested
build off the `beta` branch, overwritten on every push. Both links always resolve to that
channel's current build — bookmark them, they don't need updating.

## Download the accelerator

For a headless box (a spare server, a NAS, a VPS) running just the
[accelerator daemon](#run-an-accelerator) with no GUI — copy-paste onto the machine:

```bash
# Stable
curl -fsSL -o gaggle-accelerator https://github.com/thomasgoossen04/Gaggle/releases/latest/download/gaggle-accelerator-linux-x86_64
chmod +x gaggle-accelerator
./gaggle-accelerator run --role relay
```

```bash
# Beta
curl -fsSL -o gaggle-accelerator https://github.com/thomasgoossen04/Gaggle/releases/download/beta/gaggle-accelerator-linux-x86_64
chmod +x gaggle-accelerator
./gaggle-accelerator run --role relay
```

Swap `--role relay` for `--role nas` for a full durable replica instead of a hot-chunk
cache. Windows and macOS builds are published too, as `gaggle-accelerator-windows-x86_64.exe`,
`gaggle-accelerator-macos-aarch64` and `gaggle-accelerator-macos-x86_64` alongside the
launcher under the same `releases/latest/download/` (stable) and `releases/download/beta/`
(beta) paths.

Gaggle is a hybrid-P2P application for sharing very large folders (100GB+ — think modded
game installs, media libraries, datasets) over private, invite-based swarms. Peers exchange
content-addressed chunks directly with each other; optional always-on **accelerator nodes**
improve availability and throughput without needing anyone to stay online.

Content is chunked, hashed and Merkle-verified (BLAKE3) the same way a torrent is, but
sharing is **invite-only** rather than public: a share has an owner, an Ed25519 identity,
and signed, scoped, revocable-by-expiry invite tokens — no public tracker, no DHT anyone
can browse.

## Features

- **Huge folders, bounded memory.** Content-defined chunking + a Merkle tree per file mean
  seeding a 100GB folder costs a bounded RAM cache, not a second on-disk copy or a full
  index held in memory.
- **Private, invite-based swarms.** Every share has its own keypair. Invites are signed,
  bearer tokens (`gaggleshare1…`) scoped to the whole share or specific files, with an
  optional expiry — no invite, no access.
- **Multi-peer swarming.** Downloads pull from several sources at once, scheduling
  rarest-chunk-first with per-peer concurrency caps, and route around dead or partial
  sources automatically.
- **NAT traversal built in.** Kademlia DHT peer discovery, libp2p relay circuits, and
  `dcutr` hole-punching — two peers behind NAT still end up talking directly when possible.
- **Delta sync.** Re-scanning a changed folder and re-syncing a subscribed copy only moves
  the chunks that actually changed — not the whole share again.
- **Accelerators.** Optional always-on nodes that either cache hot chunks for many
  downloaders (relay role) or hold a full durable replica (NAS role), so a share stays
  available when the original owner is offline. One accelerator can carry many shares, and
  can be driven remotely over a signed admin API.
- **Cross-platform desktop GUI**, a headless accelerator daemon, and a self-updating
  launcher — see "Getting started" below.

## How it works, briefly

Gaggle splits the network into two independent planes:

- **Data plane** — peer-to-peer and peer-to-accelerator chunk transfer over QUIC
  (via `rust-libp2p`), independently multiplexed so many chunks move concurrently with no
  head-of-line blocking.
- **Control plane** — plain HTTPS for the low-volume stuff: invite exchange and the
  accelerator admin API.

Trust flows from the manifest: a share's manifest id is authenticated by a signed invite
capability, and every chunk is verified against the manifest's Merkle root regardless of
which peer or accelerator it came from — so chunk data itself never needs to be trusted,
only counted.

## Getting started

### Install (recommended)

Grab `gaggle-launcher` for your platform from the [table above](#download-the-launcher)
and run it. It installs itself natively:

- **Linux** — adds a Gaggle entry to your applications menu.
- **Windows** — adds a Start Menu entry.
- **macOS** — installs `Gaggle.app` under `~/Applications`.

A desktop shortcut is opt-in (a checkbox in the launcher window, or `--desktop-shortcut`
on the command line). Every launch re-checks for updates; if you're already current (or
the check fails and you're offline) it hands off straight to the app with no extra window.
Open it again any time — from the shortcut it just created — to update or launch.

Two release channels are available: **stable** (default) and **beta** (newer, less
tested). Switch with the `CH` toggle in the launcher window, or
`gaggle-launcher --channel beta`.

### Build from source

Requires a recent stable Rust toolchain.

```bash
git clone https://github.com/thomasgoossen04/Gaggle.git
cd Gaggle
cargo build --release -p gui -p launcher -p accelerator
```

The GUI needs the usual system libraries for a windowed GPU app on Linux
(Vulkan, `libxkbcommon`, Wayland/X11, fontconfig); Windows and macOS need nothing extra.

```bash
cargo run -p gui          # desktop app
cargo run -p launcher     # installer / updater / launcher
cargo run -p accelerator -- run --role relay   # headless accelerator daemon
```

## Quick usage guide

### Share a folder

1. Open the **Shares** tab and click **Add folder**.
2. Pick a folder. Gaggle indexes it (streaming — it doesn't copy the folder anywhere) and
   starts seeding.
3. Click **Copy link** to get a share link for a *public* share (anyone with the link and
   a route to you can pull it), or use **Add private folder** to mint a per-share identity
   up front and require an invite for every connection.

### Invite someone to a private share

1. In the **Shares** tab, expand the private share's row (▸).
2. Pick a scope — the whole share, or specific files — and optionally an expiry.
3. Click **Mint invite** and send the resulting `gaggleshare1…` token to whoever you're
   sharing with. Each invite is independently revocable by letting it expire; there's no
   way to claw back a still-valid one, so mint tight scopes/expiries for anything sensitive.

### Join a share

1. Open the **Transfers** tab.
2. Paste a share link or invite token into the field and confirm.
3. Watch progress in the transfer list — pause/resume any time, and expand a row (▸) to
   see the per-source chunk breakdown while it's swarming from multiple peers.

### Keep a synced copy up to date

Once a transfer completes, its row offers:

- **Check updates** — asks the source for its current version, no download yet.
- **Resync** — pulls down only the changed chunks and applies them: new files arrive,
  removed files are deleted, changed files are patched in place.

The owner's side: **Rescan** on a seeded share re-indexes it and bumps its version so
subscribers see an update is available. A rescanned *private* share needs a fresh invite,
since the invite is pinned to a specific manifest.

### Run an accelerator

From the GUI's **Accelerator** tab: **Benchmark** measures your disk throughput and free
space and suggests a role, then **Start relay** (bandwidth-heavy hot-chunk cache) or
**Start NAS** (storage-heavy full replica) with one or more share links pasted in. The
card that appears lists every share it's carrying, with per-share add/remove and live
cache/replica stats.

Headless, for something that should run unattended on its own machine:

```bash
cargo run -p accelerator -- run --role relay --cache-mib 4096
cargo run -p accelerator -- identity                # print its public key
cargo run -p accelerator -- authorize <operator-key-hex>   # let yourself manage it remotely
cargo run -p accelerator -- share add gaggleshare1…  # queue a share to carry, offline
```

Once authorized, add it as a **remote accelerator** from the GUI's Accelerator tab (label +
its admin URL + the public key it printed) to manage its shares and watch its status from
anywhere, without a `net`/libp2p connection between your machine and it — that traffic all
goes over the signed HTTPS admin API.

## Development

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
```

See `CLAUDE.md` for the full architecture writeup, crate-by-crate breakdown, and the
release/versioning process.

## License

MIT — see [LICENSE.md](LICENSE.md).
