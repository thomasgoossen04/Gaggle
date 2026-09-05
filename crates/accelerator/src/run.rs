//! `accelerator run` — load the persistent identity + `config.toml`, start the
//! [`Supervisor`], and serve the admin API until Ctrl-C.

use anyhow::Context;
use control_plane::admin::AdminState;
use control_plane::{RendezvousRegistry, serve_daemon};
use gaggle_core::AgentKeypair;
use tokio::sync::mpsc;

use crate::config::{AcceleratorConfig, Home, Role};
use crate::supervisor::Supervisor;

/// CLI overrides applied on top of `config.toml` before the daemon starts.
#[derive(Debug, Default)]
pub struct Overrides {
    pub role: Option<Role>,
    pub cache_mib: Option<u64>,
    pub replica_dir: Option<String>,
    pub admin_listen: Option<String>,
    /// `Some("")` clears `AcceleratorConfig::rendezvous_listen` back to
    /// `None` (merged onto `admin_listen`); `Some(addr)` sets it; `None`
    /// leaves whatever `config.toml` already has.
    pub rendezvous_listen: Option<String>,
    pub listen: Option<String>,
}

pub async fn run(home: Home, overrides: Overrides) -> anyhow::Result<()> {
    let identity = net::load_or_create_identity(&home.identity_path())?;
    let seed = net::identity_seed(&identity)?;

    let mut config = AcceleratorConfig::load(&home.config_path())?;
    if let Some(v) = overrides.role {
        config.role = v;
    }
    if let Some(v) = overrides.cache_mib {
        config.cache_mib = v;
    }
    if overrides.replica_dir.is_some() {
        config.replica_dir = overrides.replica_dir;
    }
    if let Some(v) = overrides.admin_listen {
        config.admin_listen = v;
    }
    if let Some(v) = overrides.rendezvous_listen {
        config.rendezvous_listen = if v.trim().is_empty() { None } else { Some(v) };
    }
    if let Some(v) = overrides.listen {
        config.listen = v;
    }
    config.save(&home.config_path()).context("persisting config.toml")?;

    banner(&home, &identity, &config);

    let authorized = config.authorized_ids()?;
    if authorized.is_empty() {
        tracing::warn!(
            "no authorized_keys — the admin API will refuse every request until you run \
             `accelerator authorize <operator-key>` (or edit config.toml) and restart"
        );
    }

    let (cmd_tx, cmd_rx) = mpsc::channel(16);
    let (supervisor, status_rx) = Supervisor::start(
        home.clone(),
        config.clone(),
        AgentKeypair::from_seed(seed),
        identity.clone(),
    )
    .await
    .context("starting the accelerator backend")?;
    let sup_task = tokio::spawn(supervisor.run(cmd_rx));

    let state = AdminState::new(authorized, AgentKeypair::from_seed(seed), cmd_tx, status_rx);
    let rendezvous = RendezvousRegistry::new();
    let listener = tokio::net::TcpListener::bind(&config.admin_listen)
        .await
        .with_context(|| format!("binding admin API to {}", config.admin_listen))?;

    let rendezvous_listener = match &config.rendezvous_listen {
        Some(addr) => Some(
            tokio::net::TcpListener::bind(addr)
                .await
                .with_context(|| format!("binding NAT rendezvous to {addr}"))?,
        ),
        None => None,
    };
    match &config.rendezvous_listen {
        Some(addr) => tracing::info!(
            admin_addr = %config.admin_listen,
            rendezvous_addr = %addr,
            "admin API (https, self-signed — pin the public key above) and NAT rendezvous listening separately"
        ),
        None => tracing::info!(
            addr = %config.admin_listen,
            "admin API (https, self-signed — pin the public key above) + NAT rendezvous listening"
        ),
    }

    tokio::select! {
        r = serve_daemon(listener, state, rendezvous, rendezvous_listener) => r.context("admin/rendezvous server failed")?,
        _ = tokio::signal::ctrl_c() => tracing::info!("Ctrl-C — shutting down"),
    }
    sup_task.abort();
    Ok(())
}

fn banner(home: &Home, identity: &net::Keypair, config: &AcceleratorConfig) {
    let peer_id = identity.public().to_peer_id();
    let agent = AgentKeypair::from_seed(
        net::identity_seed(identity).expect("identity is ed25519"),
    )
    .public();
    tracing::info!("──────────────────────────────────────────────");
    tracing::info!(role = config.role.as_str(), "accelerator daemon");
    tracing::info!(%peer_id, "libp2p peer id");
    tracing::info!(public_key = %agent.to_hex(), "operator-facing identity — hand this to whoever manages the daemon");
    tracing::info!(home = %home.dir().display(), "state directory");
    tracing::info!("──────────────────────────────────────────────");
}
