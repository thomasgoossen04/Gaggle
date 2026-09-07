//! Gaggle desktop GUI — a `gpui` + `gpui-component` shell over the headless
//! [`app_state::App`] transfer manager.
//!
//! Layout:
//! - [`app`] — the single `gpui` view ([`app::Gaggle`]): state snapshot, tab,
//!   actions. Delegates every pixel to [`ui`].
//! - [`ui`] — stateless element builders: [`ui::widgets`] (themed primitives),
//!   [`ui::chrome`] (title bar + status bar), [`ui::views`] (the four tabs).
//! - [`theme`] — the [`app_state::Theme`]-aware `activate` wrapper over the
//!   shared [`gaggle_ui_kit::theme`] palette; every widget paints from
//!   [`theme::active()`].
//! - [`clipboard`] — Linux clipboard writes that survive the click.
//! - [`util`] — pure formatters.
//!
//! The look is a deliberate "Super Earth field terminal" pastiche — panel
//! surfaces, a single high-viz accent, hazard-stripe rules, targeting brackets
//! on every card, and uppercase monospace chrome.

// A plain Windows binary launches with a console window attached by default —
// this is what makes one flash up behind the GUI. `windows_subsystem =
// "windows"` drops that, but only in release builds: a `cargo run`/`cargo
// build` (debug) keeps the console, since that's still where panics and
// stray `println!`s are most useful during development.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod clipboard;
mod theme;
mod ui;
mod util;

use std::path::PathBuf;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    Application, KeyBinding, Menu, MenuItem, TitlebarOptions, WindowBackgroundAppearance,
    WindowDecorations, WindowOptions, actions, point, px, size,
};

// A unit action so there's something for the macOS app menu's Quit item and
// Cmd-Q / Ctrl-Q to invoke. Without an explicit menu + keybinding a gpui app
// has no way to quit on macOS: Cmd-Q does nothing and the app menu is empty.
actions!(gaggle, [Quit]);

fn config_path() -> Option<PathBuf> {
    // `~/.config` (or `$XDG_CONFIG_HOME`) on Linux, `~/Library/Application
    // Support` on macOS, `%APPDATA%` on Windows.
    dirs::config_dir().map(|base| base.join("gaggle").join("settings.json"))
}

/// `2.0.<short-commit-hash>`, baked in by `build.rs`.
const VERSION: &str = env!("GAGGLE_VERSION");

fn main() -> anyhow::Result<()> {
    if std::env::args().skip(1).any(|a| a == "--version" || a == "-V") {
        println!("gaggle-gui {VERSION}");
        return Ok(());
    }

    // As early as possible, so nothing before it (identity load, first
    // settings read, …) logs unseen. A packaged GUI has no attached console —
    // this is the only place its background-task warnings become visible;
    // the Logs tab reads from the returned handle.
    let logs = app_state::init_logging();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let app = runtime.block_on(app_state::App::new(config_path()))?;
    // The manager's background tasks live on this runtime for the whole process.
    std::mem::forget(runtime);
    let app = Arc::new(app);

    Application::new().run(move |cx| {
        gpui_component::init(cx);
        // Register the embedded monospace font before anything renders — see
        // `gaggle_ui_kit::fonts` for why bundling it (rather than relying on
        // the OS to have a font named "monospace") matters on Windows/macOS.
        gaggle_ui_kit::fonts::install(cx);
        // Seed the palette (and gpui-component's own frame colour) from the
        // persisted setting; `Gaggle::render` keeps it current thereafter.
        let mode = theme::activate(app.snapshot().settings.theme, cx.window_appearance());
        theme::apply_mode(mode, None, cx);

        // Make the app quittable. On macOS this is what puts a working "Quit
        // Gaggle" in the app menu and binds Cmd-Q; without it neither does
        // anything and the process can't be closed from the keyboard or menu.
        // Ctrl-Q covers the Linux/Windows habit. `on_window_closed` makes the
        // last window's close button terminate the process too (the tokio
        // runtime is deliberately leaked, so nothing else would).
        cx.on_action(|_: &Quit, cx: &mut gpui::App| cx.quit());
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("ctrl-q", Quit, None),
        ]);
        cx.set_menus(vec![Menu {
            name: "Gaggle".into(),
            items: vec![MenuItem::action("Quit Gaggle", Quit)],
        }]);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let app = app.clone();
        let opts = WindowOptions {
            // Hide the native title *bar chrome* but keep a `Some(TitlebarOptions)`:
            // on macOS, a bare `titlebar: None` makes gpui build the window
            // without `NSResizableWindowMask` / `NSClosableWindowMask` /
            // `NSMiniaturizableWindowMask` at all, so the window can't be
            // resized and the traffic lights don't exist to click — it silently
            // only "works" on Linux/Windows, whose resize/close/minimize don't
            // key off this. `traffic_light_position` just re-homes the (still
            // native, still functional) macOS buttons under our own header;
            // `ui::chrome::header` hides our custom win-min/max/close cluster
            // on macOS so there's only one set of controls.
            titlebar: Some(TitlebarOptions {
                title: None,
                appears_transparent: true,
                traffic_light_position: Some(point(px(9.0), px(9.0))),
            }),
            window_decorations: Some(WindowDecorations::Client),
            // Transparent so `window_border`'s drop shadow has somewhere to fall.
            window_background: WindowBackgroundAppearance::Transparent,
            // Keep the header (tabs + window controls) from being clipped.
            window_min_size: Some(size(px(820.0), px(560.0))),
            app_id: Some("gaggle".into()),
            ..Default::default()
        };
        cx.open_window(opts, |window, cx| {
            // gpui-component's `Input` (and other stateful widgets) look the
            // window's first layer up as a `gpui_component::Root`; without it
            // they panic on focus. Root also draws the `window_border` frame.
            let view = cx.new(|cx| app::Gaggle::new(app, logs.clone(), window, cx));
            cx.new(|cx| gpui_component::Root::new(view, window, cx))
        })
        .expect("failed to open window");
        cx.activate(true);
    });
    Ok(())
}
