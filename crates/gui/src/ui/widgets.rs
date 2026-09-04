//! Themed, stateless building blocks. Each reads [`crate::theme::active()`] at
//! call time, so a theme swap re-colours everything on the next render.
//!
//! The framework-only primitives (`card`, `btn`, `hazard_bar`, `win_btn`, …)
//! live in [`gaggle_ui_kit::widgets`] and are re-exported here so existing
//! `crate::ui::widgets::…` imports keep working. Only the ones that need
//! `app-state` or `gpui-component`'s `Input` stay in this file.

use app_state::TransferStatus;
use gpui::prelude::*;
use gpui::{AnyElement, Div, ElementId, Entity, FontWeight, Stateful, div, px};
use gpui_component::Sizable as _;
use gpui_component::input::{Input, InputState};

pub use gaggle_ui_kit::widgets::{
    btn, card, chip, danger_btn, hint, kv, primary_btn, progress_bar, section_title, win_btn,
};

use crate::theme;

/// Tri-state selection mark for the invite file tree.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tri {
    On,
    Off,
    Partial,
}

/// A 15px checkbox rendering a [`Tri`] state.
pub fn checkmark(state: Tri) -> Div {
    let t = theme::active();
    let (bg, border, glyph) = match state {
        Tri::On => (t.accent, t.accent, "✓"),
        Tri::Partial => (t.accent_dim, t.accent_dim, "–"),
        Tri::Off => (t.panel_hi, t.line, ""),
    };
    div()
        .w(px(15.0))
        .h(px(15.0))
        .flex()
        .items_center()
        .justify_center()
        .bg(bg)
        .border_1()
        .border_color(border)
        .text_color(t.on_accent)
        .text_size(px(10.0))
        .child(glyph.to_string())
}

/// A labelled form row wrapping a `gpui-component` text input.
pub fn field(label: &str, state: &Entity<InputState>) -> AnyElement {
    field_row(label, Input::new(state).appearance(true).small())
}

/// [`field`] with a trailing element inside the input (e.g. a "browse" button).
pub fn field_suffixed(
    label: &str,
    state: &Entity<InputState>,
    suffix: impl IntoElement,
) -> AnyElement {
    field_row(label, Input::new(state).appearance(true).small().suffix(suffix))
}

/// A tiny inline affordance rendered inside an input's suffix slot.
pub fn suffix_btn(id: impl Into<ElementId>, label: &str) -> Stateful<Div> {
    let t = theme::active();
    div()
        .id(id)
        .flex()
        .items_center()
        .px_2()
        .h_full()
        .text_xs()
        .font_weight(FontWeight::BOLD)
        .text_color(t.accent)
        .cursor_pointer()
        .hover(|s| s.bg(t.panel))
        .child(label.to_uppercase())
}

fn field_row(label: &str, input: impl IntoElement) -> AnyElement {
    let t = theme::active();
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            div()
                .min_w(px(150.0))
                .text_xs()
                .font_family(theme::MONO)
                .text_color(t.muted)
                .child(label.to_uppercase()),
        )
        .child(div().flex_1().child(input))
        .into_any_element()
}

/// A colour-coded status chip for a transfer row.
pub fn status_pill(status: TransferStatus) -> Div {
    let t = theme::active();
    let color = match status {
        TransferStatus::Complete => t.good,
        TransferStatus::Failed => t.bad,
        TransferStatus::Paused => t.muted,
        _ => t.accent,
    };
    div()
        .px_2()
        .py(px(1.0))
        .border_1()
        .border_color(color)
        .text_xs()
        .font_family(theme::MONO)
        .font_weight(FontWeight::BOLD)
        .text_color(color)
        .child(status.label().to_uppercase())
}
