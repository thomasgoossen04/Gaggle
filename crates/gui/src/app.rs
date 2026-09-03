//! The one gpui view: [`Gaggle`] holds the latest [`AppState`] snapshot, the
//! active tab, the per-row expansion set, the Settings / Accelerator / invite
//! form inputs and the title-bar drag latch. All rendering is delegated to
//! [`crate::ui`]; all behaviour is a thin wrapper over [`app_state::App`].

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use app_state::{
    AcceleratorRequest, App, AppState, Scope, Settings, ShareLink, Theme, TransferId, TransferRow,
};
use gpui::prelude::*;
use gpui::{ClipboardItem, Entity, PathPromptOptions, SharedString, Timer, Window, div};
use gpui_component::input::InputState;

use crate::util::{
    fmt_minutes, fmt_rate_mib, fmt_size_gib, parse_minutes, parse_rate_mib, parse_size_gib,
};
use crate::{clipboard, theme, ui};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Transfers,
    Shares,
    Accelerator,
    Settings,
}

/// How long a minted invite stays valid.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ExpiryChoice {
    Never,
    Hour,
    Day,
    Week,
    Month,
}

impl ExpiryChoice {
    pub fn label(self) -> &'static str {
        match self {
            ExpiryChoice::Never => "never",
            ExpiryChoice::Hour => "1 hour",
            ExpiryChoice::Day => "1 day",
            ExpiryChoice::Week => "7 days",
            ExpiryChoice::Month => "30 days",
        }
    }

    pub fn next(self) -> Self {
        match self {
            ExpiryChoice::Never => ExpiryChoice::Hour,
            ExpiryChoice::Hour => ExpiryChoice::Day,
            ExpiryChoice::Day => ExpiryChoice::Week,
            ExpiryChoice::Week => ExpiryChoice::Month,
            ExpiryChoice::Month => ExpiryChoice::Never,
        }
    }

    /// Absolute unix expiry, or `None` for a token that never expires.
    pub fn as_unix(self) -> Option<u64> {
        let secs = match self {
            ExpiryChoice::Never => return None,
            ExpiryChoice::Hour => 3600,
            ExpiryChoice::Day => 86_400,
            ExpiryChoice::Week => 7 * 86_400,
            ExpiryChoice::Month => 30 * 86_400,
        };
        let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
        Some(now + secs)
    }
}

pub struct Gaggle {
    pub(crate) app: Arc<App>,
    pub(crate) state: AppState,
    pub(crate) tab: Tab,
    pub(crate) notice: Option<SharedString>,
    /// Rows whose detail panel (swarm inspector / invite form) is open.
    pub(crate) expanded: HashSet<TransferId>,
    pub(crate) invite_expiry: ExpiryChoice,
    /// The Settings theme dropdown is open.
    pub(crate) theme_menu_open: bool,
    /// Last `ThemeMode` pushed into gpui-component, so `render` can re-sync it
    /// when the resolved mode drifts (the OS appearance can land after frame 1).
    pub(crate) theme_mode: Option<gpui_component::ThemeMode>,
    /// Armed on mouse-down in the title bar; a subsequent move starts a
    /// compositor window drag (so a plain click still reaches the buttons).
    pub(crate) dragging: bool,

    // Settings form.
    pub(crate) set_dir: Entity<InputState>,
    pub(crate) set_dl: Entity<InputState>,
    pub(crate) set_ul: Entity<InputState>,
    pub(crate) set_store: Entity<InputState>,
    pub(crate) set_resync: Entity<InputState>,
    // Invite form (one open at a time).
    pub(crate) invite_paths: Entity<InputState>,
    // Accelerator form.
    pub(crate) accel_cache: Entity<InputState>,
    pub(crate) accel_link: Entity<InputState>,
    pub(crate) accel_dir: Entity<InputState>,
}

impl Gaggle {
    pub fn new(app: Arc<App>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = app.snapshot();
        let s = &state.settings;

        // Sync gpui-component's own theme to the persisted setting *with a real
        // window*, so form inputs render in the right mode from the first frame
        // (an earlier `Theme::change` in `main` runs before a window exists and
        // can guess the appearance wrong).
        let initial_mode = theme::activate(state.settings.theme, window.appearance());
        gpui_component::Theme::change(initial_mode, Some(&mut *window), cx);

        // A field that only accepts a decimal / integer number (or empty).
        let decimal = regex::Regex::new(r"^\d*\.?\d*$").unwrap();
        let integer = regex::Regex::new(r"^\d*$").unwrap();

        let text = |cx: &mut Context<Self>, window: &mut Window, initial: String| {
            cx.new(|cx| InputState::new(window, cx).default_value(initial))
        };
        let num = |cx: &mut Context<Self>, window: &mut Window, initial: String, re: regex::Regex| {
            cx.new(|cx| InputState::new(window, cx).default_value(initial).pattern(re))
        };

        let set_dir = text(cx, window, s.download_dir.display().to_string());
        let set_dl = num(cx, window, fmt_rate_mib(s.download_cap_bps), decimal.clone());
        let set_ul = num(cx, window, fmt_rate_mib(s.upload_cap_bps), decimal.clone());
        let set_store = num(cx, window, fmt_size_gib(s.storage_cap_bytes), decimal);
        let set_resync = num(cx, window, fmt_minutes(s.auto_resync_secs), integer.clone());
        let invite_paths = cx.new(|cx| InputState::new(window, cx).multi_line(true).auto_grow(1, 4));
        let accel_cache = num(cx, window, "256".into(), integer);
        let accel_link = text(cx, window, String::new());
        let accel_dir = text(cx, window, String::new());

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
            tab: Tab::Transfers,
            notice: None,
            expanded: HashSet::new(),
            invite_expiry: ExpiryChoice::Never,
            theme_menu_open: false,
            theme_mode: Some(initial_mode),
            dragging: false,
            set_dir,
            set_dl,
            set_ul,
            set_store,
            set_resync,
            invite_paths,
            accel_cache,
            accel_link,
            accel_dir,
        }
    }

    pub(crate) fn set_notice(&mut self, msg: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.notice = Some(msg.into());
        cx.notify();
    }

    pub(crate) fn toggle_expand(&mut self, id: TransferId, cx: &mut Context<Self>) {
        if !self.expanded.remove(&id) {
            self.expanded.insert(id);
        }
        cx.notify();
    }

    pub(crate) fn pick_folder(&mut self, private: bool, cx: &mut Context<Self>) {
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
                if private {
                    app.add_private_share(dir);
                } else {
                    app.add_local_share(dir);
                }
                let _ = this.update(cx, |this: &mut Gaggle, cx| {
                    this.set_notice("Snapshotting folder…", cx);
                });
            }
        })
        .detach();
    }

    /// Native folder picker for the Settings download-directory field.
    pub(crate) fn browse_download_dir(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.set_dir.clone();
        self.browse_into(target, window, cx);
    }

    /// Native folder picker for the Accelerator replica-directory field.
    pub(crate) fn browse_accel_dir(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.accel_dir.clone();
        self.browse_into(target, window, cx);
    }

    /// Open a native folder picker and write the chosen path into `target`.
    fn browse_into(
        &mut self,
        target: Entity<InputState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let recv = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose folder".into()),
        });
        window
            .spawn(cx, async move |cx| {
                if let Ok(Ok(Some(paths))) = recv.await
                    && let Some(dir) = paths.into_iter().next()
                {
                    let _ = target.update_in(cx, |st, window, cx| {
                        st.set_value(dir.display().to_string(), window, cx);
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

    pub(crate) fn copy_text(&mut self, text: String, note: &str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        clipboard::copy(&text);
        self.set_notice(note.to_string(), cx);
    }

    pub(crate) fn copy_link(&mut self, row: &TransferRow, cx: &mut Context<Self>) {
        let Some(addr) = row.share_addr.clone() else {
            self.set_notice("Share is still coming online — try again in a moment", cx);
            return;
        };
        if row.private {
            self.set_notice("Private share — mint an invite from the row's ▸ panel", cx);
            return;
        }
        let link = ShareLink::new(row.name.clone(), row.manifest_id, vec![addr]).encode();
        self.copy_text(link, "Share link copied to clipboard", cx);
    }

    pub(crate) fn rescan(&mut self, id: TransferId, cx: &mut Context<Self>) {
        self.app.rescan_share(id);
        self.set_notice("Rescanning folder…", cx);
    }

    pub(crate) fn check_updates(&mut self, id: TransferId, cx: &mut Context<Self>) {
        self.app.check_updates(id);
        self.set_notice("Checking for a newer version…", cx);
    }

    pub(crate) fn resync(&mut self, id: TransferId, cx: &mut Context<Self>) {
        self.app.resync(id);
        self.set_notice("Pulling the delta…", cx);
    }

    pub(crate) fn cycle_expiry(&mut self, cx: &mut Context<Self>) {
        self.invite_expiry = self.invite_expiry.next();
        cx.notify();
    }

    pub(crate) fn mint_invite(&mut self, seed_id: TransferId, cx: &mut Context<Self>) {
        let raw = self.invite_paths.read(cx).value().to_string();
        let paths: Vec<String> = raw
            .split(['\n', ','])
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        let scope = if paths.is_empty() { Scope::All } else { Scope::files(paths) };
        self.app.mint_invite(seed_id, scope, self.invite_expiry.as_unix());
        self.set_notice("Minting invite…", cx);
    }

    pub(crate) fn apply_settings(&mut self, cx: &mut Context<Self>) {
        let dir = self.set_dir.read(cx).value().to_string();
        let dl = parse_rate_mib(&self.set_dl.read(cx).value());
        let ul = parse_rate_mib(&self.set_ul.read(cx).value());
        let store = parse_size_gib(&self.set_store.read(cx).value());
        let resync = parse_minutes(&self.set_resync.read(cx).value());

        let mut next = self.state.settings.clone();
        if !dir.trim().is_empty() {
            next.download_dir = dir.trim().into();
        }
        next.download_cap_bps = dl;
        next.upload_cap_bps = ul;
        next.storage_cap_bytes = store;
        next.auto_resync_secs = resync;
        self.app.update_settings(next);
        self.set_notice("Settings saved", cx);
    }

    pub(crate) fn run_benchmark(&mut self, cx: &mut Context<Self>) {
        self.app.benchmark();
        self.set_notice("Benchmarking the download volume…", cx);
    }

    pub(crate) fn start_relay(&mut self, cx: &mut Context<Self>) {
        let cache_mib = self.accel_cache.read(cx).value().trim().parse::<u64>().unwrap_or(256);
        let upstream = ShareLink::parse(self.accel_link.read(cx).value().trim()).ok();
        self.app.start_accelerator(AcceleratorRequest::Relay {
            cache_bytes: cache_mib.max(16) * 1024 * 1024,
            upstream,
        });
        self.set_notice("Starting relay accelerator…", cx);
    }

    pub(crate) fn start_nas(&mut self, cx: &mut Context<Self>) {
        let dir = self.accel_dir.read(cx).value().trim().to_string();
        let link = ShareLink::parse(self.accel_link.read(cx).value().trim());
        match (dir.is_empty(), link) {
            (true, _) => self.set_notice("NAS mode needs a replica directory", cx),
            (_, Err(e)) => self.set_notice(format!("NAS mode needs a valid share link: {e}"), cx),
            (false, Ok(source)) => {
                self.app.start_accelerator(AcceleratorRequest::Nas {
                    dir: dir.into(),
                    source,
                    materialize: None,
                });
                self.set_notice("Starting NAS replica…", cx);
            }
        }
    }

    pub(crate) fn stop_accelerator(&mut self, cx: &mut Context<Self>) {
        self.app.stop_accelerator();
        self.set_notice("Accelerator stopped", cx);
    }

    pub(crate) fn toggle_theme_menu(&mut self, cx: &mut Context<Self>) {
        self.theme_menu_open = !self.theme_menu_open;
        cx.notify();
    }

    pub(crate) fn set_theme(&mut self, theme: Theme, window: &mut Window, cx: &mut Context<Self>) {
        self.theme_menu_open = false;
        // Reflect it locally right away; the manager echo (via the 200 ms poll)
        // would otherwise briefly render the old theme.
        self.state.settings.theme = theme;
        self.app.update_settings(Settings {
            theme,
            ..self.state.settings.clone()
        });
        let mode = theme::activate(theme, window.appearance());
        self.theme_mode = Some(mode);
        gpui_component::Theme::change(mode, Some(window), cx);
        self.set_notice(format!("Theme → {}", theme.label()), cx);
    }
}

impl Render for Gaggle {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mode = theme::activate(self.state.settings.theme, window.appearance());
        // Keep gpui-component's own theme (window border, inputs) in step — the
        // resolved mode can change after the first frame when the OS appearance
        // finally lands, or on a live system theme switch.
        if self.theme_mode != Some(mode) {
            self.theme_mode = Some(mode);
            gpui_component::Theme::change(mode, Some(&mut *window), cx);
        }
        let t = theme::active();

        // The `window_border` frame is drawn by the wrapping `gpui_component::Root`.
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
            .child(ui::chrome::status_bar(self))
    }
}
