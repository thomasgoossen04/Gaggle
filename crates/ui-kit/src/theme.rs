//! Colour themes.
//!
//! Every widget paints from [`active()`] — a `&'static Palette` kept in a
//! thread-local (the UI is single-threaded). Reskinning is one [`activate`]
//! call; adding a theme is one more `static Palette` below plus a branch in the
//! caller that decides `dark`.

use std::cell::Cell;

use gpui::Hsla;
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

/// Make [`DARK`] or [`LIGHT`] the active palette and return the matching
/// gpui-component [`ThemeMode`] so the caller can keep its chrome (the
/// `window_border` frame, inputs) in step via
/// `gpui_component::Theme::change(mode, window, cx)`.
pub fn activate(dark: bool) -> ThemeMode {
    ACTIVE.with(|a| a.set(if dark { &DARK } else { &LIGHT }));
    if dark {
        ThemeMode::Dark
    } else {
        ThemeMode::Light
    }
}
