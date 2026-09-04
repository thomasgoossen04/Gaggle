//! `gaggle-launcher` — installs, updates and launches the Gaggle desktop app.
//!
//! Default (no subcommand) opens a small decorationless window styled like the
//! main GUI. `check` / `update` are headless for scripting.

mod channel;
mod manifest;
mod paths;
mod ui;
mod updater;

use channel::Channel;
use clap::{Parser, Subcommand};
use gpui::prelude::*;
use gpui::{
    Application, Bounds, WindowAppearance, WindowBackgroundAppearance, WindowBounds,
    WindowDecorations, WindowOptions, px, size,
};
use updater::Updater;

/// `2.0.<short-commit-hash>`, baked in by `build.rs`.
const VERSION: &str = env!("GAGGLE_VERSION");

#[derive(Parser)]
#[command(
    name = "gaggle-launcher",
    version = VERSION,
    about = "Install, update and launch the Gaggle desktop app"
)]
struct Cli {
    /// Release channel to track: `stable` (default) or `beta` (pre-release test
    /// builds). Remembered for next time.
    #[arg(long, value_enum, global = true)]
    channel: Option<Channel>,
    /// Override the descriptor URL entirely (else $GAGGLE_UPDATE_URL, else the
    /// selected channel's URL).
    #[arg(long, global = true, value_name = "URL")]
    manifest_url: Option<String>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Open the launcher window (this is the default).
    Run,
    /// Print update status and exit: 0 = up to date, 10 = update/install needed, 1 = error.
    Check,
    /// Download + install the latest build headlessly, then exit.
    Update,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Resolve the channel: --channel > $GAGGLE_UPDATE_CHANNEL > persisted > stable.
    // An explicit --channel is remembered for next launch.
    let ch = cli
        .channel
        .or_else(|| {
            std::env::var("GAGGLE_UPDATE_CHANNEL")
                .ok()
                .and_then(|s| Channel::parse(&s))
        })
        .unwrap_or_else(channel::load);
    if let Some(explicit) = cli.channel {
        let _ = channel::save(explicit);
    }

    let up = match updater::url_override(cli.manifest_url.as_deref()) {
        Some(url) => Updater::with_url(url),
        None => Updater::for_channel(ch),
    };

    match cli.cmd.unwrap_or(Cmd::Run) {
        Cmd::Run => {
            run_window(up);
            Ok(())
        }
        Cmd::Check => headless_check(&up),
        Cmd::Update => headless_update(&up),
    }
}

fn headless_check(up: &Updater) -> anyhow::Result<()> {
    let manifest = match up.fetch() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };
    match updater::decide(
        updater::installed_record().as_ref(),
        &manifest.version,
        up.channel(),
    ) {
        updater::Status::UpToDate { version } => {
            println!("up-to-date {version}");
            Ok(())
        }
        updater::Status::UpdateAvailable { version, .. } => {
            println!("update-available {version}");
            std::process::exit(10);
        }
        updater::Status::NotInstalled { version } => {
            println!("not-installed {version}");
            std::process::exit(10);
        }
        _ => unreachable!("decide only returns the three variants above"),
    }
}

fn headless_update(up: &Updater) -> anyhow::Result<()> {
    let version = up.install_blocking()?;
    println!("installed {version}");
    Ok(())
}

fn run_window(up: Updater) {
    Application::new().run(move |cx| {
        gpui_component::init(cx);
        let dark = matches!(
            cx.window_appearance(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        );
        let mode = gaggle_ui_kit::theme::activate(dark);
        gpui_component::Theme::change(mode, None, cx);

        let up = up.clone();
        // A small, fixed, centred splash — no resize, no minimize (Discord-style).
        let win_size = size(px(380.0), px(300.0));
        let opts = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, win_size, cx))),
            titlebar: None,
            window_decorations: Some(WindowDecorations::Client),
            window_background: WindowBackgroundAppearance::Transparent,
            window_min_size: Some(win_size),
            is_resizable: false,
            is_minimizable: false,
            app_id: Some("gaggle-launcher".into()),
            ..Default::default()
        };
        cx.open_window(opts, |window, cx| {
            let view = cx.new(|cx| ui::Launcher::new(up, window, cx));
            cx.new(|cx| gpui_component::Root::new(view, window, cx))
        })
        .expect("failed to open window");
        cx.activate(true);
    });
}
