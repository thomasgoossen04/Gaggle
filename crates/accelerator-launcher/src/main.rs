//! `gaggle-accelerator-launcher` — headless auto-updating launcher for the
//! `accelerator` daemon binary.
//!
//! It is the headless, automatic counterpart of `gaggle-launcher`: no window,
//! no interaction, designed to be the `ExecStart=` of a systemd unit (see
//! `service`). On every `run` it best-effort-updates the accelerator daemon
//! to the selected channel's latest build (falling back to whatever's already
//! installed if the network check fails — a transient outage must never stop
//! the daemon from starting), then **execs** it with whatever arguments
//! follow `--`. Exec'ing (not spawning a child) means systemd tracks the
//! daemon's own PID and exit code directly, so `Restart=` policies apply to
//! the real thing and every restart re-triggers the update check.
//!
//! ```text
//! gaggle-accelerator-launcher run -- run --role relay --listen /ip4/0.0.0.0/udp/4001/quic-v1
//! gaggle-accelerator-launcher check                  # 0 = up to date, 10 = update available
//! gaggle-accelerator-launcher update                  # install the latest build, then exit
//! gaggle-accelerator-launcher service --role relay    # print a systemd user-unit template
//! ```

mod channel;
mod manifest;
mod paths;
mod service;
mod signing;
mod updater;

use anyhow::Context;
use channel::Channel;
use clap::{Parser, Subcommand};
use updater::Updater;

/// `2.0.<short-commit-hash>`, baked in by `build.rs`.
const VERSION: &str = env!("GAGGLE_VERSION");

#[derive(Parser)]
#[command(
    name = "gaggle-accelerator-launcher",
    version = VERSION,
    about = "Headless auto-updating launcher for the Gaggle accelerator daemon"
)]
struct Cli {
    /// Release channel to track: `stable` (default) or `beta` (pre-release
    /// test builds). Remembered for next time.
    #[arg(long, value_enum, global = true)]
    channel: Option<Channel>,
    /// Override the descriptor URL entirely (else $GAGGLE_UPDATE_URL, else the
    /// selected channel's URL).
    #[arg(long, global = true, value_name = "URL")]
    manifest_url: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Best-effort update, then exec the installed `accelerator` binary with
    /// whatever arguments follow `--` (e.g. `run --role relay --listen …`).
    Run {
        /// Skip the update check and just exec whatever's already installed.
        #[arg(long)]
        no_update: bool,
        #[arg(last = true)]
        accelerator_args: Vec<String>,
    },
    /// Print update status and exit: 0 = up to date, 10 = update/install needed, 1 = error.
    Check,
    /// Download + install the latest accelerator build headlessly, then exit.
    Update,
    /// Print (or `--install`) a systemd user-unit template for `run`.
    Service(service::ServiceArgs),
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Resolve the channel: --channel > $GAGGLE_UPDATE_CHANNEL > persisted > stable.
    // An explicit --channel is remembered for next time.
    let ch = cli
        .channel
        .or_else(|| std::env::var("GAGGLE_UPDATE_CHANNEL").ok().and_then(|s| Channel::parse(&s)))
        .unwrap_or_else(channel::load);
    if let Some(explicit) = cli.channel {
        let _ = channel::save(explicit);
    }

    let up = match updater::url_override(cli.manifest_url.as_deref()) {
        Some(url) => Updater::with_url(url),
        None => Updater::for_channel(ch),
    };

    match cli.cmd {
        Cmd::Run { no_update, accelerator_args } => run(&up, no_update, &accelerator_args),
        Cmd::Check => headless_check(&up),
        Cmd::Update => headless_update(&up),
        Cmd::Service(args) => service::run(args),
    }
}

/// Best-effort update (never fatal — an unreachable network just means "run
/// what's already installed", same as the desktop launcher's offline
/// hand-off), then exec the installed binary.
fn run(up: &Updater, no_update: bool, args: &[String]) -> anyhow::Result<()> {
    if no_update {
        eprintln!("gaggle-accelerator-launcher: --no-update set, skipping the update check");
    } else {
        match up.install_blocking() {
            Ok(version) => eprintln!("gaggle-accelerator-launcher: running {version}"),
            Err(e) => eprintln!(
                "gaggle-accelerator-launcher: update check failed ({e:#}); running whatever is \
                 already installed"
            ),
        }
    }

    let bin = paths::accelerator_binary()?;
    anyhow::ensure!(
        bin.exists(),
        "no accelerator binary installed at {} — check your network / --manifest-url and try \
         again, or run `gaggle-accelerator-launcher update` first",
        bin.display()
    );
    exec_accelerator(&bin, args)
}

/// Replace this process with the accelerator binary (Unix: `execvp`, so
/// systemd/journald see the daemon's own PID and stdio directly — this call
/// only returns on failure). Non-Unix targets have no equivalent, so they
/// spawn a child and forward its exit code instead.
#[cfg(unix)]
fn exec_accelerator(bin: &std::path::Path, args: &[String]) -> anyhow::Result<()> {
    use std::os::unix::process::CommandExt;
    let err = std::process::Command::new(bin).args(args).exec();
    Err(err).with_context(|| format!("exec {}", bin.display()))
}

#[cfg(not(unix))]
fn exec_accelerator(bin: &std::path::Path, args: &[String]) -> anyhow::Result<()> {
    let status = std::process::Command::new(bin)
        .args(args)
        .status()
        .with_context(|| format!("spawn {}", bin.display()))?;
    std::process::exit(status.code().unwrap_or(1));
}

fn headless_check(up: &Updater) -> anyhow::Result<()> {
    let manifest = match up.fetch() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };
    match updater::decide(updater::installed_record().as_ref(), &manifest.version, up.channel()) {
        updater::Status::UpToDate { version } => {
            println!("up-to-date {version}");
            Ok(())
        }
        updater::Status::UpdateAvailable { version } => {
            println!("update-available {version}");
            std::process::exit(10);
        }
        updater::Status::NotInstalled { version } => {
            println!("not-installed {version}");
            std::process::exit(10);
        }
    }
}

fn headless_update(up: &Updater) -> anyhow::Result<()> {
    let version = up.install_blocking()?;
    println!("installed {version}");
    Ok(())
}
