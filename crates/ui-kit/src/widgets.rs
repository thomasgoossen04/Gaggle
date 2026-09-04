//! Themed, stateless building blocks. Each reads [`crate::theme::active()`] at
//! call time, so a theme swap re-colours everything on the next render.

use gpui::prelude::*;
use gpui::{Div, ElementId, FontWeight, MouseButton, Stateful, div, px, relative};

use crate::theme;

/// A card: panel surface, hairline border, an accent spine down the left edge
/// and two targeting brackets at opposite corners.
pub fn card() -> Div {
    let t = theme::active();
    div()
        .relative()
        .flex()
        .flex_col()
        .gap_1()
        .p_3()
        .pl(px(14.0))
        .bg(t.panel)
        .border_1()
        .border_color(t.line)
        .child(
            div()
                .absolute()
                .left_0()
                .top_0()
                .bottom_0()
                .w(px(3.0))
                .bg(t.accent_dim),
        )
        .child(bracket(true, false))
        .child(bracket(false, true))
}

/// One L-shaped corner tick. `top` / `left` pick the corner.
pub fn bracket(top: bool, left: bool) -> Div {
    let t = theme::active();
    let mut d = div().absolute().w(px(7.0)).h(px(7.0)).border_color(t.accent);
    d = if top {
        d.top_0().border_t_2()
    } else {
        d.bottom_0().border_b_2()
    };
    d = if left {
        d.left_0().border_l_2()
    } else {
        d.right_0().border_r_2()
    };
    d
}

/// Hollow high-viz action button (uppercase, sharp; the outline brightens and the
/// surface lifts on hover).
///
/// Note: gpui shapes a plain-string child's colour at *layout* time, before
/// hover state is known, so `.hover(|s| s.text_color(..))` can't recolour the
/// label. Every hover here changes only `bg` / `border`, and the text colour is
/// picked to stay legible against both the resting and hovered surface.
pub fn btn(id: impl Into<ElementId>, label: &str) -> Stateful<Div> {
    let t = theme::active();
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .px_3()
        .py_1()
        .bg(t.panel_hi)
        .border_1()
        .border_color(t.accent_dim)
        .text_xs()
        .font_weight(FontWeight::BOLD)
        .text_color(t.accent)
        .cursor_pointer()
        .hover(|s| s.bg(t.panel).border_color(t.accent))
        .child(label.to_uppercase())
}

/// Filled primary button — the one call to action per view. Same height as
/// [`btn`] / [`danger_btn`] so they line up in a row.
pub fn primary_btn(id: impl Into<ElementId>, label: &str) -> Stateful<Div> {
    let t = theme::active();
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .px_3()
        .py_1()
        .bg(t.accent)
        .border_1()
        .border_color(t.accent)
        .text_xs()
        .font_weight(FontWeight::BLACK)
        .text_color(t.on_accent)
        .cursor_pointer()
        // Keep the accent fill (so `on_accent` text stays readable) and cue the
        // hover with a dark ring instead of darkening the fill.
        .hover(|s| s.border_color(t.on_accent))
        .child(label.to_uppercase())
}

/// Destructive variant of [`btn`] — red outline, red label; surface lifts on
/// hover (see the note on [`btn`] about hovered text colour).
pub fn danger_btn(id: impl Into<ElementId>, label: &str) -> Stateful<Div> {
    let t = theme::active();
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .px_3()
        .py_1()
        .bg(t.panel_hi)
        .border_1()
        .border_color(t.bad)
        .text_xs()
        .font_weight(FontWeight::BOLD)
        .text_color(t.bad)
        .cursor_pointer()
        .hover(|s| s.bg(t.panel))
        .child(label.to_uppercase())
}

/// A title-bar window control (minimize / maximize / close). Square, muted, and
/// lights up on hover — accent for the benign two, red for close.
pub fn win_btn(id: impl Into<ElementId>, glyph: &str, danger: bool) -> Stateful<Div> {
    let t = theme::active();
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .w(px(28.0))
        .h(px(22.0))
        .text_xs()
        // `danger` gets `fg` at rest so the glyph stays legible on the red hover
        // fill (the label colour can't change on hover — see [`btn`]).
        .text_color(if danger { t.fg } else { t.muted })
        .cursor_pointer()
        .hover(|s| s.bg(if danger { t.bad } else { t.panel_hi }))
        // Swallow the press so the title bar never reads it as a drag.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(glyph.to_string())
}

/// Caution-tape rule: a clipped run of alternating accent / ink blocks.
pub fn hazard_bar() -> impl IntoElement {
    let t = theme::active();
    div()
        .flex()
        .h(px(6.0))
        .w_full()
        .overflow_hidden()
        .children((0..80).map(move |i| {
            div()
                .w(px(16.0))
                .h_full()
                .bg(if i % 2 == 0 { t.accent } else { t.on_accent })
        }))
}

/// A stencilled section header for a card.
pub fn section_title(text: &str) -> Div {
    let t = theme::active();
    div()
        .font_family("monospace")
        .font_weight(FontWeight::BOLD)
        .text_color(t.fg)
        .child(format!("▮ {}", text.to_uppercase()))
}

/// A muted, `//`-prefixed empty-state / footnote line.
pub fn hint(text: &str) -> Div {
    let t = theme::active();
    div()
        .p_3()
        .text_xs()
        .font_family("monospace")
        .text_color(t.muted)
        .child(format!("// {text}"))
}

/// A key / value row in monospace.
pub fn kv(key: &str, value: String) -> Div {
    let t = theme::active();
    div()
        .flex()
        .justify_between()
        .gap_3()
        .text_xs()
        .font_family("monospace")
        .child(div().text_color(t.muted).child(key.to_uppercase()))
        .child(div().text_color(t.fg).child(value))
}

/// A small outlined chip — used for the "UPDATE" badge and role tags.
pub fn chip(text: &str, color: gpui::Hsla) -> Div {
    div()
        .px_2()
        .py(px(1.0))
        .border_1()
        .border_color(color)
        .text_xs()
        .font_family("monospace")
        .font_weight(FontWeight::BOLD)
        .text_color(color)
        .child(text.to_uppercase())
}

/// A framed progress bar: hairline-bordered trough with an accent fill.
/// `frac` is clamped to `0.0..=1.0`.
pub fn progress_bar(frac: f32) -> impl IntoElement {
    let t = theme::active();
    div()
        .w_full()
        .h(px(10.0))
        .bg(t.track)
        .border_1()
        .border_color(t.line)
        .child(
            div()
                .h_full()
                .w(relative(frac.clamp(0.0, 1.0)))
                .bg(t.accent),
        )
}
