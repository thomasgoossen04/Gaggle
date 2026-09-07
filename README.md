<div align="center">

<img src="docs/logo.png" alt="Gaggle" width="120">

<h1>Gaggle</h1>

<p><strong>Share very large folders over private, invite-only peer-to-peer swarms.</strong></p>

<p>
  <img src="https://img.shields.io/badge/license-MIT-3b5bdb?style=flat-square" alt="MIT license">
  <img src="https://img.shields.io/badge/platforms-Linux%20%C2%B7%20Windows%20%C2%B7%20macOS-6c757d?style=flat-square" alt="Platforms">
  <img src="https://img.shields.io/badge/built%20with-Rust-b7410e?style=flat-square" alt="Built with Rust">
</p>

</div>

Gaggle moves 100 GB+ folders — modded game installs, media libraries, datasets — between
people who already trust each other. Peers trade content-addressed chunks directly;
optional always-on **accelerator nodes** keep a share available and fast when nobody else
is online.

Content is chunked, hashed and Merkle-verified with BLAKE3, like a torrent. Unlike a
torrent, there is no public tracker or browsable DHT: each share has an owner, an Ed25519
identity, and signed invite tokens that can be scoped to specific files and set to expire.

## Contents

- [Install](#install)
- [Quick start](#quick-start)
- [Features](#features)
- [How it works](#how-it-works)
- [Run an accelerator](#run-an-accelerator)
- [Build from source](#build-from-source)

## Install

| Platform | Stable | Beta |
|---|---|---|
| macOS (Apple Silicon) | [Gaggle.dmg](https://github.com/thomasgoossen04/Gaggle/releases/latest/download/Gaggle-macos-aarch64.dmg) | [Gaggle.dmg](https://github.com/thomasgoossen04/Gaggle/releases/download/beta/Gaggle-macos-aarch64.dmg) |
| macOS (Intel) | [Gaggle.dmg](https://github.com/thomasgoossen04/Gaggle/releases/latest/download/Gaggle-macos-x86_64.dmg) | [Gaggle.dmg](https://github.com/thomasgoossen04/Gaggle/releases/download/beta/Gaggle-macos-x86_64.dmg) |
| Windows (x86_64) | [gaggle-launcher.exe](https://github.com/thomasgoossen04/Gaggle/releases/latest/download/gaggle-launcher-windows-x86_64.exe) | [gaggle-launcher.exe](https://github.com/thomasgoossen04/Gaggle/releases/download/beta/gaggle-launcher-windows-x86_64.exe) |
| Linux (x86_64) | [gaggle-launcher](https://github.com/thomasgoossen04/Gaggle/releases/latest/download/gaggle-launcher-linux-x86_64) | [gaggle-launcher](https://github.com/thomasgoossen04/Gaggle/releases/download/beta/gaggle-launcher-linux-x86_64) |

- **macOS** — open the `.dmg` and drag **Gaggle** to **Applications**. The app checks for
  updates on launch and updates itself in place.
- **Windows / Linux** — run `gaggle-launcher`. It installs the app under your user data
  directory, adds a menu entry, and checks for updates on every launch. (Linux: `chmod +x`
  it first.)

**Stable** is the latest build off `main`; **Beta** is a rolling, less-tested build off the
`beta` branch. Both links always resolve to that channel's current build — bookmark them.
Switch channels with the in-app **Update channel** dropdown (Advanced mode), the launcher
window's `CH` toggle, or `gaggle-launcher --channel beta`.

## Quick start

### Share a folder

1. **Shares** tab → **Add folder** (or **Add private folder** to require an invite for
   every connection). A short form opens: give the share a display name, version and
   description, and add **launch entries** — a game's `.exe`, a server's `start.sh`, a
   `.bat` — each tagged with the OS it's built for. All optional; leave it blank to skip.
2. Gaggle writes what you entered into the folder as `.gaggle-meta.toml`, indexes the
   folder in place (no copy) and starts seeding. **Copy link** to share it.
3. For a private share, expand its row, pick a scope (whole share or specific files) and
   an optional expiry, and **Mint invite**. Send the `gaggleshare1…` token to the other
   person. An invite can only be revoked by letting it expire, so keep scopes and expiries
   tight for anything sensitive.

### Join a share

1. **Transfers** tab → paste a share link or invite token.
   Or, if you've set a **Settings → Rendezvous URL**, use **Browse public shares** to
   pick one from that accelerator's directory — no link needed.
2. A picker opens with the share's contents: tick the files and folders you want (or
   **Select all**), choose where to download them, and start. Downloading a strict
   subset is a leech-only copy — it won't seed back, since its file set has a
   different id than the origin's.
3. Watch progress per row: transferred / total, speed, source count, time left. Expand a
   row to see the per-source chunk breakdown.
4. A transfer seeds the chunks it already has while still downloading, and keeps serving
   the whole share once done. Pause per row, or turn it off under **Settings → Startup**.
5. **Verify & repair** on a finished download re-checks every file against the manifest
   and refetches only the parts that no longer match (a local check when the tree is
   intact — nothing is pulled).
6. If the share carries launch entries, each row gets a **▶ Run** button. A Windows
   entry on Linux/macOS runs through Wine (set a Proton wrapper under
   **Settings → Wine/Proton command** if you have one).

### Keep a copy up to date

A completed transfer's row has **Check updates** (ask the source for its version) and
**Resync** (pull only the changed chunks — new files added, removed files deleted, changed
files patched). On the owner's side, **Rescan** re-indexes a share and bumps its version.
A rescanned private share needs a fresh invite, since invites pin one manifest.

## Features

- **Built for huge folders.** Content-defined chunking and a Merkle tree per file mean
  seeding 100 GB costs a bounded RAM cache, not a second on-disk copy.
- **Private by invite.** Every share has its own keypair. Access needs a signed token,
  scoped to the share or specific files, with an optional expiry.
- **Multi-source downloads.** Pulls from every seed and replica at once, rarest chunk
  first, routing around dead or partial sources.
- **Pick what you download.** A pre-download picker shows the share's file tree; take
  the whole thing or just the files you want, into a folder you choose.
- **Verify & repair.** Re-check a finished download against the manifest and refetch only
  the pieces that don't match — Gaggle's equivalent of "verify integrity".
- **Share metadata & launch.** Attach a name, version, description and per-OS launch
  entries to a share; they travel with it as `.gaggle-meta.toml` and give every
  downloader a **▶ Run** button — with a Wine/Proton path for a Windows game on
  Linux/macOS.
- **Seeds while downloading.** A transfer uploads the chunks it already holds and keeps
  serving after it finishes.
- **NAT traversal.** mDNS on the LAN, UPnP for a direct port, accelerator-assisted
  rendezvous hole-punching, and a full libp2p relay circuit as the fallback.
- **Delta sync.** Re-syncing a changed share moves only the chunks that changed.
- **Encrypted, compressed transfer.** Each chunk is compressed (when that helps) and
  sealed before it leaves, on top of QUIC's TLS — one chunk at a time, so nothing waits
  on a whole-share pass.
- **Accelerators.** Optional always-on nodes that cache hot chunks (relay role) or hold a
  full replica (NAS role). One carries many shares and can be driven remotely over a
  signed admin API.
- **Uses every core.** Chunking, hashing, verification and compression run off the
  network thread.
- **Throughput graphs.** The **Stats** tab plots up/down speed over 1m / 5m / 15m / 1h,
  for this machine or any connected remote accelerator.
- **Simple or advanced.** The GUI opens with Transfers, Shares, Stats and Settings.
  **Advanced mode** adds the Accelerator and Logs tabs and the editable network fields.
- **Themes.** System (follows the OS) plus Dark, Light, Dracula, Nord, Gruvbox, Tokyo
  Night, Catppuccin, Solarized and Rosé Pine Dawn.

## How it works

The network is split into two planes:

- **Data plane** — peer-to-peer and peer-to-accelerator chunk transfer over QUIC (via
  `rust-libp2p`), independently multiplexed so many chunks move at once.
- **Control plane** — plain HTTPS for low-volume traffic: invite exchange, NAT
  rendezvous, the seeder tracker, and the accelerator admin API.

**Trust flows from the manifest.** A share's manifest id is authenticated by a signed
invite; every chunk is verified against the manifest's Merkle root no matter which peer
or accelerator sent it. Chunk data itself is never trusted, only counted.

## Run an accelerator

An accelerator keeps a share available and fast when the owner is offline. Two roles:
**relay** (a bandwidth-heavy hot-chunk cache) and **NAS** (a storage-heavy full replica).

### From the GUI

Turn on **Settings → Advanced mode**, open the **Accelerator** tab, and either:

- **Add a remote accelerator** by its label, admin URL and public key, to manage a
  headless daemon from here — the common case.
- **Start relay** or **Start NAS** on this machine after **Benchmark** suggests a role.

Each carried share has a **Seed** toggle (pause uploading without dropping the copy) and
**Remove**. Removing a NAS share deletes its on-disk replica (you're asked first). NAS
replicas are stored zstd-compressed.

A NAS share starts uploading the chunks it already holds **while** it is still
replicating — it doesn't wait for the whole folder to land first — and each share
replicates concurrently. If a share can't reach its source (a restart before the origin
is back, a stale address), it retries on its own with a short backoff instead of stopping.

### Headless daemon

For a spare server, NAS or VPS with no GUI:

```bash
curl -fsSL -o gaggle-accelerator \
  https://github.com/thomasgoossen04/Gaggle/releases/latest/download/gaggle-accelerator-linux-x86_64
chmod +x gaggle-accelerator
./gaggle-accelerator run --role relay        # or: --role nas
```

```bash
./gaggle-accelerator identity                # print its public key
./gaggle-accelerator authorize <operator-key-hex>   # let your GUI manage it
./gaggle-accelerator share add gaggleshare1… # queue a share, offline
./gaggle-accelerator share rm <manifest-id>  # stop carrying it (deletes the NAS replica)
```

Windows and macOS builds are published alongside the launcher under the same
`releases/latest/download/` (stable) and `releases/download/beta/` (beta) paths.

<details>
<summary>Auto-updating via systemd</summary>

`gaggle-accelerator-launcher` is the headless counterpart of the desktop launcher: on
every start it installs a newer `accelerator` build if one exists (falling back to the
installed one if the check fails), then runs the daemon.

```bash
curl -fsSL -o gaggle-accelerator-launcher \
  https://github.com/thomasgoossen04/Gaggle/releases/latest/download/gaggle-accelerator-launcher-linux-x86_64
chmod +x gaggle-accelerator-launcher
sudo mv gaggle-accelerator-launcher /usr/local/bin/

gaggle-accelerator-launcher service --role relay --install
systemctl --user daemon-reload
systemctl --user enable --now gaggle-accelerator.service
loginctl enable-linger "$USER"
```

`service` without `--install` just prints the unit. `check` / `update` and `--channel beta`
work like the desktop launcher.

</details>

<details>
<summary>Rendezvous and the seeder tracker</summary>

Any running accelerator doubles as a **rendezvous point**: put its `http://host:port` into
**Settings → Rendezvous URL** on both ends of a transfer and two peers behind NAT can
swap addresses and punch a direct hole, with no chunk data routed through the accelerator.

The same URL enables the **seeder tracker**: every folder this node serves announces
itself there, and every download asks who else has the share first. That's how a download
fans out across a NAS replica and the origin even when the link only named the origin, and
it powers **Browse public shares**. Private shares announce too (so invite holders fan out
across replicas) but stay off the public list. No chunk data or share secret touches the
tracker.

</details>

<details>
<summary>Reaching a NAS behind NAT from outside its network</summary>

A standalone daemon hosts rendezvous/tracker endpoints for others but doesn't register
itself. For a NAS reachable only over a VPN, point it at a public accelerator:

```bash
./gaggle-accelerator run --role nas \
  --rendezvous-url https://relay.example:8749 \
  --public-relay /ip4/203.0.113.4/udp/4001/quic-v1/p2p/12D3Koo…relay
```

`--rendezvous-url` (the same accelerator your downloaders use) makes the daemon answer NAT
punches and announce to that tracker over HTTP. `--public-relay` makes each replica
reserve a relay circuit on boot so it's dialable while `dcutr` upgrades to direct. Both
persist to `config.toml`; pass an empty string to clear. A relay-role daemon only needs
`--rendezvous-url`.

</details>

## Build from source

Requires a recent stable Rust toolchain.

```bash
git clone https://github.com/thomasgoossen04/Gaggle.git
cd Gaggle
cargo build --release -p gui -p launcher -p accelerator -p accelerator-launcher
```

On Linux the GUI needs the usual libraries for a windowed GPU app (Vulkan,
`libxkbcommon`, Wayland/X11, fontconfig); Windows and macOS need nothing extra.

```bash
cargo run -p gui                              # desktop app
cargo run -p accelerator -- run --role relay  # headless accelerator daemon
```

### Development

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
```

See [`CLAUDE.md`](CLAUDE.md) for the full architecture, crate-by-crate breakdown, and the
release process.

## License

MIT — see [LICENSE.md](LICENSE.md).
