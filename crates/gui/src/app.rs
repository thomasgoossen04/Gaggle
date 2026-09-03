//! The one gpui view: [`Gaggle`] holds the latest [`AppState`] snapshot, the
//! active tab and the title-bar drag latch. All rendering is delegated to
//! [`crate::ui`]; all behaviour is a thin wrapper over [`app_state::App`].

use std::sync::Arc;
use std::time::Duration;

use app_state::{App, AppState, Settings, ShareLink, Theme, TransferRow};
use gpui::prelude::*;
use gpui::{ClipboardItem, PathPromptOptions, SharedString, Timer, Window, div};
use gpui_component::window_border;

use crate::{clipboard, theme, ui};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Shares,
    Transfers,
    Settings,
}

pub struct Gaggle {
    pub(crate) app: Arc<App>,
    pub(crate) state: AppState,
    pub(crate) tab: Tab,
    pub(crate) notice: Option<SharedString>,
    /// Armed on mouse-down in the title bar; a subsequent move starts a
    /// compositor window drag (so a plain click still reaches the buttons).
    pub(crate) dragging: bool,
}

impl Gaggle {
    pub fn new(app: Arc<App>, cx: &mut Context<Self>) -> Self {
        let state = app.snapshot();

        // Poll the manager and re-render on change.
        cx.spawn(async move |this, cx| {
            loop {
                Timer::after(Duration::from_millis(200)).await;
                let stop = this
                    .update(cx, |this: &mut Gaggle, cx| {
                        this.state = this.app.snapshot();
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
            dragging: false,
        }
    }

    pub(crate) fn set_notice(&mut self, msg: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.notice = Some(msg.into());
        cx.notify();
    }

    pub(crate) fn pick_folder(&mut self, cx: &mut Context<Self>) {
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

    pub(crate) fn paste_subscription(&mut self, cx: &mut Context<Self>) {
        let text = cx.read_from_clipboard().and_then(|c| c.text());
        match text.as_deref().map(str::trim).map(ShareLink::parse) {
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

    pub(crate) fn copy_link(&mut self, row: &TransferRow, cx: &mut Context<Self>) {
        let Some(addr) = row.share_addr.clone() else {
            self.set_notice("Share is still coming online — try again in a moment", cx);
            return;
        };
        let link = ShareLink::new(row.name.clone(), row.manifest_id, vec![addr]).encode();
        cx.write_to_clipboard(ClipboardItem::new_string(link.clone()));
        clipboard::copy(&link);
        self.set_notice("Share link copied to clipboard", cx);
    }

    pub(crate) fn cycle_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let next = match self.state.settings.theme {
            Theme::System => Theme::Light,
            Theme::Light => Theme::Dark,
            Theme::Dark => Theme::System,
        };
        self.app.update_settings(Settings {
            theme: next,
            ..self.state.settings.clone()
        });
        // Reflect it immediately; `render` would pick it up on the next poll but
        // this keeps the click snappy and syncs the window frame.
        let mode = theme::activate(next, window.appearance());
        gpui_component::Theme::change(mode, Some(window), cx);
        self.set_notice(format!("Theme → {}", next.label()), cx);
    }
}

impl Render for Gaggle {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Keep the palette in step with the persisted setting every frame; cheap
        // (a thread-local swap) and covers changes made outside `cycle_theme`.
        theme::activate(self.state.settings.theme, window.appearance());
        let t = theme::active();

        // `window_border` draws our own 1px frame + drop shadow and wires the
        // edge/corner resize grips, since the server titlebar is gone.
        window_border().child(
            div()
                .flex()
                .flex_col()
                .size_full()
                .bg(t.bg)
                .text_color(t.fg)
                .text_sm()
                .child(ui::chrome::header(self, window, cx))
                .child(
                    div()
                        .id("body")
                        .flex_1()
                        .overflow_y_scroll()
                        .p_4()
                        .child(ui::views::body(self.tab, self, cx)),
                )
                .child(ui::chrome::status_bar(self)),
        )
    }
}
