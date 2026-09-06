//! Colour themes.
//!
//! The palette itself (the `Palette` struct, `DARK` / `LIGHT`, the thread-local
//! [`active()`] every widget paints from) lives in the shared
//! [`gaggle_ui_kit::theme`] crate so the launcher renders identically. This
//! module only adds the [`app_state::Theme`]-aware [`activate`] wrapper.

use gpui::{App, Hsla, Window, WindowAppearance, px};
use gpui_component::ThemeMode;

pub use gaggle_ui_kit::theme::{MONO, active};

/// Resolve an [`app_state::Theme`] against the OS appearance, make the matching
/// palette active, and return the gpui-component mode to keep its own chrome
/// (the `window_border` frame) in step.
pub fn activate(theme: app_state::Theme, appearance: WindowAppearance) -> ThemeMode {
    use app_state::Theme as T;
    use gaggle_ui_kit::theme as k;

    let os_dark = matches!(
        appearance,
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    );
    let dark = theme.is_dark().unwrap_or(os_dark);

    let palette: &'static k::Palette = match theme {
        T::System => {
            if os_dark {
                &k::DARK
            } else {
                &k::LIGHT
            }
        }
        T::Dark => &k::DARK,
        T::Light => &k::LIGHT,
        T::Dracula => &k::DRACULA,
        T::Nord => &k::NORD,
        T::Gruvbox => &k::GRUVBOX,
        T::TokyoNight => &k::TOKYO_NIGHT,
        T::Catppuccin => &k::CATPPUCCIN,
        T::SolarizedDark => &k::SOLARIZED_DARK,
        T::SolarizedLight => &k::SOLARIZED_LIGHT,
        T::RosePineDawn => &k::ROSE_PINE_DAWN,
    };
    k::set_palette(palette);

    if dark {
        ThemeMode::Dark
    } else {
        ThemeMode::Light
    }
}

/// Set gpui-component's theme mode, flatten its border radii to 0, and repaint
/// the slots the one still-borrowed library widget (`gpui_component::input::Input`)
/// reads from the active [`Palette`](gaggle_ui_kit::theme::Palette).
///
/// `Theme::change` only distinguishes *dark* from *light* and repaints the
/// library's own two palettes — so under Dracula / Nord / Solarized / … the text
/// fields would ignore our colours and render flat black or white. Copying the
/// handful of `ThemeColor` slots `Input` actually uses keeps every field in step
/// with its theme. Must run *after* `Theme::change` (which overwrites
/// `theme.colors` wholesale).
pub fn apply_mode(mode: ThemeMode, window: Option<&mut Window>, cx: &mut App) {
    gpui_component::Theme::change(mode, window, cx);

    let p = gaggle_ui_kit::theme::active();
    let theme = gpui_component::Theme::global_mut(cx);

    // Square corners — every other element in this UI is hand-drawn square.
    theme.radius = px(0.);
    theme.radius_lg = px(0.);

    theme.background = p.bg;
    theme.foreground = p.fg;
    theme.input = p.panel_hi;
    theme.border = p.line;
    theme.muted = p.panel_hi;
    theme.muted_foreground = p.muted;
    theme.secondary = p.panel;
    theme.secondary_hover = p.panel_hi;
    theme.secondary_active = p.panel_hi;
    theme.secondary_foreground = p.fg;
    theme.caret = p.accent;
    theme.selection = Hsla { a: 0.30, ..p.accent };
    theme.ring = p.accent;
    theme.accent = p.panel_hi;
    theme.accent_foreground = p.fg;
    theme.popover = p.panel;
    theme.popover_foreground = p.fg;
    theme.list = p.panel;
    theme.list_hover = p.panel_hi;
    theme.list_active = p.panel_hi;
    theme.list_active_border = p.accent_dim;
    theme.scrollbar = Hsla { a: 0., ..p.panel };
    theme.scrollbar_thumb = p.line;
    theme.scrollbar_thumb_hover = p.muted;
}
