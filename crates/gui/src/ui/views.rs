//! The tab bodies — Shares, Transfers, Accelerator, Settings — plus their row
//! builders and the expandable detail panels (swarm inspector, invite form).

use app_state::{AcceleratorState, SourceStats, Theme, TransferRow, TransferStatus};
use gpui::prelude::*;
use gpui::{AnyElement, ClickEvent, Context, FontWeight, deferred, div, px, relative};

use crate::app::{Gaggle, Tab};
use crate::theme;
use crate::ui::widgets::{
    btn, card, chip, danger_btn, field, field_suffixed, hint, kv, primary_btn, section_title,
    status_pill, suffix_btn,
};
use crate::util::{cap, human_bytes, human_rate};

/// Local folders this node originates and serves.
pub fn shares(app: &Gaggle, cx: &mut Context<Gaggle>) -> AnyElement {
    let mut col = div().flex().flex_col().gap_2().child(
        div()
            .flex()
            .gap_2()
            .child(
                primary_btn("add-folder", "+ Add folder")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.pick_folder(false, cx))),
            )
            .child(
                btn("add-private", "+ Private folder")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.pick_folder(true, cx))),
            ),
    );

    let seeds: Vec<TransferRow> = app.state.seeds().cloned().collect();
    if seeds.is_empty() {
        col = col.child(hint("NO SHARED FOLDERS — add one to start seeding."));
    }
    for row in seeds {
        col = col.child(share_row(app, &row, cx));
    }
    col.into_any_element()
}

fn share_row(app: &Gaggle, row: &TransferRow, cx: &mut Context<Gaggle>) -> impl IntoElement {
    let t = theme::active();
    let id = row.id;
    let open = app.expanded.contains(&id);
    let addr = row
        .share_addr
        .as_ref()
        .map(|a| a.to_string())
        .unwrap_or_else(|| "coming online…".into());

    let mut c = card()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(expand_caret(id, open, cx))
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(row.name.clone()))
                        .when(row.private, |el| el.child(chip("private", t.info)))
                        .child(chip(&format!("v{}", row.version.max(1)), t.muted)),
                )
                .child(status_pill(row.status)),
        )
        .child(
            div()
                .text_xs()
                .font_family("monospace")
                .text_color(t.muted)
                .child(format!("{} FILES · {}", row.files, human_bytes(row.total_bytes))),
        )
        .child(div().text_xs().font_family("monospace").text_color(t.info).child(addr))
        .child(
            div()
                .flex()
                .gap_2()
                .child(btn(("copy", id as usize), "Copy link").on_click(cx.listener({
                    let row = row.clone();
                    move |this, _: &ClickEvent, _, cx| this.copy_link(&row, cx)
                })))
                .child(
                    btn(("rescan", id as usize), "Rescan").on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| this.rescan(id, cx),
                    )),
                )
                .child(danger_btn(("rm", id as usize), "Remove").on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| {
                        this.app.remove(id);
                        cx.notify();
                    },
                ))),
        );

    if open {
        c = c.child(seed_detail(app, row, cx));
    }
    c
}

/// The expandable panel under a seed row: source folder + (private only) the
/// invite form.
fn seed_detail(app: &Gaggle, row: &TransferRow, cx: &mut Context<Gaggle>) -> AnyElement {
    let t = theme::active();
    let id = row.id;
    let mut panel = div()
        .flex()
        .flex_col()
        .gap_2()
        .mt_1()
        .pt_2()
        .border_t_1()
        .border_color(t.line)
        .child(kv(
            "Source",
            row.source_dir
                .as_ref()
                .map(|d| d.display().to_string())
                .unwrap_or_else(|| "—".into()),
        ));

    if row.private {
        let minted = app
            .state
            .minted_invite
            .as_ref()
            .filter(|m| m.transfer == id)
            .map(|m| m.token.clone());

        panel = panel
            .child(section_title("Invite"))
            .child(
                div()
                    .text_xs()
                    .font_family("monospace")
                    .text_color(t.muted)
                    .child(
                        "PATHS — one per line for a per-file invite; blank = whole folder"
                            .to_string(),
                    ),
            )
            .child(field("Files", &app.invite_paths))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .text_xs()
                            .font_family("monospace")
                            .text_color(t.muted)
                            .child("EXPIRES"),
                    )
                    .child(btn(("exp", id as usize), app.invite_expiry.label()).on_click(
                        cx.listener(|this, _: &ClickEvent, _, cx| this.cycle_expiry(cx)),
                    ))
                    .child(primary_btn(("mint", id as usize), "Create invite").on_click(
                        cx.listener(move |this, _: &ClickEvent, _, cx| this.mint_invite(id, cx)),
                    )),
            );

        if let Some(token) = minted {
            panel = panel
                .child(
                    div()
                        .p_2()
                        .bg(t.panel_hi)
                        .border_1()
                        .border_color(t.line)
                        .text_xs()
                        .font_family("monospace")
                        .text_color(t.info)
                        .child(elide(&token, 64)),
                )
                .child(
                    btn(("copy-inv", id as usize), "Copy invite").on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            let tok = this
                                .state
                                .minted_invite
                                .as_ref()
                                .map(|m| m.token.clone())
                                .unwrap_or_default();
                            this.copy_text(tok, "Invite copied to clipboard", cx);
                        },
                    )),
                );
        }
    }
    panel.into_any_element()
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
        col = col.child(transfer_row(app, &row, cx));
    }
    col.into_any_element()
}

fn transfer_row(app: &Gaggle, row: &TransferRow, cx: &mut Context<Gaggle>) -> impl IntoElement {
    let t = theme::active();
    let id = row.id;
    let open = app.expanded.contains(&id);
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
        "{} / {}  ·  {}  ·  {} SOURCE(S)",
        human_bytes(row.done_bytes),
        human_bytes(row.total_bytes),
        human_rate(row.speed_bps),
        row.sources.len(),
    );

    let can_pause = matches!(
        row.status,
        TransferStatus::Active | TransferStatus::Connecting | TransferStatus::Queued
    );
    let can_resume = row.status == TransferStatus::Paused;

    let mut c = card()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(expand_caret(id, open, cx))
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(row.name.clone()))
                        .child(chip(&format!("v{}", row.version.max(1)), t.muted))
                        .when_some(row.update_available, |el, v| {
                            el.child(chip(&format!("update v{v}"), t.info))
                        }),
                )
                .child(status_pill(row.status)),
        )
        .child(bar)
        .child(div().text_xs().font_family("monospace").text_color(t.muted).child(line))
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
                .when(row.status == TransferStatus::Complete, |el| {
                    el.child(btn(("chk", id as usize), "Check updates").on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| this.check_updates(id, cx),
                    )))
                })
                .when(row.update_available.is_some(), |el| {
                    el.child(primary_btn(("resync", id as usize), "Resync").on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| this.resync(id, cx),
                    )))
                })
                .child(danger_btn(("drm", id as usize), "Remove").on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| {
                        this.app.remove(id);
                        cx.notify();
                    },
                ))),
        );

    if open {
        c = c.child(swarm_inspector(row));
    }
    c
}

/// The per-transfer source breakdown.
fn swarm_inspector(row: &TransferRow) -> AnyElement {
    let t = theme::active();
    let mut panel = div()
        .flex()
        .flex_col()
        .gap_1()
        .mt_1()
        .pt_2()
        .border_t_1()
        .border_color(t.line)
        .child(section_title("Sources"));

    if row.sources.is_empty() {
        panel = panel.child(hint("no source has contributed a chunk yet"));
        return panel.into_any_element();
    }

    let max_bytes = row.sources.iter().map(|s| s.bytes).max().unwrap_or(1).max(1);
    for s in &row.sources {
        panel = panel.child(source_line(s, max_bytes));
    }
    panel.into_any_element()
}

fn source_line(s: &SourceStats, max_bytes: u64) -> impl IntoElement {
    let t = theme::active();
    let frac = (s.bytes as f64 / max_bytes as f64).clamp(0.02, 1.0) as f32;
    div()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .child(
            div()
                .flex()
                .justify_between()
                .text_xs()
                .font_family("monospace")
                .child(div().text_color(t.info).child(short_peer(&s.peer.to_string())))
                .child(
                    div()
                        .text_color(t.muted)
                        .child(format!("{} chunks · {}", s.chunks, human_bytes(s.bytes))),
                ),
        )
        .child(
            div()
                .w_full()
                .h(px(6.0))
                .bg(t.track)
                .child(div().h_full().w(relative(frac)).bg(t.accent_dim)),
        )
}

/// Opt this machine in as an accelerator — the setup wizard.
pub fn accelerator(app: &Gaggle, cx: &mut Context<Gaggle>) -> AnyElement {
    let t = theme::active();
    let mut col = div().flex().flex_col().gap_3();

    // Benchmark card.
    let bench = &app.state.benchmark;
    col = col.child(
        card()
            .child(section_title("Benchmark"))
            .child(
                div()
                    .text_xs()
                    .font_family("monospace")
                    .text_color(t.muted)
                    .child("Measures write speed + free space on the download volume."),
            )
            .when_some(bench.as_ref(), |el, b| {
                el.child(kv("Disk write", format!("{}/s", human_bytes(b.disk_write_bps))))
                    .child(kv("Free space", human_bytes(b.free_bytes)))
                    .child(kv("Suggested role", b.suggested.label().to_string()))
            })
            .child(
                btn("bench", "Run benchmark")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.run_benchmark(cx))),
            ),
    );

    match &app.state.accelerator {
        Some(acc) => col = col.child(accelerator_status(acc, cx)),
        None => col = col.child(accelerator_form(app, cx)),
    }

    col.into_any_element()
}

fn accelerator_form(app: &Gaggle, cx: &mut Context<Gaggle>) -> AnyElement {
    let t = theme::active();
    card()
        .child(section_title("Start an accelerator"))
        .child(
            div()
                .text_xs()
                .font_family("monospace")
                .text_color(t.muted)
                .child(
                    "RELAY: bandwidth-heavy hot-chunk cache + bootstrap.  \
                     NAS: durable full replica of one share."
                        .to_string(),
                ),
        )
        .child(field("Cache MiB", &app.accel_cache))
        .child(field("Share link", &app.accel_link))
        .child(field_suffixed(
            "Replica dir",
            &app.accel_dir,
            suffix_btn("browse-accel-dir", "Browse").on_click(cx.listener(
                |this, _: &ClickEvent, window, cx| this.browse_accel_dir(window, cx),
            )),
        ))
        .child(
            div()
                .flex()
                .gap_2()
                .child(
                    primary_btn("start-relay", "Start relay")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.start_relay(cx))),
                )
                .child(
                    btn("start-nas", "Start NAS replica")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.start_nas(cx))),
                ),
        )
        .into_any_element()
}

fn accelerator_status(acc: &AcceleratorState, cx: &mut Context<Gaggle>) -> AnyElement {
    let t = theme::active();
    let mut c = card()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(section_title("Running"))
                .child(chip(acc.role.label(), t.good)),
        )
        .child(kv("Peer id", short_peer(&acc.peer_id.to_string())))
        .child(kv("Status", acc.detail.clone()));

    for addr in &acc.listen_addrs {
        c = c.child(
            div()
                .text_xs()
                .font_family("monospace")
                .text_color(t.info)
                .child(addr.to_string()),
        );
    }
    if let Some(cache) = &acc.cache {
        let total = (cache.hits + cache.misses).max(1);
        c = c.child(kv(
            "Hot cache",
            format!(
                "{} / {}  ·  {}% hit  ·  {} evicted",
                human_bytes(cache.used_bytes),
                human_bytes(cache.capacity_bytes),
                cache.hits * 100 / total,
                cache.evictions,
            ),
        ));
    }
    if let Some(n) = acc.replica_chunks {
        c = c.child(kv("Replica", format!("{n} chunks on disk")));
    }
    c.child(
        danger_btn("stop-accel", "Stop accelerator")
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.stop_accelerator(cx))),
    )
    .into_any_element()
}

/// The theme selector: a trigger button with a small overlay menu.
fn theme_dropdown(app: &Gaggle, cx: &mut Context<Gaggle>) -> AnyElement {
    let t = theme::active();
    let cur = app.state.settings.theme;
    let open = app.theme_menu_open;

    let trigger = div()
        .id("theme-trigger")
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .py_1()
        .bg(t.panel_hi)
        .border_1()
        .border_color(t.accent_dim)
        .text_xs()
        .font_weight(FontWeight::BOLD)
        .text_color(t.accent)
        .cursor_pointer()
        .hover(|s| s.border_color(t.accent))
        .child(cur.label().to_uppercase())
        .child(div().text_color(t.muted).child(if open { "▲" } else { "▼" }))
        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_theme_menu(cx)));

    let mut wrap = div().relative().child(trigger);
    if open {
        let menu = div()
            .absolute()
            .top_full()
            .right_0()
            .mt_1()
            .min_w(px(140.0))
            .flex()
            .flex_col()
            .bg(t.panel)
            .border_1()
            .border_color(t.accent_dim)
            .children(Theme::ALL.map(|opt| {
                div()
                    .id(("theme-opt", opt as usize))
                    .px_3()
                    .py_1()
                    .text_xs()
                    .font_weight(FontWeight::BOLD)
                    .bg(if opt == cur { t.panel_hi } else { t.panel })
                    .text_color(if opt == cur { t.accent } else { t.fg })
                    .cursor_pointer()
                    .hover(|s| s.bg(t.panel_hi).text_color(t.accent))
                    .child(opt.label().to_uppercase())
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.set_theme(opt, window, cx)
                    }))
            }));
        wrap = wrap.child(deferred(menu));
    }
    wrap.into_any_element()
}

/// Editable settings.
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
                    .child(theme_dropdown(app, cx)),
            ),
        )
        .child(
            card()
                .child(section_title("Downloads & limits"))
                .child(field_suffixed(
                    "Download folder",
                    &app.set_dir,
                    suffix_btn("browse-dir", "Browse").on_click(cx.listener(
                        |this, _: &ClickEvent, window, cx| this.browse_download_dir(window, cx),
                    )),
                ))
                .child(field("Download MiB/s", &app.set_dl))
                .child(field("Upload MiB/s", &app.set_ul))
                .child(field("Cache store GiB", &app.set_store))
                .child(field("Auto-check min", &app.set_resync))
                .child(
                    div().mt_1().flex().child(
                        primary_btn("apply-settings", "Save settings").on_click(cx.listener(
                            |this, _: &ClickEvent, _, cx| this.apply_settings(cx),
                        )),
                    ),
                ),
        )
        .child(
            card()
                .child(section_title("In effect"))
                .child(kv("Folder", s.download_dir.display().to_string()))
                .child(kv("Download cap", cap(s.download_cap_bps)))
                .child(kv("Upload cap", cap(s.upload_cap_bps)))
                .child(kv("Cache storage cap", cap(s.storage_cap_bytes)))
                .child(kv(
                    "Auto update-check",
                    match s.auto_resync_secs {
                        Some(v) => format!("every {} min", v / 60),
                        None => "off".into(),
                    },
                )),
        )
        .child(hint("Blank a limit field to clear it (unlimited / off)."))
        .into_any_element()
}

/// A small ▸ / ▾ toggle for a row's detail panel.
fn expand_caret(id: app_state::TransferId, open: bool, cx: &mut Context<Gaggle>) -> impl IntoElement {
    let t = theme::active();
    div()
        .id(("exp-caret", id as usize))
        .w(px(16.0))
        .text_xs()
        .text_color(t.muted)
        .cursor_pointer()
        .hover(|s| s.text_color(t.accent))
        .child(if open { "▾" } else { "▸" })
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_expand(id, cx)))
}

fn short_peer(id: &str) -> String {
    if id.len() > 20 {
        format!("{}…{}", &id[..10], &id[id.len() - 6..])
    } else {
        id.to_string()
    }
}

fn elide(s: &str, keep: usize) -> String {
    if s.len() > keep {
        format!("{}…", &s[..keep])
    } else {
        s.to_string()
    }
}

/// Dispatch to the body for `tab`.
pub fn body(tab: Tab, app: &Gaggle, cx: &mut Context<Gaggle>) -> AnyElement {
    match tab {
        Tab::Shares => shares(app, cx),
        Tab::Transfers => transfers(app, cx),
        Tab::Accelerator => accelerator(app, cx),
        Tab::Settings => settings(app, cx),
    }
}
