//! Colour themes.
//!
//! Every widget paints from [`active()`] — a `&'static Palette` kept in a
//! thread-local (the UI is single-threaded). Reskinning is one [`activate`]
//! call; adding a theme is one more `static Palette` below plus a branch in the
//! caller that decides `dark`.

use std::cell::Cell;

use gpui::Hsla;
use gpui_component::ThemeMode;

pub use crate::fonts::MONO;

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

const fn fmax(a: f32, b: f32) -> f32 {
    if a > b { a } else { b }
}
const fn fmin(a: f32, b: f32) -> f32 {
    if a < b { a } else { b }
}

/// Opaque `Hsla` from a `0xRRGGBB` literal — so a palette lifted from a
/// published scheme can be transcribed as its documented hex codes instead of
/// hand-converted HSL. `const`, so these still live in `static`s.
const fn rgb(v: u32) -> Hsla {
    let r = ((v >> 16) & 0xff) as f32 / 255.0;
    let g = ((v >> 8) & 0xff) as f32 / 255.0;
    let b = (v & 0xff) as f32 / 255.0;
    let max = fmax(fmax(r, g), b);
    let min = fmin(fmin(r, g), b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d == 0.0 {
        return Hsla { h: 0.0, s: 0.0, l, a: 1.0 };
    }
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let mut hp = if max == r {
        (g - b) / d
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    if hp < 0.0 {
        hp += 6.0;
    }
    Hsla {
        h: hp * 60.0 / 360.0,
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

// --- Popular published schemes -------------------------------------------------
//
// Each is transcribed from the scheme's own documented hex codes and mapped onto
// the semantic slots above: `bg` < `panel` < `panel_hi` in "raisedness",
// `accent` the signature colour, `info` the diegetic cyan for codes/addresses.

/// [Dracula](https://draculatheme.com) — purple on near-black.
pub static DRACULA: Palette = Palette {
    bg: rgb(0x282a36),
    panel: rgb(0x2d2f3d),
    panel_hi: rgb(0x44475a),
    line: rgb(0x3b3d4d),
    accent: rgb(0xbd93f9),
    accent_dim: rgb(0x8a6fd1),
    on_accent: rgb(0x282a36),
    info: rgb(0x8be9fd),
    fg: rgb(0xf8f8f2),
    muted: rgb(0x6272a4),
    good: rgb(0x50fa7b),
    bad: rgb(0xff5555),
    track: rgb(0x21222c),
};

/// [Nord](https://www.nordtheme.com) — arctic, frost-blue accent.
pub static NORD: Palette = Palette {
    bg: rgb(0x2e3440),
    panel: rgb(0x3b4252),
    panel_hi: rgb(0x434c5e),
    line: rgb(0x4c566a),
    accent: rgb(0x88c0d0),
    accent_dim: rgb(0x5e81ac),
    on_accent: rgb(0x2e3440),
    info: rgb(0x8fbcbb),
    fg: rgb(0xeceff4),
    muted: rgb(0x8b98b0),
    good: rgb(0xa3be8c),
    bad: rgb(0xbf616a),
    track: rgb(0x272b34),
};

/// [Gruvbox](https://github.com/morhetz/gruvbox) dark — retro warm, yellow accent.
pub static GRUVBOX: Palette = Palette {
    bg: rgb(0x282828),
    panel: rgb(0x3c3836),
    panel_hi: rgb(0x504945),
    line: rgb(0x665c54),
    accent: rgb(0xfabd2f),
    accent_dim: rgb(0xd79921),
    on_accent: rgb(0x282828),
    info: rgb(0x8ec07c),
    fg: rgb(0xebdbb2),
    muted: rgb(0x928374),
    good: rgb(0xb8bb26),
    bad: rgb(0xfb4934),
    track: rgb(0x1d2021),
};

/// [Tokyo Night](https://github.com/enkia/tokyo-night-vscode-theme) — deep navy, blue accent.
pub static TOKYO_NIGHT: Palette = Palette {
    bg: rgb(0x1a1b26),
    panel: rgb(0x1f2335),
    panel_hi: rgb(0x292e42),
    line: rgb(0x3b4261),
    accent: rgb(0x7aa2f7),
    accent_dim: rgb(0x3d59a1),
    on_accent: rgb(0x1a1b26),
    info: rgb(0x7dcfff),
    fg: rgb(0xc0caf5),
    muted: rgb(0x565f89),
    good: rgb(0x9ece6a),
    bad: rgb(0xf7768e),
    track: rgb(0x16161e),
};

/// [Catppuccin](https://catppuccin.com) Mocha — soft pastels, mauve accent.
pub static CATPPUCCIN: Palette = Palette {
    bg: rgb(0x1e1e2e),
    panel: rgb(0x313244),
    panel_hi: rgb(0x45475a),
    line: rgb(0x585b70),
    accent: rgb(0xcba6f7),
    accent_dim: rgb(0x9d7cd8),
    on_accent: rgb(0x1e1e2e),
    info: rgb(0x89dceb),
    fg: rgb(0xcdd6f4),
    muted: rgb(0x7f849c),
    good: rgb(0xa6e3a1),
    bad: rgb(0xf38ba8),
    track: rgb(0x181825),
};

/// [Solarized](https://ethanschoonover.com/solarized) dark.
pub static SOLARIZED_DARK: Palette = Palette {
    bg: rgb(0x002b36),
    panel: rgb(0x073642),
    panel_hi: rgb(0x0a4b5c),
    line: rgb(0x586e75),
    accent: rgb(0x268bd2),
    accent_dim: rgb(0x1f6f9e),
    on_accent: rgb(0x002b36),
    info: rgb(0x2aa198),
    fg: rgb(0x93a1a1),
    muted: rgb(0x657b83),
    good: rgb(0x859900),
    bad: rgb(0xdc322f),
    track: rgb(0x00232e),
};

/// [Solarized](https://ethanschoonover.com/solarized) light.
pub static SOLARIZED_LIGHT: Palette = Palette {
    bg: rgb(0xfdf6e3),
    panel: rgb(0xeee8d5),
    panel_hi: rgb(0xe2dcc6),
    line: rgb(0x93a1a1),
    accent: rgb(0x268bd2),
    accent_dim: rgb(0x1f6f9e),
    on_accent: rgb(0xfdf6e3),
    info: rgb(0x0f6e66),
    fg: rgb(0x586e75),
    muted: rgb(0x93a1a1),
    good: rgb(0x647900),
    bad: rgb(0xdc322f),
    track: rgb(0xe2dcc6),
};

/// [Rosé Pine](https://rosepinetheme.com) Dawn — the light variant, iris accent.
pub static ROSE_PINE_DAWN: Palette = Palette {
    bg: rgb(0xfaf4ed),
    panel: rgb(0xf2e9e1),
    panel_hi: rgb(0xdfdad9),
    line: rgb(0xcecacd),
    accent: rgb(0x907aa9),
    accent_dim: rgb(0x6e5a86),
    on_accent: rgb(0xfaf4ed),
    info: rgb(0x56949f),
    fg: rgb(0x575279),
    muted: rgb(0x797593),
    good: rgb(0x286983),
    bad: rgb(0xb4637a),
    track: rgb(0xdfdad9),
};

thread_local! {
    static ACTIVE: Cell<&'static Palette> = const { Cell::new(&DARK) };
}

/// The palette every widget paints from.
pub fn active() -> &'static Palette {
    ACTIVE.with(|a| a.get())
}

/// Make an arbitrary palette active. The caller (`gui`) owns the mapping from
/// its `app_state::Theme` to one of the `static Palette`s in this module and is
/// responsible for passing the matching [`ThemeMode`] to
/// `gpui_component::Theme::change` so the library chrome stays in step.
pub fn set_palette(p: &'static Palette) {
    ACTIVE.with(|a| a.set(p));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) {
        assert!((a - b).abs() < 0.01, "{a} !~ {b}");
    }

    #[test]
    fn rgb_matches_known_hsl() {
        let black = rgb(0x000000);
        approx(black.l, 0.0);
        approx(black.s, 0.0);

        let white = rgb(0xffffff);
        approx(white.l, 1.0);
        approx(white.s, 0.0);

        // Pure red: h 0°, full saturation, mid lightness.
        let red = rgb(0xff0000);
        approx(red.h, 0.0);
        approx(red.s, 1.0);
        approx(red.l, 0.5);

        // Pure green sits at 120° = 1/3 of the wheel.
        approx(rgb(0x00ff00).h, 1.0 / 3.0);
        // Pure blue at 240° = 2/3.
        approx(rgb(0x0000ff).h, 2.0 / 3.0);

        // A mid grey: no hue, no saturation, ~half lightness.
        let grey = rgb(0x808080);
        approx(grey.s, 0.0);
        approx(grey.l, 0.502);
    }

    #[test]
    fn every_palette_is_opaque_and_layered() {
        // `bg` is the resting surface; `panel` / `panel_hi` are raised above it.
        // For a dark scheme they get lighter, for a light scheme darker — either
        // way `bg` and `panel_hi` must not be the same flat colour.
        for p in [
            &DARK,
            &LIGHT,
            &DRACULA,
            &NORD,
            &GRUVBOX,
            &TOKYO_NIGHT,
            &CATPPUCCIN,
            &SOLARIZED_DARK,
            &SOLARIZED_LIGHT,
            &ROSE_PINE_DAWN,
        ] {
            for ch in [p.bg, p.panel, p.accent, p.fg, p.good, p.bad] {
                approx(ch.a, 1.0);
            }
            assert!(
                (p.bg.l - p.panel_hi.l).abs() > 0.01,
                "bg and panel_hi collapsed to one lightness"
            );
        }
    }
}
