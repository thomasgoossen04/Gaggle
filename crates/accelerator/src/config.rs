//! On-disk state for the accelerator daemon: a persistent identity key plus a
//! `config.toml` holding the role, the operators allowed to manage it, and the
//! list of shares to accelerate on boot.

use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

/// Which accelerator role the daemon runs as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// High-bandwidth hot-chunk cache + NAT relay / rendezvous point.
    #[default]
    Relay,
    /// Durable on-disk replica; one serving node per share.
    Nas,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Relay => "relay",
            Role::Nas => "nas",
        }
    }
}

/// Where the daemon keeps its identity and config.
#[derive(Debug, Clone)]
pub struct Home(PathBuf);

impl Home {
    /// `--home` if given, else `$GAGGLE_ACCEL_HOME`, else the per-OS data dir
    /// (`~/.local/share` on Linux, `~/Library/Application Support` on macOS,
    /// `%APPDATA%` on Windows) `+ /gaggle/accelerator`, else a temp dir.
    pub fn resolve(explicit: Option<PathBuf>) -> Self {
        if let Some(dir) = explicit {
            return Self(dir);
        }
        if let Some(dir) = std::env::var_os("GAGGLE_ACCEL_HOME") {
            return Self(PathBuf::from(dir));
        }
        if let Some(data) = dirs::data_dir() {
            return Self(data.join("gaggle").join("accelerator"));
        }
        Self(std::env::temp_dir().join("gaggle-accelerator"))
    }

    pub fn dir(&self) -> &Path {
        &self.0
    }

    pub fn identity_path(&self) -> PathBuf {
        self.0.join("identity.key")
    }

    pub fn config_path(&self) -> PathBuf {
        self.0.join("config.toml")
    }

    /// Default replica root when the config does not set one.
    pub fn default_replica_dir(&self) -> PathBuf {
        self.0.join("replica")
    }
}

/// The daemon's `config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AcceleratorConfig {
    pub role: Role,
    /// Multiaddr to listen on. Empty = ephemeral port on every local
    /// interface. Set e.g. `/ip4/0.0.0.0/udp/4001/quic-v1` for a stable port
    /// to open in a firewall/router.
    pub listen: String,
    /// `host:port` for the admin API — TLS-terminated (self-signed, keyed off
    /// this daemon's own identity; see `control_plane::tls`), not plain HTTP.
    pub admin_listen: String,
    /// `host:port` for the NAT-rendezvous endpoints, if different from
    /// `admin_listen`. Rendezvous is unauthenticated by design (any peer
    /// reaching one of this daemon's shares may need it, not just the
    /// operator) — this lets an operator keep the signed admin API on a
    /// private address (e.g. a Tailscale/VPN IP, or `127.0.0.1` behind an SSH
    /// tunnel) while rendezvous sits on a publicly reachable one. `None`
    /// (the default) serves both on `admin_listen`, as before this existed.
    pub rendezvous_listen: Option<String>,
    /// An *external* accelerator's control-plane base URL (e.g.
    /// `https://relay.example:8749`) whose unauthenticated rendezvous + seeder
    /// tracker this daemon uses as a **client**:
    ///
    /// * it answers NAT-rendezvous punch requests aimed at the shares it
    ///   serves (NAS role — a relay is expected to be publicly reachable
    ///   already), and
    /// * it announces every ready share to that tracker over HTTP,
    ///
    /// so a downloader pointed at the same accelerator discovers this daemon
    /// as a source even when no address for it is in the share link. Point it
    /// at the same accelerator the downloading peers use as their
    /// `rendezvous_url`. `None` skips both.
    pub rendezvous_url: Option<String>,
    /// A relay's dialable `…/p2p/<id>` multiaddr. NAS role: every serving node
    /// reserves a circuit slot on it and advertises the resulting
    /// `/p2p-circuit/…` address (via the tracker in `rendezvous_url`), so a
    /// NAT'd replica with no shared network path to a downloader is still
    /// reachable through the relay — dcutr then upgrades to a direct
    /// hole-punch opportunistically. `None` skips it.
    pub public_relay: Option<String>,
    /// Relay role: hot-chunk cache budget in MiB.
    pub cache_mib: u64,
    /// NAS role: replica root. Relative paths resolve under the home dir.
    pub replica_dir: Option<String>,
    /// NAS role: store replica chunks zstd-compressed on disk (only kept for a
    /// chunk when it actually shrinks it). On by default; `accelerator run
    /// --no-compress-replica` turns it off.
    #[serde(default = "default_true")]
    pub compress_replica: bool,
    /// Hex `AgentId`s permitted to call the admin API.
    pub authorized_keys: Vec<String>,
    /// `gaggleshare1…` tokens to accelerate on boot and keep in sync.
    pub shares: Vec<String>,
    /// Manifest-id hex of shares in `shares` an operator has paused: kept in
    /// config and (NAS) kept on disk, but not served until resumed via
    /// `POST /admin/shares/{id}` `{"seeding":true}`.
    #[serde(default)]
    pub paused_shares: Vec<String>,
}

impl Default for AcceleratorConfig {
    fn default() -> Self {
        Self {
            role: Role::Relay,
            listen: String::new(),
            admin_listen: "127.0.0.1:8749".to_string(),
            rendezvous_listen: None,
            rendezvous_url: None,
            public_relay: None,
            cache_mib: 256,
            replica_dir: None,
            compress_replica: true,
            authorized_keys: Vec::new(),
            shares: Vec::new(),
            paused_shares: Vec::new(),
        }
    }
}

fn default_true() -> bool {
    true
}

impl AcceleratorConfig {
    /// Load from `path`; a missing file yields [`AcceleratorConfig::default`].
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Write to `path` (pretty TOML), creating parent directories.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)?;
        std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    /// The listen multiaddr, if one is configured.
    pub fn listen_addr(&self) -> anyhow::Result<Option<net::Multiaddr>> {
        let s = self.listen.trim();
        if s.is_empty() {
            return Ok(None);
        }
        Ok(Some(s.parse().with_context(|| format!("bad listen address {s:?}"))?))
    }

    pub fn resolved_replica_dir(&self, home: &Home) -> PathBuf {
        match &self.replica_dir {
            Some(d) => {
                let p = PathBuf::from(d);
                if p.is_absolute() { p } else { home.dir().join(p) }
            }
            None => home.default_replica_dir(),
        }
    }

    pub fn authorized_ids(&self) -> anyhow::Result<Vec<gaggle_core::AgentId>> {
        self.authorized_keys
            .iter()
            .map(|k| {
                gaggle_core::AgentId::from_hex(k.trim())
                    .with_context(|| format!("bad authorized key {k:?}"))
            })
            .collect()
    }
}
