//! `gaggle-launcher` — installs, updates and launches the Gaggle desktop app.
//!
//! Default (no subcommand) opens a small decorationless window styled like the
//! main GUI. `check` / `update` are headless for scripting.

// No console by default (a plain Windows binary launches with one attached,
// which is what flashes up behind the window from a desktop shortcut); `main`
// re-attaches to the parent's console on start when one exists (i.e. this was
// run from an existing terminal), so `check`/`update`'s printed output still
// reaches a script or an interactive shell. Debug builds keep the normal
// console — `cargo run` is still the place for panics and stray `println!`s.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod channel;
mod desktop;
mod manifest;
mod paths;
mod signing;
mod ui;
mod updater;

use channel::Channel;
use clap::{Parser, Subcommand};
use gpui::prelude::*;
use gpui::{
    Application, Bounds, TitlebarOptions, WindowAppearance, WindowBackgroundAppearance,
    WindowBounds, WindowDecorations, WindowOptions, point, px, size,
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
    /// Also create a desktop shortcut (the apps-menu / Start Menu entry is
    /// always created). Only takes effect on an install/update.
    #[arg(long, global = true)]
    desktop_shortcut: bool,
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

/// Re-attach to the console of whatever launched this process, if any — a
/// no-op no-console build (`windows_subsystem = "windows"`) otherwise has no
/// way to print `check`/`update`'s output when run from an existing shell.
/// Launched from a desktop shortcut (no parent console) this is a silent
/// no-op, which is exactly the point.
#[cfg(windows)]
fn attach_parent_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(windows))]
fn attach_parent_console() {}

fn main() -> anyhow::Result<()> {
    attach_parent_console();
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
    if cli.desktop_shortcut {
        up.set_desktop_shortcut(true);
    }

    match cli.cmd.unwrap_or(Cmd::Run) {
        Cmd::Run => run(up),
        Cmd::Check => headless_check(&up),
        Cmd::Update => headless_update(&up),
    }
}

/// Default entry point: silently hand off to the installed GUI when there's
/// nothing to show the user (already current, or offline with something
/// installed); otherwise open the launcher window.
fn run(up: Updater) -> anyhow::Result<()> {
    let installed = updater::installed_record();
    let gui_present = paths::gui_binary().map(|p| p.exists()).unwrap_or(false);
    let fetched = up
        .fetch_quick()
        .ok()
        .map(|m| updater::decide(installed.as_ref(), &m.version, up.channel()));

    if updater::wants_auto_launch(installed.is_some(), gui_present, fetched)
        && updater::launch_installed().is_ok()
    {
        return Ok(());
    }

    run_window(up);
    Ok(())
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
        // See `gaggle_ui_kit::fonts` — without this, "monospace" fails to
        // resolve to any font on Windows/macOS.
        gaggle_ui_kit::fonts::install(cx);
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
            // See the matching comment in `gui::main` — `titlebar: None` drops
            // `NSClosableWindowMask` on macOS too, so the window can't even be
            // closed there. `ui::Launcher::render` hides the custom close
            // button on macOS in favor of the (still native) repositioned one.
            titlebar: Some(TitlebarOptions {
                title: None,
                appears_transparent: true,
                traffic_light_position: Some(point(px(9.0), px(9.0))),
            }),
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
