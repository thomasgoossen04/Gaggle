//! Headless accelerator daemon.
//!
//! It keeps a **persistent Ed25519 identity** (printed on every start) and a
//! `config.toml` under its home directory, accelerates a *list* of shares, and
//! exposes a signed-request **admin API** so an operator can add / remove
//! shares and read status remotely.
//!
//! ```text
//! accelerator run --role relay                 # start the daemon
//! accelerator identity                          # print the public key, exit
//! accelerator authorize <operator-key-hex>      # let an operator manage it
//! accelerator share add gaggleshare1…           # offline: queue a share
//! ```

mod config;
mod run;
mod supervisor;

use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};

use crate::config::{AcceleratorConfig, Home, Role};
use crate::run::Overrides;

#[derive(Debug, Parser)]
#[command(name = "accelerator", about = "P2P folder-share accelerator daemon")]
struct Cli {
    /// State directory (identity + config.toml). Defaults to
    /// $GAGGLE_ACCEL_HOME or the per-OS data dir + /gaggle/accelerator.
    #[arg(long, global = true)]
    home: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the daemon (default).
    Run(RunArgs),
    /// Print the daemon's peer id + public key and exit.
    Identity,
    /// Authorise an operator key (hex) to use the admin API.
    Authorize {
        /// The operator's public key, 64 hex chars.
        key: String,
    },
    /// Offline edits to the share list (a running daemon uses the admin API).
    Share {
        #[command(subcommand)]
        cmd: ShareCmd,
    },
}

#[derive(Debug, Subcommand)]
enum ShareCmd {
    /// Add a `gaggleshare1…` token to the boot list.
    Add { token: String },
    /// Remove a share by its manifest id.
    Rm { manifest_id: String },
    /// List configured share tokens.
    Ls,
}

#[derive(Debug, Default, clap::Args)]
struct RunArgs {
    /// Role to run as on first start (persisted to config.toml).
    #[arg(long, value_enum)]
    role: Option<Role>,
    /// Relay hot-chunk cache budget, MiB.
    #[arg(long)]
    cache_mib: Option<u64>,
    /// NAS replica root directory.
    #[arg(long = "dir")]
    replica_dir: Option<String>,
    /// NAS role: store the replica raw instead of zstd-compressed on disk.
    #[arg(long)]
    no_compress_replica: bool,
    /// host:port for the admin API.
    #[arg(long)]
    admin_listen: Option<String>,
    /// host:port for NAT-rendezvous, if different from --admin-listen (e.g.
    /// admin behind a VPN, rendezvous on a public port). Pass an empty string
    /// to go back to serving both on --admin-listen.
    #[arg(long)]
    rendezvous_listen: Option<String>,
    /// Multiaddr to listen on, e.g. /ip4/0.0.0.0/udp/4001/quic-v1.
    #[arg(long)]
    listen: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let home = Home::resolve(cli.home);

    match cli.command.unwrap_or(Command::Run(RunArgs::default())) {
        Command::Run(args) => {
            tracing::info!("{}", net::describe());
            tracing::info!("{}", control_plane::describe());
            run::run(
                home,
                Overrides {
                    role: args.role,
                    cache_mib: args.cache_mib,
                    replica_dir: args.replica_dir,
                    compress_replica: if args.no_compress_replica { Some(false) } else { None },
                    admin_listen: args.admin_listen,
                    rendezvous_listen: args.rendezvous_listen,
                    listen: args.listen,
                },
            )
            .await
        }
        Command::Identity => {
            let identity = net::load_or_create_identity(&home.identity_path())?;
            let seed = net::identity_seed(&identity)?;
            let agent = gaggle_core::AgentKeypair::from_seed(seed).public();
            println!("peer id:    {}", identity.public().to_peer_id());
            println!("public key: {}", agent.to_hex());
            println!("home:       {}", home.dir().display());
            Ok(())
        }
        Command::Authorize { key } => {
            let id = gaggle_core::AgentId::from_hex(key.trim())
                .context("that is not a 64-hex-char operator key")?;
            let path = home.config_path();
            let mut config = AcceleratorConfig::load(&path)?;
            let hex = id.to_hex();
            if config.authorized_keys.iter().any(|k| k.trim() == hex) {
                println!("{hex} is already authorised");
            } else {
                config.authorized_keys.push(hex.clone());
                config.save(&path)?;
                println!("authorised {hex}");
                println!("restart the daemon for it to take effect");
            }
            Ok(())
        }
        Command::Share { cmd } => {
            let path = home.config_path();
            let mut config = AcceleratorConfig::load(&path)?;
            match cmd {
                ShareCmd::Ls => {
                    for token in &config.shares {
                        println!("{token}");
                    }
                }
                ShareCmd::Add { token } => {
                    let link = net::ShareLink::parse(token.trim())
                        .context("that is not a valid gaggleshare1… link")?;
                    if config.shares.iter().any(|t| t.trim() == token.trim()) {
                        println!("already listed");
                    } else {
                        config.shares.push(token.trim().to_string());
                        config.save(&path)?;
                        println!("added “{}” ({})", link.name, link.manifest_id);
                    }
                }
                ShareCmd::Rm { manifest_id } => {
                    let want = manifest_id.trim();
                    let before = config.shares.len();
                    config.shares.retain(|t| {
                        net::ShareLink::parse(t)
                            .map(|l| l.manifest_id.to_hex() != want)
                            .unwrap_or(true)
                    });
                    if config.shares.len() == before {
                        println!("no configured share with manifest id {want}");
                    } else {
                        config.save(&path)?;
                        // Reclaim the on-disk replica too (NAS role). Best-effort:
                        // a relay has none, and a missing dir is fine.
                        let replica = config.resolved_replica_dir(&home).join(want);
                        match std::fs::remove_dir_all(&replica) {
                            Ok(()) => println!("removed {want} and its replica at {}", replica.display()),
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                                println!("removed {want}")
                            }
                            Err(e) => println!("removed {want} (could not delete {}: {e})", replica.display()),
                        }
                    }
                }
            }
            Ok(())
        }
    }
}
