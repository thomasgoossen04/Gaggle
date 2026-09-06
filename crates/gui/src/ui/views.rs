//! The tab bodies — Shares, Transfers, Accelerator, Settings — plus their row
//! builders and the expandable detail panels (swarm inspector, invite form).

use std::time::{Duration, SystemTime};

use app_state::{
    AccelShareRow, AcceleratorRole, AcceleratorState, LogLevel, RemoteAccelState, SourceStats,
    SpeedSample, Theme, TransferRow, TransferStatus,
};
use gpui::prelude::*;
use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, Hsla, KeyDownEvent, MouseButton, SharedString,
    deferred, div, hsla, px, relative, uniform_list,
};
use gpui_component::chart::LineChart;

use crate::app::{ConfirmKind, Gaggle, StatsSource, Tab};
use crate::theme;
use crate::ui::widgets::{
    Tri, btn, card, checkmark, chip, danger_btn, field, field_suffixed, hint, kv, primary_btn,
    progress_bar, section_title, status_pill, suffix_btn,
};
use crate::util::{cap, fmt_log_time, human_bytes, human_rate};

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
        .when(row.status == TransferStatus::Scanning, |el| {
            el.child(progress_bar(row.progress()))
        })
        .child(
            div()
                .text_xs()
                .font_family(theme::MONO)
                .text_color(t.muted)
                .child(if row.status == TransferStatus::Scanning {
                    format!(
                        "SCANNING… {} / {} · {} FILES",
                        human_bytes(row.done_bytes),
                        human_bytes(row.total_bytes),
                        row.files
                    )
                } else {
                    format!("{} FILES · {}", row.files, human_bytes(row.total_bytes))
                }),
        )
        .child(div().text_xs().font_family(theme::MONO).text_color(t.info).child(addr))
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
                    move |this, _: &ClickEvent, window, cx| this.ask_remove(id, window, cx),
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
                    .font_family(theme::MONO)
                    .text_color(t.muted)
                    .child("Tick the files & folders this invite may access."),
            )
            .child(invite_tree(app, row, cx))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .text_xs()
                            .font_family(theme::MONO)
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
                        .font_family(theme::MONO)
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

/// The invite's file/folder picker: a collapsible tree of checkboxes over the
/// seed's manifest paths.
fn invite_tree(app: &Gaggle, row: &TransferRow, cx: &mut Context<Gaggle>) -> AnyElement {
    let t = theme::active();
    if row.file_paths.is_empty() {
        return hint("reading files…").into_any_element();
    }
    // Very large trees would make the 200 ms redraw sluggish — offer whole-folder
    // only past a sane cap.
    if row.file_paths.len() > 4000 {
        return hint(&format!(
            "{} files — this invite covers the whole folder",
            row.file_paths.len()
        ))
        .into_any_element();
    }
    div()
        .id(SharedString::from(format!("invite-tree-{}", row.id)))
        .max_h(px(240.0))
        .overflow_y_scroll()
        .p_1()
        .border_1()
        .border_color(t.line)
        .flex()
        .flex_col()
        .children(tree_rows(&row.file_paths, "", 0, app, cx))
        .into_any_element()
}

/// Rows for the children of `prefix` (`""` = root), recursing into expanded
/// folders. `files` is the sorted manifest path list.
fn tree_rows(
    files: &[String],
    prefix: &str,
    depth: usize,
    app: &Gaggle,
    cx: &mut Context<Gaggle>,
) -> Vec<AnyElement> {
    let t = theme::active();
    let indent = px(depth as f32 * 14.0);

    let mut dirs: Vec<String> = Vec::new();
    let mut here: Vec<&String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for p in files {
        let Some(rest) = p.strip_prefix(prefix) else { continue };
        if rest.is_empty() {
            continue;
        }
        match rest.split_once('/') {
            Some((seg, _)) => {
                let full = format!("{prefix}{seg}");
                if seen.insert(full.clone()) {
                    dirs.push(full);
                }
            }
            None => here.push(p),
        }
    }

    let mut out: Vec<AnyElement> = Vec::new();

    for dir in dirs {
        let child_prefix = format!("{dir}/");
        let (sel, total) = files
            .iter()
            .filter(|p| p.starts_with(&child_prefix))
            .fold((0usize, 0usize), |(s, n), p| {
                (s + usize::from(app.invite_sel.contains(p)), n + 1)
            });
        let state = if total == 0 || sel == 0 {
            Tri::Off
        } else if sel == total {
            Tri::On
        } else {
            Tri::Partial
        };
        let open = app.tree_expanded.contains(&dir);
        let name = dir.rsplit('/').next().unwrap_or(&dir).to_string();

        out.push(
            div()
                .flex()
                .items_center()
                .gap_2()
                .py(px(1.0))
                .pl(indent)
                .child(
                    div()
                        .id(SharedString::from(format!("tc:{dir}")))
                        .w(px(12.0))
                        .text_xs()
                        .text_color(t.muted)
                        .cursor_pointer()
                        .child(if open { "▾" } else { "▸" })
                        .on_click(cx.listener({
                            let d = dir.clone();
                            move |this, _: &ClickEvent, _, cx| this.toggle_tree_dir(d.clone(), cx)
                        })),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("tb:{dir}")))
                        .cursor_pointer()
                        .child(checkmark(state))
                        .on_click(cx.listener({
                            let d = dir.clone();
                            move |this, _: &ClickEvent, _, cx| this.toggle_invite_dir(d.clone(), cx)
                        })),
                )
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(t.fg)
                        .child(format!("{name}/")),
                )
                .into_any_element(),
        );

        if open {
            out.extend(tree_rows(files, &child_prefix, depth + 1, app, cx));
        }
    }

    for f in here {
        let on = app.invite_sel.contains(f);
        let name = f.rsplit('/').next().unwrap_or(f).to_string();
        out.push(
            div()
                .flex()
                .items_center()
                .gap_2()
                .py(px(1.0))
                .pl(indent)
                .child(div().w(px(12.0)))
                .child(
                    div()
                        .id(SharedString::from(format!("tf:{f}")))
                        .cursor_pointer()
                        .child(checkmark(if on { Tri::On } else { Tri::Off }))
                        .on_click(cx.listener({
                            let p = f.clone();
                            move |this, _: &ClickEvent, _, cx| this.toggle_invite_file(p.clone(), cx)
                        })),
                )
                .child(
                    div()
                        .text_xs()
                        .font_family(theme::MONO)
                        .text_color(if on { t.fg } else { t.muted })
                        .child(name),
                )
                .into_any_element(),
        );
    }

    out
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
    let can_retry = row.status == TransferStatus::Failed;

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
        .when_some(
            (row.status == TransferStatus::Connecting)
                .then(|| row.detail.clone())
                .flatten(),
            |el, detail| {
                el.child(
                    div()
                        .text_xs()
                        .font_family(theme::MONO)
                        .text_color(t.muted)
                        .child(format!("… {detail}")),
                )
            },
        )
        .child(bar)
        .child(div().text_xs().font_family(theme::MONO).text_color(t.muted).child(line))
        .when_some(row.error.clone(), |el, e| {
            el.child(
                div()
                    .text_xs()
                    .font_family(theme::MONO)
                    .text_color(t.bad)
                    .child(format!("!! {e}")),
            )
        })
        .child(
            div()
                .flex()
                .flex_wrap()
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
                .when(can_retry, |el| {
                    el.child(primary_btn(("retry", id as usize), "Retry").on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.app.retry(id);
                            cx.notify();
                        },
                    )))
                })
                .when(row.output_dir.is_some(), |el| {
                    el.child(btn(("open", id as usize), "Open folder").on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| this.open_output_dir(id, cx),
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
                    move |this, _: &ClickEvent, window, cx| this.ask_remove(id, window, cx),
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
                .font_family(theme::MONO)
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

/// Opt this machine in as an accelerator (local), and manage remote ones.
pub fn accelerator(app: &Gaggle, cx: &mut Context<Gaggle>) -> AnyElement {
    let t = theme::active();
    let mut col = div().flex().flex_col().gap_3();

    // Operator key card.
    col = col.child(
        card()
            .child(section_title("Your operator key"))
            .child(
                div()
                    .text_xs()
                    .font_family(theme::MONO)
                    .text_color(t.muted)
                    .child("Authorise it on a daemon:  accelerator authorize <key>"),
            )
            .child(
                div()
                    .p_2()
                    .bg(t.panel_hi)
                    .border_1()
                    .border_color(t.line)
                    .text_xs()
                    .font_family(theme::MONO)
                    .text_color(t.info)
                    .child(app.state.operator_key.clone()),
            )
            .child(
                btn("copy-op-key", "Copy operator key").on_click(
                    cx.listener(|this, _: &ClickEvent, _, cx| this.copy_operator_key(cx)),
                ),
            ),
    );

    // Benchmark card.
    let bench = &app.state.benchmark;
    col = col.child(
        card()
            .child(section_title("Benchmark"))
            .child(
                div()
                    .text_xs()
                    .font_family(theme::MONO)
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

    // Local accelerator: form or running status.
    col = match &app.state.accelerator {
        Some(acc) => col.child(accelerator_status(app, acc, cx)),
        None => col.child(accelerator_form(app, cx)),
    };

    // Remote accelerators.
    col = col.child(remote_accelerators(app, cx));

    col.into_any_element()
}

fn accelerator_form(app: &Gaggle, cx: &mut Context<Gaggle>) -> AnyElement {
    let t = theme::active();
    card()
        .child(section_title("Start a local accelerator"))
        .child(
            div()
                .text_xs()
                .font_family(theme::MONO)
                .text_color(t.muted)
                .child(
                    "RELAY: hot-chunk cache + bootstrap.  NAS: durable on-disk replicas.  \
                     One link per line — an accelerator carries any number of shares."
                        .to_string(),
                ),
        )
        .child(field("Cache MiB", &app.accel_cache))
        .child(field("Share link(s)", &app.accel_link))
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

/// One "share carried by an accelerator" row: a seed pause/resume toggle (local
/// NAS only), its on-disk footprint + path, and a Remove button that confirms
/// first (removing a NAS share deletes its replica from disk).
fn accel_share_row(
    prefix: &str,
    s: &AccelShareRow,
    is_local_nas: bool,
    remote_label: Option<String>,
    cx: &mut Context<Gaggle>,
) -> impl IntoElement {
    let t = theme::active();
    let key = format!("{prefix}-{}", s.manifest_id);
    let mut meta = if let Some(e) = &s.error {
        format!("!! {e}")
    } else if let Some(p) = &s.replicating {
        if p.chunks_total == 0 {
            "replicating — fetching metadata…".to_string()
        } else {
            format!(
                "replicating {}/{} chunks ({} of {})",
                p.chunks_done,
                p.chunks_total,
                human_bytes(p.bytes_done),
                human_bytes(p.bytes_total)
            )
        }
    } else if let Some(n) = s.replica_chunks {
        format!("{} files · {} · {n} chunks", s.files, human_bytes(s.total_bytes))
    } else {
        format!("{} files · {}", s.files, human_bytes(s.total_bytes))
    };
    if let Some(b) = s.disk_bytes {
        meta.push_str(&format!(" · {} on disk", human_bytes(b)));
    }

    let on_disk = is_local_nas || s.replica_chunks.is_some() || s.disk_bytes.is_some();
    let mid = s.manifest_id.clone();
    let name = s.name.clone();
    let seeding = s.seeding;
    // A pause/resume toggle applies to a local NAS share and to any share
    // carried by a remote daemon (the daemon keeps the replica / cache entry
    // and its token, it just stops serving it). A local *relay* share has no
    // pause verb.
    let show_toggle = is_local_nas || remote_label.is_some();
    let dimmed = show_toggle && !seeding;

    let mut info = div().flex().flex_col().flex_1().min_w_0().child(
        div()
            .flex()
            .items_center()
            .flex_wrap()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(if dimmed { t.muted } else { t.fg })
                    .child(s.name.clone()),
            )
            .when(s.private, |el| el.child(chip("private", t.info))),
    );
    info = info.child(
        div()
            .w_full()
            .truncate()
            .text_xs()
            .font_family(theme::MONO)
            .text_color(if s.error.is_some() { t.bad } else { t.muted })
            .child(meta),
    );
    if let Some(path) = &s.replica_path {
        info = info.child(
            div()
                .w_full()
                .truncate()
                .text_xs()
                .font_family(theme::MONO)
                .text_color(t.muted)
                .child(path.display().to_string()),
        );
    }

    // Right-hand action cluster: never shrinks, so it can't be pushed off the
    // card's edge (and clipped by the scroll container) by wide meta text.
    let mut actions = div().flex().items_center().gap_2().flex_shrink_0();
    if show_toggle {
        let toggle_mid = mid.clone();
        let toggle_remote = remote_label.clone();
        actions = actions.child(
            div()
                .id((SharedString::from(format!("{key}-seed")), 0))
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .border_1()
                .border_color(if seeding { t.accent_dim } else { t.line })
                .bg(t.panel_hi)
                .cursor_pointer()
                .hover(|s| s.border_color(t.accent))
                .child(checkmark(if seeding { Tri::On } else { Tri::Off }))
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::BOLD)
                        .text_color(t.muted)
                        .child(if seeding { "SEEDING" } else { "PAUSED" }),
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    match &toggle_remote {
                        Some(label) => this.remote_set_share_seeding(
                            label.clone(),
                            toggle_mid.clone(),
                            !seeding,
                            cx,
                        ),
                        None => this.accel_set_seeding(toggle_mid.clone(), !seeding, cx),
                    }
                })),
        );
    }
    actions = actions.child(danger_btn((SharedString::from(key), 1), "Remove").on_click(
        cx.listener(move |this, _: &ClickEvent, window, cx| {
            this.ask_remove_accel_share(
                mid.clone(),
                name.clone(),
                on_disk,
                remote_label.clone(),
                window,
                cx,
            )
        }),
    ));

    div()
        .flex()
        .items_start()
        .justify_between()
        .gap_3()
        .py(px(4.0))
        .child(info)
        .child(actions)
}

fn accelerator_status(app: &Gaggle, acc: &AcceleratorState, cx: &mut Context<Gaggle>) -> AnyElement {
    let t = theme::active();
    let mut c = card()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(section_title("Local accelerator"))
                .child(chip(acc.role.label(), t.good)),
        )
        .child(kv("Peer id", short_peer(&acc.peer_id.to_string())))
        .child(kv("Status", acc.detail.clone()));

    for addr in &acc.listen_addrs {
        c = c.child(
            div()
                .text_xs()
                .font_family(theme::MONO)
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

    c = c.child(section_title("Shares"));
    if acc.shares.is_empty() {
        c = c.child(hint("no shares — add one below, or run as a plain relay"));
    }
    let is_nas = acc.role == AcceleratorRole::Nas;
    for s in &acc.shares {
        c = c.child(accel_share_row("acc-share", s, is_nas, None, cx));
    }
    c = c
        .child(field("Add share link", &app.accel_add))
        .child(
            btn("accel-add-share", "Add share").on_click(
                cx.listener(|this, _: &ClickEvent, _, cx| this.accel_add_share(cx)),
            ),
        );

    c.child(
        danger_btn("stop-accel", "Stop accelerator")
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.stop_accelerator(cx))),
    )
    .into_any_element()
}

fn remote_accelerators(app: &Gaggle, cx: &mut Context<Gaggle>) -> AnyElement {
    let t = theme::active();
    let mut c = card().child(section_title("Remote accelerators"));

    if app.state.remote_accelerators.is_empty() {
        c = c.child(hint("none registered — add one by its admin URL below"));
    }
    for r in &app.state.remote_accelerators {
        c = c.child(remote_row(app, r, cx));
    }

    c.child(
        div()
            .mt_1()
            .pt_2()
            .border_t_1()
            .border_color(t.line)
            .flex()
            .flex_col()
            .gap_2()
            .child(field("Label", &app.remote_label))
            .child(field("Admin URL", &app.remote_url))
            .child(
                primary_btn("add-remote", "Add remote accelerator").on_click(
                    cx.listener(|this, _: &ClickEvent, _, cx| this.add_remote(cx)),
                ),
            ),
    )
    .into_any_element()
}

fn remote_row(app: &Gaggle, r: &RemoteAccelState, cx: &mut Context<Gaggle>) -> AnyElement {
    let t = theme::active();
    let label = r.label.clone();
    let dot = if r.reachable { t.good } else { t.bad };

    let mut panel = div()
        .flex()
        .flex_col()
        .gap_1()
        .py_2()
        .border_t_1()
        .border_color(t.line)
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
                        .child(div().w(px(8.0)).h(px(8.0)).bg(dot))
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(r.label.clone()))
                        .when_some(r.role, |el, role| el.child(chip(role.label(), t.muted))),
                )
                .child(danger_btn((SharedString::from(format!("rm-remote-{label}")), 0), "Forget").on_click(
                    cx.listener({
                        let label = label.clone();
                        move |this, _: &ClickEvent, _, cx| this.remove_remote(label.clone(), cx)
                    }),
                )),
        )
        .child(
            div()
                .text_xs()
                .font_family(theme::MONO)
                .text_color(t.muted)
                .child(r.admin_url.clone()),
        );

    if let Some(pid) = &r.peer_id {
        panel = panel.child(kv("Peer id", short_peer(pid)));
    }
    if let Some(err) = &r.error {
        panel = panel.child(
            div().text_xs().font_family(theme::MONO).text_color(t.bad).child(format!("!! {err}")),
        );
    }

    for s in &r.shares {
        panel = panel.child(accel_share_row(
            &format!("rmt-{label}"),
            s,
            false,
            Some(label.clone()),
            cx,
        ));
    }

    panel
        .when_some(app.remote_add_inputs.get(&label), |el, input| {
            el.child(field("Add share link", input)).child(
                btn((SharedString::from(format!("rmt-add-{label}")), 0), "Add share to this remote")
                    .on_click(cx.listener({
                        let label = label.clone();
                        move |this, _: &ClickEvent, _, cx| this.remote_add_share(label.clone(), cx)
                    })),
            )
        })
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
                            .font_family(theme::MONO)
                            .text_color(t.muted)
                            .child("THEME"),
                    )
                    .child(theme_dropdown(app, cx)),
            ),
        )
        .child(
            card().child(section_title("Startup")).child(
                div()
                    .id("toggle-persist-shares")
                    .flex()
                    .items_center()
                    .justify_between()
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.toggle_persist_shares(cx)
                    }))
                    .child(
                        div()
                            .text_xs()
                            .font_family(theme::MONO)
                            .text_color(t.muted)
                            .child("REMEMBER SHARES & TRANSFERS ON RESTART"),
                    )
                    .child(checkmark(if s.persist_shares { Tri::On } else { Tri::Off })),
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
                .child(field("Seed RAM MiB", &app.set_seed_cache))
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
                .child(section_title("Reachability"))
                .child(field("Public relay p2p address", &app.set_relay))
                .child(field("Rendezvous URL (accelerator admin API)", &app.set_rendezvous))
                .child(
                    div().mt_1().flex().child(
                        primary_btn("apply-settings-relay", "Save settings").on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.apply_settings(cx)),
                        ),
                    ),
                )
                .child(hint(
                    "A relay's dialable …/p2p/<id> address (e.g. one you run on a public \
                     VPS with `accelerator run --role relay`). When set, every share this \
                     node creates also reserves a slot on it and adds that address to the \
                     link, so it stays dialable even from a network with no path to this \
                     machine directly. Leave blank for LAN/VPN-only sharing.",
                ))
                .child(hint(
                    "Rendezvous URL: that accelerator's rendezvous address (host:port; \
                     TLS is automatic) — usually the same as its admin API, but its \
                     operator may run it on a separate address (e.g. admin kept private \
                     behind a VPN, rendezvous exposed publicly). Ask the operator if \
                     unsure. Two peers that have never talked before use it to swap \
                     current addresses and punch a direct hole through NAT — no data \
                     flows through it, just a few KB of signaling. Works even without a \
                     relay reservation above.",
                )),
        )
        .child(
            card()
                .child(section_title("In effect"))
                .child(kv("Folder", s.download_dir.display().to_string()))
                .child(kv("Download cap", cap(s.download_cap_bps)))
                .child(kv("Upload cap", cap(s.upload_cap_bps)))
                .child(kv("Cache storage cap", cap(s.storage_cap_bytes)))
                .child(kv("Seed RAM buffer", human_bytes(s.seed_cache_bytes)))
                .child(kv(
                    "Auto update-check",
                    match s.auto_resync_secs {
                        Some(v) => format!("every {} min", v / 60),
                        None => "off".into(),
                    },
                ))
                .child(kv(
                    "Public relay",
                    s.public_relay.clone().unwrap_or_else(|| "not set".into()),
                ))
                .child(kv(
                    "Rendezvous URL",
                    s.rendezvous_url.clone().unwrap_or_else(|| "not set".into()),
                )),
        )
        .child(hint(
            "Blank a limit field to clear it (unlimited / off). Seed RAM is the \
             hot-chunk cache each shared folder keeps — it streams from disk, so a \
             big share needs only a small buffer. With shares remembered on restart, \
             every seeded folder and subscription comes back on its own next launch.",
        ))
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

/// The "are you sure?" overlay for a Remove button, or `None` when nothing is
/// armed. Rendered on top of everything via `deferred`.
pub fn confirm_modal(app: &Gaggle, cx: &mut Context<Gaggle>) -> Option<AnyElement> {
    let c = app.confirm.as_ref()?;
    let t = theme::active();

    let mut actions = div().flex().flex_wrap().w_full().gap_2().justify_end().child(
        btn("cf-cancel", "Cancel")
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.confirm_cancel(cx))),
    );
    let body: String = match &c.kind {
        ConfirmKind::Share => {
            actions = actions.child(danger_btn("cf-rm", "Remove").on_click(
                cx.listener(|this, _: &ClickEvent, _, cx| this.confirm_go(false, cx)),
            ));
            "Stops seeding this folder. Your local files are left untouched.".into()
        }
        ConfirmKind::Transfer { output_dir: Some(dir) } => {
            actions = actions
                .child(btn("cf-keep", "Remove, keep files").on_click(cx.listener(
                    |this, _: &ClickEvent, _, cx| this.confirm_go(false, cx),
                )))
                .child(danger_btn("cf-del", "Remove + delete files").on_click(cx.listener(
                    |this, _: &ClickEvent, _, cx| this.confirm_go(true, cx),
                )));
            format!("Downloaded files are at {}", dir.display())
        }
        ConfirmKind::Transfer { output_dir: None } => {
            actions = actions.child(danger_btn("cf-rm", "Remove").on_click(
                cx.listener(|this, _: &ClickEvent, _, cx| this.confirm_go(false, cx)),
            ));
            "Any partial download data is discarded.".into()
        }
        ConfirmKind::AccelShare { on_disk, .. } => {
            actions = actions.child(danger_btn("cf-rm", "Remove").on_click(
                cx.listener(|this, _: &ClickEvent, _, cx| this.confirm_go(false, cx)),
            ));
            if *on_disk {
                "Deletes this share's replica from the NAS disk. This can't be undone — \
                 re-adding the share would re-download it. To stop uploading without \
                 losing the data, use the Seed toggle instead."
                    .into()
            } else {
                "Stops this accelerator from caching the share.".into()
            }
        }
    };

    // A *definite* width (not a min/max range): a flex column with only a
    // min/max width leaves gpui measuring the body paragraph at its unwrapped
    // single-line width, so the card's computed height ignored the wrap and the
    // buttons rendered below the border. A fixed width wraps the text properly.
    let card = div()
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .w(px(460.0))
        .max_w(relative(0.92))
        .bg(t.panel)
        .border_1()
        .border_color(t.accent)
        .child(section_title(&format!("Remove “{}”?", c.name)))
        .child(
            div()
                .w_full()
                .text_xs()
                .font_family(theme::MONO)
                .text_color(t.muted)
                .child(body),
        )
        .child(actions)
        // Clicks inside the card must not fall through to the backdrop.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());

    // Delete-with-files is a separate, more destructive click — Enter only
    // ever triggers the same action as a plain "Remove".
    let overlay = div()
        .id("confirm-overlay")
        .track_focus(&app.confirm_focus)
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(hsla(0.0, 0.0, 0.0, 0.55))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, _, cx| this.confirm_cancel(cx)),
        )
        .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
            if event.keystroke.key == "enter" {
                this.confirm_go(false, cx);
            }
        }))
        .child(card);

    Some(deferred(overlay).into_any_element())
}

/// Dispatch to the body for `tab`.
pub fn body(tab: Tab, app: &Gaggle, cx: &mut Context<Gaggle>) -> AnyElement {
    match tab {
        Tab::Shares => shares(app, cx),
        Tab::Transfers => transfers(app, cx),
        Tab::Accelerator => accelerator(app, cx),
        Tab::Stats => stats(app, cx),
        Tab::Settings => settings(app, cx),
        Tab::Logs => logs(app, cx),
    }
}

/// Selectable graph windows: label + span in seconds.
const STATS_WINDOWS: [(&str, u64); 4] = [("1m", 60), ("5m", 300), ("15m", 900), ("1h", 3600)];

/// Throughput graphs over a configurable window, for this machine or a
/// connected remote accelerator. All the history lives in
/// [`app_state::AppState::stats`]; this tab only slices and draws it.
pub fn stats(app: &Gaggle, cx: &mut Context<Gaggle>) -> AnyElement {
    let t = theme::active();
    let win = app.stats_window;
    let remotes: Vec<String> =
        app.state.remote_accelerators.iter().map(|r| r.label.clone()).collect();
    // A remote that was forgotten since it was picked falls back to Local.
    let source = match &app.stats_source {
        StatsSource::Remote(l) if remotes.iter().any(|r| r == l) => StatsSource::Remote(l.clone()),
        _ => StatsSource::Local,
    };

    let controls = card().child(section_title("Throughput")).child(
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_2()
            .child(stats_window_chips(app, cx))
            .child(stats_source_dropdown(app, &remotes, cx)),
    );

    let body = match &source {
        StatsSource::Local => {
            let samples = slice_window(&app.state.stats.local, win);
            div()
                .flex()
                .flex_col()
                .gap_3()
                .child(speed_chart_card("Download", &samples, win, |s| s.down_bps, t.accent))
                .child(speed_chart_card("Upload", &samples, win, |s| s.up_bps, t.info))
        }
        StatsSource::Remote(label) => {
            let samples = app
                .state
                .stats
                .accelerators
                .iter()
                .find(|a| &a.label == label)
                .map(|a| slice_window(&a.history, win))
                .unwrap_or_default();
            div().flex().flex_col().gap_3().child(speed_chart_card(
                &format!("Served — {label}"),
                &samples,
                win,
                |s| s.up_bps,
                t.accent,
            ))
        }
    };

    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(controls)
        .child(body)
        .child(hint(
            "Sampled every ~2 s (kept up to an hour) and smoothed between \
             readings. \"Local\" is this machine's own transfer + serving rate; \
             a remote accelerator only reports what it serves outward.",
        ))
        .into_any_element()
}

/// The samples within `window` of the most recent one.
fn slice_window(samples: &[SpeedSample], window: Duration) -> Vec<SpeedSample> {
    let Some(newest) = samples.last().map(|s| s.at) else {
        return Vec::new();
    };
    let cutoff = newest.checked_sub(window).unwrap_or(SystemTime::UNIX_EPOCH);
    samples.iter().filter(|s| s.at >= cutoff).copied().collect()
}

fn stats_window_chips(app: &Gaggle, cx: &mut Context<Gaggle>) -> AnyElement {
    let t = theme::active();
    let cur = app.stats_window.as_secs();
    let mut row = div().flex().items_center().gap_2();
    for (label, secs) in STATS_WINDOWS {
        let active = cur == secs;
        row = row.child(
            div()
                .id(("stats-win", secs as usize))
                .px_2()
                .py_1()
                .border_1()
                .border_color(if active { t.accent } else { t.line })
                .bg(if active { t.accent } else { t.panel_hi })
                .text_color(if active { t.on_accent } else { t.fg })
                .text_xs()
                .font_weight(FontWeight::BOLD)
                .cursor_pointer()
                .child(label)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.set_stats_window(Duration::from_secs(secs), cx)
                })),
        );
    }
    row.into_any_element()
}

/// "Local" + one entry per registered remote accelerator, styled like the
/// Settings theme selector.
fn stats_source_dropdown(app: &Gaggle, remotes: &[String], cx: &mut Context<Gaggle>) -> AnyElement {
    let t = theme::active();
    let open = app.stats_source_menu_open;
    let cur = match &app.stats_source {
        StatsSource::Remote(l) if remotes.iter().any(|r| r == l) => l.clone(),
        _ => "Local".to_string(),
    };

    let trigger = div()
        .id("stats-source-trigger")
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
        .child(cur.to_uppercase())
        .child(div().text_color(t.muted).child(if open { "▲" } else { "▼" }))
        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_stats_source_menu(cx)));

    let mut wrap = div().relative().child(trigger);
    if open {
        let mut opts: Vec<(String, StatsSource)> = vec![("Local".into(), StatsSource::Local)];
        for r in remotes {
            opts.push((r.clone(), StatsSource::Remote(r.clone())));
        }
        let menu = div()
            .absolute()
            .top_full()
            .right_0()
            .mt_1()
            .min_w(px(160.0))
            .flex()
            .flex_col()
            .bg(t.panel)
            .border_1()
            .border_color(t.accent_dim)
            .children(opts.into_iter().enumerate().map(|(i, (label, src))| {
                let selected = label.eq_ignore_ascii_case(&cur);
                div()
                    .id(("stats-src-opt", i))
                    .px_3()
                    .py_1()
                    .text_xs()
                    .font_weight(FontWeight::BOLD)
                    .bg(if selected { t.panel_hi } else { t.panel })
                    .text_color(if selected { t.accent } else { t.fg })
                    .cursor_pointer()
                    .hover(|s| s.bg(t.panel_hi).text_color(t.accent))
                    .child(label.to_uppercase())
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.set_stats_source(src.clone(), cx)
                    }))
            }));
        wrap = wrap.child(deferred(menu));
    }
    wrap.into_any_element()
}

/// Points the throughput graphs interpolate their raw ~2 s samples onto. Fixed
/// (bar a tiny-window clamp) regardless of the selected span so the categorical
/// x-axis never folds repeated labels together, and dense enough that the
/// 200 ms redraw shows a smoothly advancing curve rather than a step per
/// sample.
const SMOOTH_POINTS: usize = 90;

/// One titled line chart of `value(sample)` over `window`, plus now/peak
/// readouts. The raw samples are monotone-cubic resampled against a live `now`
/// each redraw, so the line glides between the ~2 s readings instead of
/// freezing then jumping once per sample.
fn speed_chart_card(
    title: &str,
    samples: &[SpeedSample],
    window: Duration,
    value: impl Fn(&SpeedSample) -> u64,
    color: Hsla,
) -> AnyElement {
    let t = theme::active();
    let latest = samples.last().map(&value).unwrap_or(0);
    let peak = samples.iter().map(&value).max().unwrap_or(0);

    let head = div()
        .flex()
        .items_center()
        .justify_between()
        .child(section_title(title))
        .child(
            div()
                .text_xs()
                .font_family(theme::MONO)
                .text_color(t.muted)
                .child(format!("NOW {} · PEAK {}", human_rate(latest), human_rate(peak))),
        );

    if samples.len() < 2 {
        return card()
            .child(head)
            .child(hint("collecting data — check back in a few seconds"))
            .into_any_element();
    }

    // One grid point per whole second at most, so `fmt_ago` labels stay unique
    // (the chart's categorical x-scale maps equal labels to one position).
    let n = SMOOTH_POINTS.min(window.as_secs() as usize + 1).max(2);
    let points: Vec<(SharedString, f64)> =
        app_state::resample(samples, SystemTime::now(), window, n, |s| value(s) as f64)
            .into_iter()
            .map(|(ago, v)| (SharedString::from(fmt_ago(ago)), v))
            .collect();
    let tick_margin = (points.len() / 6).max(1);
    let chart = LineChart::new(points)
        .x(|p: &(SharedString, f64)| p.0.clone())
        .y(|p: &(SharedString, f64)| p.1)
        .stroke(color)
        .tick_margin(tick_margin);

    card()
        .child(head)
        .child(div().w_full().h(px(180.0)).child(chart))
        .into_any_element()
}

/// A compact — and crucially *unique per whole second* — "how long ago" label
/// for a graph's x-axis. The chart's categorical x-scale collapses any two
/// points sharing a label onto the same position, so minute-resolution labels
/// (the old behaviour) folded a wide window's line onto itself.
fn fmt_ago(secs: f64) -> String {
    let s = secs.round() as u64;
    if s == 0 {
        "now".into()
    } else if s < 60 {
        format!("-{s}s")
    } else if s < 3600 {
        format!("-{}m{:02}", s / 60, s % 60)
    } else {
        format!("-{}h{:02}", s / 3600, (s % 3600) / 60)
    }
}

/// Fixed row height for the Logs tab's `uniform_list`. `uniform_list` derives
/// its scroll geometry from measuring a single item and applying that height
/// to every row; leaving height to auto-size from text let it drift between
/// renders (visible as uneven gaps/overlap between lines), so every row
/// pins to this instead.
const LOG_ROW_HEIGHT: f32 = 22.0;

/// Captured `tracing` output from every crate in the process — a packaged GUI
/// has no attached console, so this is the only place a background-task
/// warning (a failed relay reservation, a rejected connection, …) is visible.
fn logs(app: &Gaggle, cx: &mut Context<Gaggle>) -> AnyElement {
    let t = theme::active();

    let level_btn = |level: LogLevel, id: &'static str, label: &'static str| {
        let active = app.log_min_level == level;
        div()
            .id(id)
            .px_2()
            .py_1()
            .border_1()
            .border_color(if active { t.accent } else { t.line })
            .bg(if active { t.accent } else { t.panel_hi })
            .text_color(if active { t.on_accent } else { t.fg })
            .text_xs()
            .font_weight(FontWeight::BOLD)
            .cursor_pointer()
            .child(label)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.set_log_min_level(level, cx)
            }))
    };

    // Newest-first display order, as plain indices into `app.logs` —
    // recomputed by `Gaggle::recompute_log_order` only when `logs` or
    // `log_min_level` actually change, *not* here: this function reruns on
    // every scroll tick (the virtualized list below re-renders as it
    // scrolls), and re-filtering up to 4000 lines that often was the main
    // source of scroll jank. The actual per-line elements are still built
    // lazily, only for whatever range `uniform_list` reports as on-screen.
    let order = app.log_order.clone();
    let total = app.logs.len();
    let shown = order.len();

    let mut col = div()
        .flex()
        .flex_col()
        .gap_2()
        .flex_1()
        .min_h_0()
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
                        .child(level_btn(LogLevel::Trace, "log-lvl-all", "ALL"))
                        .child(level_btn(LogLevel::Info, "log-lvl-info", "INFO+"))
                        .child(level_btn(LogLevel::Warn, "log-lvl-warn", "WARN+"))
                        .child(level_btn(LogLevel::Error, "log-lvl-error", "ERROR"))
                        .child(
                            div()
                                .text_xs()
                                .text_color(t.muted)
                                .child(if shown == total {
                                    format!("{shown} lines")
                                } else {
                                    format!("{shown} of {total} lines")
                                }),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(btn("copy-logs", "Copy").on_click(cx.listener(
                            |this, _: &ClickEvent, _, cx| this.copy_logs(cx),
                        )))
                        .child(danger_btn("clear-logs", "Clear").on_click(cx.listener(
                            |this, _: &ClickEvent, _, cx| this.clear_logs(cx),
                        ))),
                ),
        )
        .child(hint(
            "Everything this process has logged, oldest at the bottom. RUST_LOG (if set \
             before launch) controls verbosity; INFO+ is the default.",
        ));

    if order.is_empty() {
        col = col.child(hint("NO LOG LINES YET AT THIS LEVEL."));
    } else {
        let logs = app.logs.clone();
        let list = uniform_list("log-lines", order.len(), move |range, _window, _cx| {
            range
                .map(|i| {
                    let line = &logs[order[i]];
                    let color = match line.level {
                        LogLevel::Error | LogLevel::Warn => t.bad,
                        LogLevel::Info => t.fg,
                        LogLevel::Debug | LogLevel::Trace => t.muted,
                    };
                    let stripe = if i % 2 == 0 { t.panel } else { t.panel_hi };
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_1()
                        .h(px(LOG_ROW_HEIGHT))
                        .overflow_hidden()
                        .bg(stripe)
                        .child(
                            div()
                                .w(px(64.0))
                                .flex_shrink_0()
                                .text_color(t.muted)
                                .child(fmt_log_time(line.time_unix)),
                        )
                        .child(div().w(px(40.0)).flex_shrink_0().text_color(color).child(line.level.label()))
                        .child(
                            div()
                                .w(px(160.0))
                                .flex_shrink_0()
                                .truncate()
                                .text_color(t.muted)
                                .child(line.target.clone()),
                        )
                        .child(div().flex_1().truncate().text_color(color).child(line.message.clone()))
                })
                .collect::<Vec<_>>()
        })
        .flex_1()
        .min_h_0()
        .w_full()
        .font_family(theme::MONO)
        .text_xs();
        col = col.child(list);
    }

    col.into_any_element()
}
