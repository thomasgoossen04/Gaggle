//! Colour themes.
//!
//! Every widget paints from [`active()`] — a `&'static Palette` kept in a
//! thread-local (the GUI is single-threaded). Reskinning the whole app is one
//! [`activate`] call; adding a theme is one more `static Palette` below plus a
//! branch in [`activate`].

use std::cell::Cell;

use gpui::{Hsla, WindowAppearance};
use gpui_component::ThemeMode;

/// Opaque `Hsla` from degrees / unit saturation / unit lightness. `const` so
/// palettes can live in `static`s.
const fn c(h: f32, s: f32, l: f32) -> Hsla {
    Hsla {
        h: h / 360.0,
        s,
        l,
        a: 1.0,
    }
}

/// One complete look: surfaces, text and status colours. Semantic names (not
/// "amber" / "steel") so a light or a future high-contrast palette drops in
/// without renaming anything at the call sites.
#[derive(Clone, Copy)]
pub struct Palette {
    /// Human label (Settings view, notices).
    //pub name: &'static str,
    /// Window / root background.
    pub bg: Hsla,
    /// Raised surface: header, status bar, cards.
    pub panel: Hsla,
    /// Higher still: resting buttons, inactive tabs, inputs.
    pub panel_hi: Hsla,
    /// Hairline dividers and inert borders.
    pub line: Hsla,
    /// The one high-viz accent.
    pub accent: Hsla,
    /// Dimmed accent for resting borders and the card spine.
    pub accent_dim: Hsla,
    /// Ink that sits *on* an `accent` fill — and the dark half of the hazard rule.
    pub on_accent: Hsla,
    /// Diegetic "terminal" colour for addresses and codes.
    pub info: Hsla,
    /// Body text.
    pub fg: Hsla,
    /// Secondary / label text.
    pub muted: Hsla,
    /// Success / complete.
    pub good: Hsla,
    /// Error / destructive.
    pub bad: Hsla,
    /// Progress-bar trough.
    pub track: Hsla,
}

/// "Super Earth field terminal" — near-black steel, high-viz amber.
pub static DARK: Palette = Palette {
    //name: "Dark",
    bg: c(220.0, 0.12, 0.055),
    panel: c(220.0, 0.10, 0.10),
    panel_hi: c(220.0, 0.09, 0.15),
    line: c(220.0, 0.10, 0.24),
    accent: c(45.0, 1.0, 0.52),
    accent_dim: c(43.0, 0.80, 0.40),
    on_accent: c(220.0, 0.12, 0.055),
    info: c(187.0, 0.85, 0.56),
    fg: c(45.0, 0.10, 0.91),
    muted: c(220.0, 0.07, 0.55),
    good: c(140.0, 0.50, 0.52),
    bad: c(4.0, 0.80, 0.58),
    track: c(220.0, 0.12, 0.14),
};

/// The same character on a printed-briefing white.
pub static LIGHT: Palette = Palette {
    //name: "Light",
    bg: c(45.0, 0.30, 0.95),
    panel: c(44.0, 0.34, 0.90),
    panel_hi: c(44.0, 0.30, 0.84),
    line: c(40.0, 0.22, 0.62),
    accent: c(38.0, 0.92, 0.45),
    accent_dim: c(36.0, 0.62, 0.34),
    on_accent: c(40.0, 0.30, 0.10),
    info: c(196.0, 0.90, 0.30),
    fg: c(40.0, 0.22, 0.14),
    muted: c(40.0, 0.14, 0.33),
    good: c(142.0, 0.55, 0.30),
    bad: c(4.0, 0.70, 0.44),
    track: c(42.0, 0.20, 0.82),
};

thread_local! {
    static ACTIVE: Cell<&'static Palette> = const { Cell::new(&DARK) };
}

/// The palette every widget paints from.
pub fn active() -> &'static Palette {
    ACTIVE.with(|a| a.get())
}

/// Resolve an [`app_state::Theme`] against the OS appearance, make the matching
/// [`Palette`] active, and return the gpui-component mode to keep its own chrome
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
    ACTIVE.with(|a| a.set(if dark { &DARK } else { &LIGHT }));
    if dark {
        ThemeMode::Dark
    } else {
        ThemeMode::Light
    }
}
