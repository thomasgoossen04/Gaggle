//! The view layer: pure `gpui` element builders over a [`crate::app::Gaggle`]
//! snapshot. Nothing here owns state — `chrome` and `views` read the app handle
//! and wire listeners; `widgets` are themed, stateless building blocks.

pub mod chrome;
pub mod views;
pub mod widgets;
