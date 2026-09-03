//! The three tab bodies — Shares, Transfers, Settings — plus their row builders.

use app_state::{TransferRow, TransferStatus};
use gpui::prelude::*;
use gpui::{AnyElement, ClickEvent, Context, FontWeight, div, px, relative};

use crate::app::{Gaggle, Tab};
use crate::theme;
use crate::ui::widgets::{
    btn, card, danger_btn, hint, kv, primary_btn, section_title, status_pill,
};
use crate::util::{cap, human_bytes};

/// Local folders this node originates and serves.
pub fn shares(app: &Gaggle, cx: &mut Context<Gaggle>) -> AnyElement {
    let mut col = div().flex().flex_col().gap_2().child(
        div().flex().child(
            primary_btn("add-folder", "+ Add folder")
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.pick_folder(cx))),
        ),
    );

    let seeds: Vec<TransferRow> = app.state.seeds().cloned().collect();
    if seeds.is_empty() {
        col = col.child(hint("NO SHARED FOLDERS — add one to start seeding."));
    }
    for row in seeds {
        col = col.child(share_row(&row, cx));
    }
    col.into_any_element()
}

fn share_row(row: &TransferRow, cx: &mut Context<Gaggle>) -> impl IntoElement {
    let t = theme::active();
    let id = row.id;
    let addr = row
        .share_addr
        .as_ref()
        .map(|a| a.to_string())
        .unwrap_or_else(|| "coming online…".into());
    card()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(row.name.clone()),
                )
                .child(status_pill(row.status)),
        )
        .child(
            div()
                .text_xs()
                .font_family("monospace")
                .text_color(t.muted)
                .child(format!(
                    "{} FILES · {}",
                    row.files,
                    human_bytes(row.total_bytes)
                )),
        )
        .child(
            div()
                .text_xs()
                .font_family("monospace")
                .text_color(t.info)
                .child(addr),
        )
        .child(
            div()
                .flex()
                .gap_2()
                .child(
                    btn(("copy", id as usize), "Copy link").on_click(cx.listener({
                        let row = row.clone();
                        move |this, _: &ClickEvent, _, cx| this.copy_link(&row, cx)
                    })),
                )
                .child(
                    danger_btn(("rm", id as usize), "Remove").on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.app.remove(id);
                            cx.notify();
                        },
                    )),
                ),
        )
}

/// Remote shares this node is pulling down.
pub fn transfers(app: &Gaggle, cx: &mut Context<Gaggle>) -> AnyElement {
    let mut col = div().flex().flex_col().gap_2().child(
        div().flex().child(
            btn("paste-sub", "Paste subscription link")
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.paste_subscription(cx))),
        ),
    );

    let downloads: Vec<TransferRow> = app.state.downloads().cloned().collect();
    if downloads.is_empty() {
        col = col.child(hint(
            "NO ACTIVE DOWNLOADS — copy a share link on another node, then paste it here.",
        ));
    }
    for row in downloads {
        col = col.child(transfer_row(&row, cx));
    }
    col.into_any_element()
}

fn transfer_row(row: &TransferRow, cx: &mut Context<Gaggle>) -> impl IntoElement {
    let t = theme::active();
    let id = row.id;
    let frac = row.progress().clamp(0.0, 1.0);
    let fill = match row.status {
        TransferStatus::Failed => t.bad,
        TransferStatus::Complete => t.good,
        _ => t.accent,
    };
    let bar = div()
        .w_full()
        .h(px(10.0))
        .bg(t.track)
        .border_1()
        .border_color(t.line)
        .child(div().h_full().w(relative(frac)).bg(fill));

    let line = format!(
        "{} / {}  ·  {}/s  ·  {} SOURCE(S)",
        human_bytes(row.done_bytes),
        human_bytes(row.total_bytes),
        human_bytes(row.speed_bps),
        row.sources.len(),
    );

    let can_pause = matches!(
        row.status,
        TransferStatus::Active | TransferStatus::Connecting | TransferStatus::Queued
    );
    let can_resume = row.status == TransferStatus::Paused;

    card()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(row.name.clone()),
                )
                .child(status_pill(row.status)),
        )
        .child(bar)
        .child(
            div()
                .text_xs()
                .font_family("monospace")
                .text_color(t.muted)
                .child(line),
        )
        .when_some(row.error.clone(), |el, e| {
            el.child(
                div()
                    .text_xs()
                    .font_family("monospace")
                    .text_color(t.bad)
                    .child(format!("!! {e}")),
            )
        })
        .child(
            div()
                .flex()
                .gap_2()
                .when(can_pause, |el| {
                    el.child(btn(("pause", id as usize), "Pause").on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.app.pause(id);
                            cx.notify();
                        },
                    )))
                })
                .when(can_resume, |el| {
                    el.child(btn(("resume", id as usize), "Resume").on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.app.resume(id);
                            cx.notify();
                        },
                    )))
                })
                .child(
                    danger_btn(("drm", id as usize), "Remove").on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.app.remove(id);
                            cx.notify();
                        },
                    )),
                ),
        )
}

/// Read-only settings summary; the theme toggle is the one live control.
pub fn settings(app: &Gaggle, cx: &mut Context<Gaggle>) -> AnyElement {
    let s = &app.state.settings;
    let t = theme::active();
    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            card().child(section_title("Appearance")).child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .font_family("monospace")
                            .text_color(t.muted)
                            .child("THEME"),
                    )
                    .child(btn("theme", s.theme.label()).on_click(cx.listener(
                        |this, _: &ClickEvent, window, cx| this.cycle_theme(window, cx),
                    ))),
            ),
        )
        .child(
            card()
                .child(section_title("Downloads"))
                .child(kv("Folder", s.download_dir.display().to_string()))
                .child(kv("Download cap", cap(s.download_cap_bps)))
                .child(kv("Upload cap", cap(s.upload_cap_bps)))
                .child(kv("Cache storage cap", cap(s.storage_cap_bytes))),
        )
        .child(hint(
            "Bandwidth and storage caps are edited via a config file for now; \
             the GUI form lands in v2.",
        ))
        .into_any_element()
}

/// Dispatch to the body for `tab`.
pub fn body(tab: Tab, app: &Gaggle, cx: &mut Context<Gaggle>) -> AnyElement {
    match tab {
        Tab::Shares => shares(app, cx),
        Tab::Transfers => transfers(app, cx),
        Tab::Settings => settings(app, cx),
    }
}
