//! The launcher window: a small, fixed-size, decorationless gpui view over
//! [`Updater`] — a Discord-style updater splash — styled from the shared
//! [`gaggle_ui_kit`] theme so it matches the main GUI.

use std::time::Duration;

use gaggle_ui_kit::theme;
use gaggle_ui_kit::widgets::{btn, hazard_bar, primary_btn, progress_bar, win_btn};
use gpui::prelude::*;
use gpui::{
    ClickEvent, Context, FontWeight, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    Timer, Window, WindowAppearance, div, px,
};
use gpui_component::ThemeMode;

use crate::channel::{self, Channel};
use crate::updater::{Status, Updater};

const VERSION: &str = env!("GAGGLE_VERSION");

pub struct Launcher {
    updater: Updater,
    dragging: bool,
    mode: Option<ThemeMode>,
}

impl Launcher {
    pub fn new(updater: Updater, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Kick off the initial check right away.
        updater.check();

        // Re-render on a fixed cadence so the background thread's progress shows.
        cx.spawn(async move |this, cx| {
            loop {
                Timer::after(Duration::from_millis(200)).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        })
        .detach();

        Self {
            updater,
            dragging: false,
            mode: None,
        }
    }

    /// Label + effect of the one primary button for the current status.
    fn primary(&self) -> Option<(&'static str, PrimaryAction)> {
        match self.updater.state() {
            Status::UpToDate { .. } | Status::Ready { .. } => Some(("Launch", PrimaryAction::Launch)),
            Status::UpdateAvailable { .. } => Some(("Update now", PrimaryAction::Install)),
            Status::NotInstalled { .. } => Some(("Install", PrimaryAction::Install)),
            Status::Error(_) => Some(("Retry", PrimaryAction::Check)),
            _ => None, // Checking / Downloading / Verifying / Installing / Launching / Idle
        }
    }

    fn run_primary(&mut self, action: PrimaryAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            PrimaryAction::Launch => {
                if self.updater.launch_now().is_ok() {
                    window.remove_window();
                }
            }
            PrimaryAction::Install => self.updater.install(),
            PrimaryAction::Check => self.updater.check(),
        }
        cx.notify();
    }

    /// Switch release channel: persist the choice, rebuild the updater against
    /// the new channel's URL, and re-check.
    fn set_channel(&mut self, ch: Channel, cx: &mut Context<Self>) {
        if self.updater.channel() == ch {
            return;
        }
        let _ = channel::save(ch);
        self.updater = Updater::for_channel(ch);
        self.updater.check();
        cx.notify();
    }
}

#[derive(Clone, Copy)]
enum PrimaryAction {
    Launch,
    Install,
    Check,
}

/// Progress-bar fill for a status: `None` hides the bar (nothing is running).
fn bar_frac(status: &Status) -> Option<f32> {
    match status {
        Status::Downloading { .. } => Some(status.download_frac().unwrap_or(0.05)),
        Status::Checking => Some(0.1),
        Status::Verifying => Some(0.9),
        Status::Installing => Some(0.97),
        Status::Launching => Some(1.0),
        Status::UpToDate { .. } | Status::Ready { .. } => Some(1.0),
        Status::Idle | Status::NotInstalled { .. } | Status::UpdateAvailable { .. } | Status::Error(_) => None,
    }
}

impl Render for Launcher {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dark = matches!(
            window.appearance(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        );
        let mode = theme::activate(dark);
        if self.mode != Some(mode) {
            self.mode = Some(mode);
            gpui_component::Theme::change(mode, Some(&mut *window), cx);
        }
        let t = theme::active();
        let status = self.updater.state();

        // --- slim drag strip + close ---------------------------------------
        let strip = div()
            .flex()
            .flex_col()
            .bg(t.panel)
            .child(
                div()
                    .id("drag")
                    .flex()
                    .items_center()
                    .justify_between()
                    .pl_3()
                    .pr_1()
                    .py_1()
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
                    .child(
                        div()
                            .text_xs()
                            .font_family("monospace")
                            .text_color(t.muted)
                            .child(match self.updater.channel() {
                                Channel::Beta => "// GAGGLE UPDATER · BETA",
                                Channel::Stable => "// GAGGLE UPDATER",
                            }),
                    )
                    .child(win_btn("win-close", "✕", true).on_click(cx.listener(
                        |_, _: &ClickEvent, window, _| window.remove_window(),
                    ))),
            )
            .child(hazard_bar());

        // --- centred body -------------------------------------------------
        let mut body = div()
            .relative()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .px_6()
            .py_4()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .text_2xl()
                            .font_weight(FontWeight::BLACK)
                            .text_color(t.accent)
                            .child(spaced("GAGGLE")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_family("monospace")
                            .font_weight(FontWeight::BOLD)
                            .text_color(t.muted)
                            .child(spaced("LAUNCHER")),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .text_center()
                    .font_family("monospace")
                    .text_xs()
                    .text_color(t.fg)
                    .child(status.line().to_uppercase()),
            );

        if let Some(frac) = bar_frac(&status) {
            body = body.child(div().w_full().child(progress_bar(frac)));
        }

        let mut buttons = div().flex().items_center().justify_center().gap_2();
        if let Some((label, action)) = self.primary() {
            buttons = buttons.child(primary_btn("primary", label).on_click(cx.listener(
                move |this, _: &ClickEvent, window, cx| this.run_primary(action, window, cx),
            )));
        }
        if matches!(
            status,
            Status::UpToDate { .. }
                | Status::Ready { .. }
                | Status::UpdateAvailable { .. }
                | Status::NotInstalled { .. }
                | Status::Error(_)
        ) {
            buttons = buttons.child(btn("recheck", "Re-check").on_click(cx.listener(
                |this, _: &ClickEvent, _, cx| {
                    this.updater.check();
                    cx.notify();
                },
            )));
        }
        // Channel switch, bottom-left. `active` is passed in so the closure
        // never has to borrow `self`.
        let cur = self.updater.channel();
        let pill = |cx: &mut Context<Self>,
                    id: &'static str,
                    label: &'static str,
                    val: Channel,
                    active: bool| {
            div()
                .id(id)
                .px(px(6.0))
                .py(px(1.0))
                .border_1()
                .border_color(if active { t.accent } else { t.line })
                .bg(if active { t.accent } else { t.panel_hi })
                .text_color(if active { t.on_accent } else { t.muted })
                .font_family("monospace")
                .font_weight(FontWeight::BOLD)
                .text_size(px(9.0))
                .cursor_pointer()
                .child(label)
                .on_click(
                    cx.listener(move |this, _: &ClickEvent, _, cx| this.set_channel(val, cx)),
                )
        };
        let chan_row = div()
            .absolute()
            .bottom_1()
            .left_2()
            .flex()
            .items_center()
            .gap_1()
            .child(
                div()
                    .font_family("monospace")
                    .text_size(px(9.0))
                    .text_color(t.muted)
                    .child("CH"),
            )
            .child(pill(cx, "ch-stable", "STABLE", Channel::Stable, cur == Channel::Stable))
            .child(pill(cx, "ch-beta", "BETA", Channel::Beta, cur == Channel::Beta));

        body = body.child(buttons).child(chan_row).child(
            div()
                .absolute()
                .bottom_1()
                .right_2()
                .text_color(t.muted)
                .font_family("monospace")
                .text_size(px(9.0))
                .child(VERSION),
        );

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(t.bg)
            .text_color(t.fg)
            .child(strip)
            .child(body)
    }
}

/// Wide-track a short label with thin spaces, for the wordmark.
fn spaced(s: &str) -> String {
    s.chars()
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join("\u{2009}")
}
