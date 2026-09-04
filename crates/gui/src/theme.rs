//! Colour themes.
//!
//! The palette itself (the `Palette` struct, `DARK` / `LIGHT`, the thread-local
//! [`active()`] every widget paints from) lives in the shared
//! [`gaggle_ui_kit::theme`] crate so the launcher renders identically. This
//! module only adds the [`app_state::Theme`]-aware [`activate`] wrapper.

use gpui::WindowAppearance;
use gpui_component::ThemeMode;

pub use gaggle_ui_kit::theme::active;

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
