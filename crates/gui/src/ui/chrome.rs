//! The persistent frame: the custom title bar (wordmark, tabs, window controls,
//! drag-to-move) and the bottom status bar.

use gpui::prelude::*;
use gpui::{
    ClickEvent, Context, FontWeight, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    Window, div, px,
};
use gpui_component::InteractiveElementExt as _;

use crate::app::{Gaggle, Tab};
use crate::theme;
use crate::ui::widgets::{hazard_bar, win_btn};
use crate::util::spaced;

/// The custom title bar + the hazard rule beneath it.
pub fn header(app: &Gaggle, window: &Window, cx: &mut Context<Gaggle>) -> impl IntoElement {
    let t = theme::active();

    let tab_btn = |app: &Gaggle, cx: &mut Context<Gaggle>, tab: Tab, label: &'static str| {
        let active = app.tab == tab;
        div()
            .id(label)
            .px_3()
            .py_1()
            .border_1()
            .border_color(if active { t.accent } else { t.line })
            .bg(if active { t.accent } else { t.panel_hi })
            .text_color(if active { t.on_accent } else { t.fg })
            .text_xs()
            .font_weight(FontWeight::BOLD)
            .cursor_pointer()
            .hover(|s| s.text_color(t.fg).border_color(t.accent_dim))
            .child(label.to_uppercase())
            // Swallow the press so the title bar never reads it as a drag.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.tab = tab;
                cx.notify();
            }))
    };

    let max_glyph = if window.is_maximized() { "❐" } else { "▢" };

    div()
        .flex()
        .flex_col()
        .bg(t.panel)
        .border_b_1()
        .border_color(t.accent_dim)
        .child(
            div()
                .id("titlebar")
                .flex()
                .items_center()
                .justify_between()
                .pl_4()
                .pr_1()
                .py_2()
                // Drag-to-move: arm on press, fire the compositor move on the
                // first drag, so clicks on the tabs / controls still land.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, _| this.dragging = true),
                )
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseUpEvent, _, _| this.dragging = false),
                )
                .on_mouse_move(cx.listener(|this, _: &MouseMoveEvent, window, _| {
                    if std::mem::take(&mut this.dragging) {
                        window.start_window_move();
                    }
                }))
                .on_double_click(cx.listener(|_, _: &ClickEvent, window, _| window.zoom_window()))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(div().text_xs().text_color(t.muted).child("//"))
                                .child(
                                    div()
                                        .font_weight(FontWeight::BLACK)
                                        .text_color(t.accent)
                                        .child(spaced("GAGGLE")),
                                ),
                        )
                        .child(div().w(px(1.0)).h(px(16.0)).bg(t.line))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(tab_btn(app, cx, Tab::Transfers, "Transfers"))
                                .child(tab_btn(app, cx, Tab::Shares, "Shares"))
                                .child(tab_btn(app, cx, Tab::Accelerator, "Accelerator"))
                                .child(tab_btn(app, cx, Tab::Settings, "Settings")),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(win_btn("win-min", "—", false).on_click(cx.listener(
                            |_, _: &ClickEvent, window, _| window.minimize_window(),
                        )))
                        .child(win_btn("win-max", max_glyph, false).on_click(cx.listener(
                            |_, _: &ClickEvent, window, _| window.zoom_window(),
                        )))
                        .child(win_btn("win-close", "✕", true).on_click(cx.listener(
                            |_, _: &ClickEvent, window, _| window.remove_window(),
                        ))),
                ),
        )
        .child(hazard_bar())
}

/// The bottom status line: a notice, or the swarm summary when idle.
pub fn status_bar(app: &Gaggle) -> impl IntoElement {
    let t = theme::active();
    let s = &app.state.swarm;
    let text = app.notice.clone().unwrap_or_else(|| {
        let accel = match &app.state.accelerator {
            Some(a) => format!("  //  ACCEL: {}", a.role.label().to_uppercase()),
            None => String::new(),
        };
        format!("{} SEEDING  //  {} DOWNLOADING{accel}", s.seeding, s.downloading).into()
    });
    div()
        .flex()
        .items_center()
        .gap_2()
        .px_4()
        .py_2()
        .bg(t.panel)
        .border_t_1()
        .border_color(t.line)
        .text_xs()
        .font_family("monospace")
        .child(div().text_color(t.accent).child("▍"))
        .child(
            div()
                .text_color(t.muted)
                .child(text.to_string().to_uppercase()),
        )
}
