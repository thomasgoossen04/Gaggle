//! Gaggle desktop GUI — a `gpui` + `gpui-component` shell over the headless
//! [`app_state::App`] transfer manager.
//!
//! Layout:
//! - [`app`] — the single `gpui` view ([`app::Gaggle`]): state snapshot, tab,
//!   actions. Delegates every pixel to [`ui`].
//! - [`ui`] — stateless element builders: [`ui::widgets`] (themed primitives),
//!   [`ui::chrome`] (title bar + status bar), [`ui::views`] (the three tabs).
//! - [`theme`] — swappable colour [`theme::Palette`]s (`DARK`, `LIGHT`); every
//!   widget paints from [`theme::active()`].
//! - [`clipboard`] — Linux clipboard writes that survive the click.
//! - [`util`] — pure formatters.
//!
//! The look is a deliberate "Super Earth field terminal" pastiche — panel
//! surfaces, a single high-viz accent, hazard-stripe rules, targeting brackets
//! on every card, and uppercase monospace chrome.

mod app;
mod clipboard;
mod theme;
mod ui;
mod util;

use std::path::PathBuf;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{Application, WindowBackgroundAppearance, WindowDecorations, WindowOptions};

fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("gaggle").join("settings.json"))
}

fn main() -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let app = runtime.block_on(app_state::App::new(config_path()))?;
    // The manager's background tasks live on this runtime for the whole process.
    std::mem::forget(runtime);
    let app = Arc::new(app);

    Application::new().run(move |cx| {
        gpui_component::init(cx);
        // Seed the palette (and gpui-component's own frame colour) from the
        // persisted setting; `Gaggle::render` keeps it current thereafter.
        let mode = theme::activate(app.snapshot().settings.theme, cx.window_appearance());
        gpui_component::Theme::change(mode, None, cx);

        let app = app.clone();
        let opts = WindowOptions {
            // Drop the server titlebar; we draw our own controls in the header.
            titlebar: None,
            window_decorations: Some(WindowDecorations::Client),
            // Transparent so `window_border`'s drop shadow has somewhere to fall.
            window_background: WindowBackgroundAppearance::Transparent,
            app_id: Some("gaggle".into()),
            ..Default::default()
        };
        cx.open_window(opts, |_, cx| cx.new(|cx| app::Gaggle::new(app, cx)))
            .expect("failed to open window");
        cx.activate(true);
    });
    Ok(())
}
