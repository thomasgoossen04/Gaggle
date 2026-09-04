//! The embedded monospace font.
//!
//! `"monospace"` is a fontconfig generic alias — it resolves on Linux, but
//! there is no font literally named that on Windows or macOS, so every
//! `.font_family("monospace")` call across the app used to fail to resolve a
//! font at all on those platforms. Bundling one font and registering it with
//! gpui's text system at startup guarantees the same "terminal" look (and
//! the same font *existing*) on every platform, regardless of what's
//! installed on the machine.
//!
//! Font: [JetBrains Mono](https://github.com/JetBrains/JetBrainsMono),
//! licensed under the SIL Open Font License 1.1 — see `assets/fonts/OFL.txt`,
//! bundled alongside the font files as the license requires.

use std::borrow::Cow;

use gpui::App;

/// The family name [`install`] registers its embedded font data under. Use
/// this — never the bare string `"monospace"` — for any `.font_family(...)`
/// call that wants the terminal monospace look.
pub const MONO: &str = "JetBrains Mono";

const REGULAR: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf");
const BOLD: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Bold.ttf");

/// Register the embedded monospace font with `cx`'s text system. Call once,
/// early in `Application::new().run(|cx| { ... })`, before any window is
/// created — text laid out before this runs won't pick it up.
pub fn install(cx: &App) {
    if let Err(err) =
        cx.text_system().add_fonts(vec![Cow::Borrowed(REGULAR), Cow::Borrowed(BOLD)])
    {
        // Falls back to whatever the OS resolves for `MONO`'s family name
        // (likely nothing, on Windows/macOS) rather than hard-failing —
        // losing the intended look is better than not starting at all.
        tracing::warn!(error = %err, "could not embed the monospace font");
    }
}
