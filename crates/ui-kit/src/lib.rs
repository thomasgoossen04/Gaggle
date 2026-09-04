//! `gaggle-ui-kit` — the framework-only slice of the Gaggle desktop look:
//! the colour [`theme`] and the stateless [`widgets`] that paint from it.
//!
//! It depends on `gpui` + `gpui-component` and nothing else, so both the main
//! [`gui`](../gui) and the [`launcher`](../launcher) render identically without
//! either copying the palette.

pub mod fonts;
pub mod theme;
pub mod widgets;
