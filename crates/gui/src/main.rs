//! Gaggle GUI v1 (milestone 8): a gpui + gpui-component shell over the headless
//! [`app_state::App`] transfer manager.
//!
//! Three views — **Shares** (local folders this node originates), **Transfers**
//! (downloads in flight), and **Settings** — each a plain render of the
//! [`app_state::AppState`] snapshot. The GUI never touches `net`: it calls the
//! sync methods on `App`, polls `App::snapshot` on a timer, and re-renders.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use app_state::{App, AppState, Settings, ShareLink, Theme, TransferRow, TransferStatus};
use gpui::prelude::*;
use gpui::{
    Application, ClickEvent, ClipboardItem, Hsla, PathPromptOptions, SharedString, Timer, Window,
    WindowOptions, div, px, relative,
};

// --- palette (a single dark theme for v1) --------------------------------
const fn c(h: f32, s: f32, l: f32) -> Hsla {
    Hsla {
        h: h / 360.0,
        s,
        l,
        a: 1.0,
    }
}
const BG: Hsla = c(222.0, 0.16, 0.11);
const PANEL: Hsla = c(222.0, 0.15, 0.15);
const PANEL_HI: Hsla = c(222.0, 0.14, 0.19);
const FG: Hsla = c(0.0, 0.0, 0.92);
const MUTED: Hsla = c(0.0, 0.0, 0.62);
const ACCENT: Hsla = c(210.0, 0.75, 0.58);
const GOOD: Hsla = c(150.0, 0.55, 0.5);
const BAD: Hsla = c(2.0, 0.7, 0.6);
const TRACK: Hsla = c(222.0, 0.14, 0.24);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Shares,
    Transfers,
    Settings,
}

struct Gaggle {
    app: Arc<App>,
    state: AppState,
    tab: Tab,
    notice: Option<SharedString>,
}

impl Gaggle {
    fn new(app: Arc<App>, cx: &mut Context<Self>) -> Self {
        let state = app.snapshot();

        // Poll the manager and re-render on change.
        cx.spawn(async move |this, cx| {
            loop {
                Timer::after(Duration::from_millis(200)).await;
                let stop = this
                    .update(cx, |this: &mut Gaggle, cx| {
                        let next = this.app.snapshot();
                        this.state = next;
                        cx.notify();
                    })
                    .is_err();
                if stop {
                    break;
                }
            }
        })
        .detach();

        Self {
            app,
            state,
            tab: Tab::Shares,
            notice: None,
        }
    }

    fn set_notice(&mut self, msg: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.notice = Some(msg.into());
        cx.notify();
    }

    fn pick_folder(&mut self, cx: &mut Context<Self>) {
        let app = self.app.clone();
        let recv = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Share folder".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = recv.await
                && let Some(dir) = paths.into_iter().next()
            {
                app.add_local_share(dir);
                let _ = this.update(cx, |this: &mut Gaggle, cx| {
                    this.set_notice("Snapshotting folder…", cx);
                });
            }
        })
        .detach();
    }

    fn paste_subscription(&mut self, cx: &mut Context<Self>) {
        let text = cx.read_from_clipboard().and_then(|c| c.text());
        match text.as_deref().map(ShareLink::parse) {
            Some(Ok(link)) => {
                let name = link.name.clone();
                self.app.subscribe(link.into_request());
                self.tab = Tab::Transfers;
                self.set_notice(format!("Subscribed to “{name}”"), cx);
            }
            Some(Err(e)) => self.set_notice(format!("Clipboard is not a share link: {e}"), cx),
            None => self.set_notice("Clipboard is empty — copy a share link first", cx),
        }
    }

    fn copy_link(&mut self, row: &TransferRow, cx: &mut Context<Self>) {
        let Some(addr) = row.share_addr.clone() else {
            return;
        };
        let link = ShareLink::new(row.name.clone(), row.manifest_id, vec![addr]).encode();
        println!("{}", link);
        cx.write_to_clipboard(ClipboardItem::new_string(link));
        self.set_notice("Share link copied to clipboard", cx);
    }

    fn cycle_theme(&mut self, cx: &mut Context<Self>) {
        let cur = self.state.settings.theme;
        let next = match cur {
            Theme::System => Theme::Light,
            Theme::Light => Theme::Dark,
            Theme::Dark => Theme::System,
        };
        self.app.update_settings(Settings {
            theme: next,
            ..self.state.settings.clone()
        });
        self.set_notice(format!("Theme → {}", next.label()), cx);
    }
}

impl Render for Gaggle {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(BG)
            .text_color(FG)
            .text_sm()
            .child(self.header(cx))
            .child(
                div()
                    .id("body")
                    .flex_1()
                    .overflow_y_scroll()
                    .p_4()
                    .child(match self.tab {
                        Tab::Shares => self.shares_view(cx),
                        Tab::Transfers => self.transfers_view(cx),
                        Tab::Settings => self.settings_view(cx),
                    }),
            )
            .child(self.status_bar())
    }
}

impl Gaggle {
    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tab_btn = |this: &Gaggle, cx: &mut Context<Self>, tab: Tab, label: &'static str| {
            let active = this.tab == tab;
            div()
                .id(label)
                .px_3()
                .py_1()
                .rounded_md()
                .bg(if active { ACCENT } else { PANEL_HI })
                .text_color(if active { BG } else { FG })
                .child(label)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.tab = tab;
                    cx.notify();
                }))
        };
        div()
            .flex()
            .items_center()
            .gap_3()
            .px_4()
            .py_3()
            .bg(PANEL)
            .border_b_1()
            .border_color(TRACK)
            .child(div().font_weight(gpui::FontWeight::BOLD).child("Gaggle"))
            .child(tab_btn(self, cx, Tab::Shares, "Shares"))
            .child(tab_btn(self, cx, Tab::Transfers, "Transfers"))
            .child(tab_btn(self, cx, Tab::Settings, "Settings"))
    }

    fn status_bar(&self) -> impl IntoElement {
        let s = &self.state.swarm;
        let text = self.notice.clone().unwrap_or_else(|| {
            format!("{} seeding · {} downloading", s.seeding, s.downloading).into()
        });
        div()
            .flex()
            .px_4()
            .py_2()
            .bg(PANEL)
            .border_t_1()
            .border_color(TRACK)
            .text_xs()
            .text_color(MUTED)
            .child(text)
    }

    fn shares_view(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut col = div().flex().flex_col().gap_2().child(
            div()
                .id("add-folder")
                .px_3()
                .py_2()
                .rounded_md()
                .bg(ACCENT)
                .text_color(BG)
                .child("+ Add folder…")
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.pick_folder(cx))),
        );

        let seeds: Vec<TransferRow> = self.state.seeds().cloned().collect();
        if seeds.is_empty() {
            col = col.child(hint("No shared folders yet — add one to start seeding."));
        }
        for row in seeds {
            col = col.child(self.share_row(&row, cx));
        }
        col.into_any_element()
    }

    fn share_row(&mut self, row: &TransferRow, cx: &mut Context<Self>) -> impl IntoElement {
        let id = row.id;
        let addr = row
            .share_addr
            .as_ref()
            .map(|a| a.to_string())
            .unwrap_or_else(|| "starting…".into());
        card()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(row.name.clone()),
                    )
                    .child(status_pill(row.status)),
            )
            .child(div().text_xs().text_color(MUTED).child(format!(
                "{} files · {}",
                row.files,
                human_bytes(row.total_bytes)
            )))
            .child(div().text_xs().text_color(MUTED).child(addr))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        div()
                            .id(("copy", id as usize))
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(PANEL_HI)
                            .text_xs()
                            .child("Copy link")
                            .on_click(cx.listener({
                                let row = row.clone();
                                move |this, _: &ClickEvent, _, cx| this.copy_link(&row, cx)
                            })),
                    )
                    .child(
                        div()
                            .id(("rm", id as usize))
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(PANEL_HI)
                            .text_xs()
                            .text_color(BAD)
                            .child("Remove")
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.app.remove(id);
                                cx.notify();
                            })),
                    ),
            )
    }

    fn transfers_view(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut col = div().flex().flex_col().gap_2().child(
            div()
                .id("paste-sub")
                .px_3()
                .py_2()
                .rounded_md()
                .bg(PANEL_HI)
                .child("Paste subscription link (from clipboard)")
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.paste_subscription(cx))),
        );

        let downloads: Vec<TransferRow> = self.state.downloads().cloned().collect();
        if downloads.is_empty() {
            col = col.child(hint(
                "No downloads. Copy a share link on another node, then paste it here.",
            ));
        }
        for row in downloads {
            col = col.child(self.transfer_row(&row, cx));
        }
        col.into_any_element()
    }

    fn transfer_row(&mut self, row: &TransferRow, cx: &mut Context<Self>) -> impl IntoElement {
        let id = row.id;
        let frac = row.progress().clamp(0.0, 1.0);
        let bar = div().w_full().h(px(6.0)).rounded_full().bg(TRACK).child(
            div().h_full().w(relative(frac)).rounded_full().bg(
                if row.status == TransferStatus::Failed {
                    BAD
                } else {
                    ACCENT
                },
            ),
        );

        let line = format!(
            "{} / {}  ·  {}/s  ·  {} source(s)",
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
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(row.name.clone()),
                    )
                    .child(status_pill(row.status)),
            )
            .child(bar)
            .child(div().text_xs().text_color(MUTED).child(line))
            .when_some(row.error.clone(), |el, e| {
                el.child(div().text_xs().text_color(BAD).child(e))
            })
            .child(
                div()
                    .flex()
                    .gap_2()
                    .when(can_pause, |el| {
                        el.child(
                            div()
                                .id(("pause", id as usize))
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .bg(PANEL_HI)
                                .text_xs()
                                .child("Pause")
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.app.pause(id);
                                    cx.notify();
                                })),
                        )
                    })
                    .when(can_resume, |el| {
                        el.child(
                            div()
                                .id(("resume", id as usize))
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .bg(PANEL_HI)
                                .text_xs()
                                .child("Resume")
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.app.resume(id);
                                    cx.notify();
                                })),
                        )
                    })
                    .child(
                        div()
                            .id(("drm", id as usize))
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(PANEL_HI)
                            .text_xs()
                            .text_color(BAD)
                            .child("Remove")
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.app.remove(id);
                                cx.notify();
                            })),
                    ),
            )
    }

    fn settings_view(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let s = &self.state.settings;
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                card()
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child("Appearance"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(div().text_color(MUTED).child("Theme"))
                            .child(
                                div()
                                    .id("theme")
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .bg(PANEL_HI)
                                    .child(s.theme.label())
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.cycle_theme(cx)
                                    })),
                            ),
                    ),
            )
            .child(
                card()
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child("Downloads"),
                    )
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
}

// --- small view helpers -------------------------------------------------

fn card() -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .p_3()
        .rounded_md()
        .bg(PANEL)
        .border_1()
        .border_color(TRACK)
}

fn hint(text: &'static str) -> gpui::Div {
    div().p_3().text_xs().text_color(MUTED).child(text)
}

fn kv(key: &'static str, value: String) -> gpui::Div {
    div()
        .flex()
        .justify_between()
        .text_xs()
        .child(div().text_color(MUTED).child(key))
        .child(div().child(value))
}

fn status_pill(status: TransferStatus) -> gpui::Div {
    let (color, _) = match status {
        TransferStatus::Complete => (GOOD, ()),
        TransferStatus::Failed => (BAD, ()),
        TransferStatus::Paused => (MUTED, ()),
        _ => (ACCENT, ()),
    };
    div()
        .px_2()
        .py_1()
        .rounded_full()
        .bg(TRACK)
        .text_xs()
        .text_color(color)
        .child(status.label())
}

fn cap(bytes_per_sec: Option<u64>) -> String {
    match bytes_per_sec {
        Some(v) => format!("{}/s", human_bytes(v)),
        None => "unlimited".into(),
    }
}

fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("gaggle").join("settings.json"))
}

fn main() -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let app = runtime.block_on(app_state::App::new(config_path()))?;
    // The manager's background tasks live on this runtime for the whole process.
    std::mem::forget(runtime);
    let app = Arc::new(app);

    Application::new().run(move |cx| {
        gpui_component::init(cx);
        let app = app.clone();
        cx.open_window(WindowOptions::default(), |_, cx| {
            cx.new(|cx| Gaggle::new(app, cx))
        })
        .expect("failed to open window");
        cx.activate(true);
    });
    Ok(())
}
