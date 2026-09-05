//! Colour themes.
//!
//! The palette itself (the `Palette` struct, `DARK` / `LIGHT`, the thread-local
//! [`active()`] every widget paints from) lives in the shared
//! [`gaggle_ui_kit::theme`] crate so the launcher renders identically. This
//! module only adds the [`app_state::Theme`]-aware [`activate`] wrapper.

use gpui::{App, Window, WindowAppearance, px};
use gpui_component::ThemeMode;

pub use gaggle_ui_kit::theme::{MONO, active};

/// Resolve an [`app_state::Theme`] against the OS appearance, make the matching
/// palette active, and return the gpui-component mode to keep its own chrome
/// (the `window_border` frame) in step.
pub fn activate(theme: app_state::Theme, appearance: WindowAppearance) -> ThemeMode {
    let dark = match theme {
        app_state::Theme::Dark => true,
        app_state::Theme::Light => false,
        app_state::Theme::System => matches!(
            appearance,
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        ),
    };
    gaggle_ui_kit::theme::activate(dark)
}

/// Set gpui-component's theme mode, then flatten its border radii to 0 — every
/// other element in this UI (buttons, panels, cards) is hand-drawn square, and
/// `gpui_component::input::Input` is the only widget still borrowed straight
/// from the library, so left alone it's the one rounded-corner outlier.
pub fn apply_mode(mode: ThemeMode, window: Option<&mut Window>, cx: &mut App) {
    gpui_component::Theme::change(mode, window, cx);
    let theme = gpui_component::Theme::global_mut(cx);
    theme.radius = px(0.);
    theme.radius_lg = px(0.);
}
