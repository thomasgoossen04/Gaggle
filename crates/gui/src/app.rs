//! The one gpui view: [`Gaggle`] holds the latest [`AppState`] snapshot, the
//! active tab, the per-row expansion set, the Settings / Accelerator / invite
//! form inputs and the title-bar drag latch. All rendering is delegated to
//! [`crate::ui`]; all behaviour is a thin wrapper over [`app_state::App`].

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use app_state::{
    AcceleratorRequest, App, AppState, Hash, LauncherChannel, LogHandle, LogLevel, LogLine,
    ReachLink, Scope, Settings, ShareLink, Theme, TransferId, TransferKind, TransferRow,
};
use gpui::prelude::*;
use gpui::{ClipboardItem, Entity, FocusHandle, PathPromptOptions, SharedString, Timer, Window, div};
use gpui_component::input::InputState;

use crate::util::{
    fmt_minutes, fmt_rate_mib, fmt_size_gib, fmt_size_mib, parse_minutes, parse_rate_mib,
    parse_size_gib, parse_size_mib,
};
use crate::{clipboard, theme, ui};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Transfers,
    Shares,
    Accelerator,
    Stats,
    Settings,
    Logs,
}

/// Which series the Stats tab is showing: this machine, or a named remote
/// accelerator.
#[derive(Clone, PartialEq, Eq)]
pub enum StatsSource {
    Local,
    Remote(String),
}

/// The curve one Stats graph last actually drew, so the next 200 ms redraw can
/// ease toward the fresh resample instead of snapping to its new shape. Keyed
/// per graph in [`Gaggle::stats_ease`]. See [`crate::ui::views::speed_chart_card`].
pub(crate) struct EasedCurve {
    pub at: Instant,
    pub vals: Vec<f64>,
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

/// A pending "are you sure?" for a Remove button.
pub struct Confirm {
    pub id: TransferId,
    pub name: String,
    pub kind: ConfirmKind,
}

pub enum ConfirmKind {
    /// A seeded folder — stop serving; local files untouched.
    Share,
    /// A download — discard it, optionally deleting `output_dir`.
    Transfer { output_dir: Option<PathBuf> },
    /// A share carried by an accelerator. Removing a NAS share (`on_disk`)
    /// deletes its replica from disk; `remote` names the daemon when it is a
    /// remote accelerator's share.
    AccelShare { manifest_id: String, on_disk: bool, remote: Option<String> },
    /// Restart a remote accelerator daemon so it comes back on a newer build.
    RestartRemote { label: String },
}

pub struct Gaggle {
    pub(crate) app: Arc<App>,
    pub(crate) state: AppState,
    pub(crate) tab: Tab,
    pub(crate) notice: Option<SharedString>,
    /// The process's captured `tracing` output — see the Logs tab.
    pub(crate) log_handle: LogHandle,
    /// The last-polled snapshot of `log_handle`, refreshed only while the
    /// Logs tab is open. `Rc`-shared rather than cloned per render so the
    /// virtualized log list (`ui::views::logs`) can hand its render closure
    /// a cheap handle instead of copying every line's strings each frame.
    pub(crate) logs: Rc<[LogLine]>,
    /// `log_handle.version()` as of the last `logs` refresh — lets the
    /// poller skip re-snapshotting (and the re-render it would trigger)
    /// when nothing new has been logged.
    pub(crate) logs_version: u64,
    /// Newest-first indices into `logs` passing `log_min_level`, recomputed
    /// only in [`Self::recompute_log_order`] (whenever `logs` or
    /// `log_min_level` actually changes) rather than on every render — the
    /// Logs tab's `uniform_list` re-renders on every scroll tick, and
    /// re-filtering up to 4000 lines on each of those was the main source of
    /// scroll jank.
    pub(crate) log_order: Rc<[usize]>,
    /// Logs tab: only show lines at or above this severity.
    pub(crate) log_min_level: LogLevel,
    /// Rows whose detail panel (swarm inspector / invite form) is open.
    pub(crate) expanded: HashSet<TransferId>,
    pub(crate) invite_expiry: ExpiryChoice,
    /// The private seed whose invite file-picker is currently populated.
    pub(crate) invite_for: Option<TransferId>,
    /// Manifest paths ticked for the next minted invite.
    pub(crate) invite_sel: HashSet<String>,
    /// Expanded folders in the invite file tree.
    pub(crate) tree_expanded: HashSet<String>,
    /// A Remove button is awaiting confirmation.
    pub(crate) confirm: Option<Confirm>,
    /// Holds keyboard focus while [`Self::confirm`] is armed, so the modal's
    /// `on_key_down` (Enter → confirm) actually receives key events — gpui
    /// routes keys to whatever's focused, and nothing else in this app calls
    /// `window.focus`, so without this Enter would fall through to the root.
    pub(crate) confirm_focus: FocusHandle,
    /// The Settings theme dropdown is open.
    pub(crate) theme_menu_open: bool,
    /// The desktop launcher's current release channel, read from
    /// `launcher.json` at startup and after each change from the dropdown.
    /// Advanced-mode Settings only — a plain GUI run (no launcher) still shows
    /// it but the switch simply has no launcher to act on.
    pub(crate) launcher_channel: LauncherChannel,
    /// The Settings release-channel dropdown is open.
    pub(crate) launcher_menu_open: bool,
    /// Stats tab: how far back the graphs reach. Ephemeral view state — never
    /// persisted to `Settings`.
    pub(crate) stats_window: Duration,
    /// Stats tab: which series is shown.
    pub(crate) stats_source: StatsSource,
    /// Stats tab: the source dropdown is open.
    pub(crate) stats_source_menu_open: bool,
    /// When an open dropdown was last dismissed by a click *outside* it (its
    /// `on_mouse_down_out`). If that click landed on the dropdown's own trigger,
    /// the trigger's `on_click` fires a beat later in the same gesture and would
    /// otherwise re-open what the outside-click just closed — so a `toggle_*`
    /// within this short window is swallowed instead of flipped. See
    /// [`Self::menu_click_swallowed`].
    pub(crate) menu_dismissed_at: Option<Instant>,
    /// Stats tab: per-graph temporal smoothing state — the last-drawn curve for
    /// each graph, eased toward every fresh resample so the line glides rather
    /// than re-morphing on each 200 ms redraw. Interior-mutable because it is
    /// updated from `render` (which only holds `&Gaggle` by the time it reaches
    /// the view fns). Keyed "local:down" / "local:up" / "remote:<label>".
    pub(crate) stats_ease: RefCell<HashMap<String, EasedCurve>>,
    /// Last `ThemeMode` pushed into gpui-component, so `render` can re-sync it
    /// when the resolved mode drifts (the OS appearance can land after frame 1).
    pub(crate) theme_mode: Option<gpui_component::ThemeMode>,
    /// Armed on mouse-down in the title bar; a subsequent move starts a
    /// compositor window drag (so a plain click still reaches the buttons).
    pub(crate) dragging: bool,
    /// Transfers tab: the "Browse public shares" directory panel is open.
    pub(crate) show_directory: bool,

    // Settings form.
    pub(crate) set_dir: Entity<InputState>,
    pub(crate) set_dl: Entity<InputState>,
    pub(crate) set_ul: Entity<InputState>,
    pub(crate) set_store: Entity<InputState>,
    pub(crate) set_resync: Entity<InputState>,
    /// Seed hot-chunk cache budget, in MiB.
    pub(crate) set_seed_cache: Entity<InputState>,
    /// A relay's `…/p2p/<id>` address — see [`Settings::public_relay`].
    pub(crate) set_relay: Entity<InputState>,
    /// An accelerator's HTTP base URL — see [`Settings::rendezvous_url`].
    pub(crate) set_rendezvous: Entity<InputState>,
    // Accelerator form.
    pub(crate) accel_cache: Entity<InputState>,
    pub(crate) accel_link: Entity<InputState>,
    pub(crate) accel_dir: Entity<InputState>,
    /// Paste field: add a share to the *running* local accelerator.
    pub(crate) accel_add: Entity<InputState>,
    // Remote accelerator form.
    pub(crate) remote_label: Entity<InputState>,
    pub(crate) remote_url: Entity<InputState>,
    /// Paste field: add a share to a remote accelerator, keyed by that
    /// remote's label — one `InputState` per row. Sharing a single entity
    /// across rows made every row echo whatever was typed in any one of
    /// them, since they'd all be bound to the same underlying state.
    /// Kept in sync with `state.remote_accelerators` by [`Self::sync_remote_inputs`].
    pub(crate) remote_add_inputs: HashMap<String, Entity<InputState>>,
    /// Replica-folder + storage-cap edit fields for a remote NAS daemon, keyed
    /// by label. Pre-filled with the daemon's current values on the first poll
    /// that carries them (`seeded`). Also synced by [`Self::sync_remote_inputs`].
    pub(crate) remote_storage_inputs: HashMap<String, RemoteStorageInputs>,
}

/// The two input entities behind a remote NAS daemon's storage edit form.
pub struct RemoteStorageInputs {
    pub dir: Entity<InputState>,
    /// Storage cap in GiB, as typed text. Blank = no cap.
    pub cap_gib: Entity<InputState>,
    /// `true` once pre-filled from a status poll, so a later poll doesn't
    /// clobber what the operator is typing.
    pub seeded: bool,
}

impl Gaggle {
    pub fn new(app: Arc<App>, log_handle: LogHandle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = app.snapshot();
        let s = &state.settings;

        // Sync gpui-component's own theme to the persisted setting *with a real
        // window*, so form inputs render in the right mode from the first frame
        // (an earlier `Theme::change` in `main` runs before a window exists and
        // can guess the appearance wrong).
        let initial_mode = theme::activate(state.settings.theme, window.appearance());
        theme::apply_mode(initial_mode, Some(&mut *window), cx);

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
        let set_seed_cache = num(cx, window, fmt_size_mib(s.seed_cache_bytes), integer.clone());
        let set_relay = text(cx, window, s.public_relay.clone().unwrap_or_default());
        let set_rendezvous = text(cx, window, s.rendezvous_url.clone().unwrap_or_default());
        let accel_cache = num(cx, window, "256".into(), integer);
        let accel_link = text(cx, window, String::new());
        let accel_dir = text(cx, window, String::new());
        let accel_add = text(cx, window, String::new());
        let remote_label = text(cx, window, String::new());
        let remote_url = text(cx, window, String::new());
        let remote_add_inputs = state
            .remote_accelerators
            .iter()
            .map(|r| (r.label.clone(), text(cx, window, String::new())))
            .collect();
        let remote_storage_inputs = HashMap::new();

        // Poll the manager (and, while the Logs tab is open, the log buffer)
        // and re-render on change. Both checks are cheap no-ops when nothing
        // changed (a `watch` version check, an atomic load) — the actual
        // state clone / log snapshot / `cx.notify()` (which forces a full
        // re-render of whichever tab is showing) only happen when there's
        // something new, instead of unconditionally 5x/sec forever. That
        // matters most for the heavier tabs (Logs, Accelerator): a redundant
        // background re-render was competing for frame budget right when a
        // tab switch also needed to lay out a tab's worth of elements fresh.
        let mut state_rx = app.state_watch();
        cx.spawn(async move |this, cx| {
            loop {
                Timer::after(Duration::from_millis(200)).await;
                let stop = this
                    .update(cx, |this: &mut Gaggle, cx| {
                        let mut changed = false;
                        if state_rx.has_changed().unwrap_or(false) {
                            this.state = state_rx.borrow_and_update().clone();
                            changed = true;
                        }
                        if this.tab == Tab::Logs {
                            let v = this.log_handle.version();
                            if v != this.logs_version {
                                this.logs_version = v;
                                this.logs = this.log_handle.snapshot().into();
                                this.recompute_log_order();
                                changed = true;
                            }
                        }
                        if changed {
                            cx.notify();
                        }
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
            log_handle,
            logs: Rc::from([]),
            logs_version: 0,
            log_order: Rc::from([]),
            log_min_level: LogLevel::Info,
            expanded: HashSet::new(),
            invite_expiry: ExpiryChoice::Never,
            invite_for: None,
            invite_sel: HashSet::new(),
            tree_expanded: HashSet::new(),
            confirm: None,
            confirm_focus: cx.focus_handle(),
            theme_menu_open: false,
            launcher_channel: app_state::launcher_channel::default_path()
                .map(|p| app_state::launcher_channel::read(&p))
                .unwrap_or_default(),
            launcher_menu_open: false,
            stats_window: Duration::from_secs(300),
            stats_source: StatsSource::Local,
            stats_source_menu_open: false,
            menu_dismissed_at: None,
            stats_ease: RefCell::new(HashMap::new()),
            theme_mode: Some(initial_mode),
            dragging: false,
            show_directory: false,
            set_dir,
            set_dl,
            set_ul,
            set_store,
            set_resync,
            set_seed_cache,
            set_relay,
            set_rendezvous,
            accel_cache,
            accel_link,
            accel_dir,
            accel_add,
            remote_label,
            remote_url,
            remote_add_inputs,
            remote_storage_inputs,
        }
    }

    /// Ensure `remote_add_inputs` has exactly one entity per currently-known
    /// remote label, creating entities for newly-added remotes and dropping
    /// ones for remotes that were forgotten. Must run somewhere that has a
    /// `&mut Window` (creating an `InputState` requires one), so it's called
    /// from [`Render::render`] rather than from the (window-less) view layer.
    fn sync_remote_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let known = |label: &String| self.state.remote_accelerators.iter().any(|r| &r.label == label);
        self.remote_add_inputs.retain(|label, _| known(label));
        self.remote_storage_inputs.retain(|label, _| known(label));
        // Collect first — the `set_value` calls below need `&mut cx` while we'd
        // otherwise still be borrowing `self.state`.
        let want: Vec<(String, Option<String>, Option<u64>)> = self
            .state
            .remote_accelerators
            .iter()
            .map(|r| (r.label.clone(), r.replica_dir.clone(), r.storage_cap_bytes))
            .collect();
        for (label, replica_dir, cap) in want {
            self.remote_add_inputs.entry(label.clone()).or_insert_with(|| {
                cx.new(|cx| InputState::new(window, cx).default_value(String::new()))
            });
            let entry = self.remote_storage_inputs.entry(label).or_insert_with(|| {
                RemoteStorageInputs {
                    dir: cx.new(|cx| InputState::new(window, cx).default_value(String::new())),
                    cap_gib: cx.new(|cx| InputState::new(window, cx).default_value(String::new())),
                    seeded: false,
                }
            });
            if !entry.seeded && let Some(dir) = replica_dir {
                let cap_text = cap
                    .map(|b| format!("{:.0}", b as f64 / (1u64 << 30) as f64))
                    .unwrap_or_default();
                entry.dir.update(cx, |st, cx| st.set_value(dir, window, cx));
                entry.cap_gib.update(cx, |st, cx| st.set_value(cap_text, window, cx));
                entry.seeded = true;
            }
        }
    }

    /// Switch tabs, refreshing the log snapshot immediately when switching
    /// *to* Logs rather than waiting for the next 200ms poll tick.
    pub(crate) fn switch_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tab = tab;
        if tab == Tab::Logs {
            self.logs_version = self.log_handle.version();
            self.logs = self.log_handle.snapshot().into();
            self.recompute_log_order();
        }
        cx.notify();
    }

    /// Rebuild [`Self::log_order`] from the current `logs` + `log_min_level`.
    /// Call whenever either changes — never from the render path, since the
    /// Logs tab's `uniform_list` re-renders on every scroll tick and
    /// filtering up to 4000 lines that often is real, measurable cost.
    fn recompute_log_order(&mut self) {
        self.log_order = self
            .logs
            .iter()
            .enumerate()
            .filter(|(_, l)| l.level >= self.log_min_level)
            .map(|(i, _)| i)
            .rev()
            .collect();
    }

    pub(crate) fn set_log_min_level(&mut self, level: LogLevel, cx: &mut Context<Self>) {
        self.log_min_level = level;
        self.recompute_log_order();
        cx.notify();
    }

    pub(crate) fn clear_logs(&mut self, cx: &mut Context<Self>) {
        self.log_handle.clear();
        self.logs = Rc::from([]);
        self.log_order = Rc::from([]);
        self.logs_version = self.log_handle.version();
        cx.notify();
    }

    pub(crate) fn copy_logs(&mut self, cx: &mut Context<Self>) {
        let text = self
            .logs
            .iter()
            .filter(|l| l.level >= self.log_min_level)
            .map(|l| format!("{} {:<5} {} {}", crate::util::fmt_log_time(l.time_unix), l.level, l.target, l.message))
            .collect::<Vec<_>>()
            .join("\n");
        self.copy_text(text, "Logs copied to clipboard", cx);
    }

    pub(crate) fn set_notice(&mut self, msg: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.notice = Some(msg.into());
        cx.notify();
    }

    /// Arm the Remove confirmation for row `id`.
    pub(crate) fn ask_remove(&mut self, id: TransferId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.state.get(id) else { return };
        let kind = match row.kind {
            TransferKind::Seeding => ConfirmKind::Share,
            TransferKind::Downloading => {
                ConfirmKind::Transfer { output_dir: row.output_dir.clone() }
            }
        };
        self.confirm = Some(Confirm { id, name: row.name.clone(), kind });
        // So the modal's on_key_down (Enter → confirm) receives events.
        self.confirm_focus.focus(window);
        cx.notify();
    }

    /// Arm the Remove confirmation for a share carried by an accelerator.
    /// `on_disk` warns that a NAS replica will be deleted; `remote` names the
    /// daemon for a remote accelerator's share.
    pub(crate) fn ask_remove_accel_share(
        &mut self,
        manifest_id: String,
        name: String,
        on_disk: bool,
        remote: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.confirm = Some(Confirm {
            id: 0,
            name,
            kind: ConfirmKind::AccelShare { manifest_id, on_disk, remote },
        });
        self.confirm_focus.focus(window);
        cx.notify();
    }

    /// Arm the Restart confirmation for a remote accelerator daemon.
    pub(crate) fn ask_restart_remote(
        &mut self,
        label: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.confirm = Some(Confirm {
            id: 0,
            name: label.clone(),
            kind: ConfirmKind::RestartRemote { label },
        });
        self.confirm_focus.focus(window);
        cx.notify();
    }

    pub(crate) fn confirm_cancel(&mut self, cx: &mut Context<Self>) {
        self.confirm = None;
        cx.notify();
    }

    /// Act on the armed confirmation. `delete_files` only applies to a download.
    pub(crate) fn confirm_go(&mut self, delete_files: bool, cx: &mut Context<Self>) {
        if let Some(c) = self.confirm.take() {
            match &c.kind {
                ConfirmKind::AccelShare { manifest_id, remote, .. } => match remote {
                    Some(label) => self.app.remote_remove_share(label.clone(), manifest_id.clone()),
                    None => self.app.accel_remove_share(manifest_id.clone()),
                },
                ConfirmKind::RestartRemote { label } => {
                    self.app.restart_remote_accelerator(label.clone());
                    self.set_notice(format!("Restarting “{}”…", c.name), cx);
                    cx.notify();
                    return;
                }
                _ if delete_files => self.app.remove_and_delete(c.id),
                _ => self.app.remove(c.id),
            }
            self.set_notice(format!("Removed “{}”", c.name), cx);
        }
        cx.notify();
    }

    pub(crate) fn toggle_expand(&mut self, id: TransferId, cx: &mut Context<Self>) {
        if self.expanded.remove(&id) {
            cx.notify();
            return;
        }
        self.expanded.insert(id);
        // Opening a private seed's panel: seed the invite file picker with
        // everything ticked (whole folder) and all folders collapsed.
        if let Some(paths) =
            self.state.get(id).filter(|r| r.private).map(|r| r.file_paths.clone())
        {
            self.invite_for = Some(id);
            self.invite_sel = paths.iter().cloned().collect();
            self.tree_expanded.clear();
        }
        cx.notify();
    }

    pub(crate) fn toggle_tree_dir(&mut self, dir: String, cx: &mut Context<Self>) {
        if !self.tree_expanded.remove(&dir) {
            self.tree_expanded.insert(dir);
        }
        cx.notify();
    }

    pub(crate) fn toggle_invite_file(&mut self, path: String, cx: &mut Context<Self>) {
        if !self.invite_sel.remove(&path) {
            self.invite_sel.insert(path);
        }
        cx.notify();
    }

    /// Tick / untick every file under `dir` (all-or-nothing).
    pub(crate) fn toggle_invite_dir(&mut self, dir: String, cx: &mut Context<Self>) {
        let Some(files) =
            self.invite_for.and_then(|id| self.state.get(id)).map(|r| r.file_paths.clone())
        else {
            return;
        };
        let prefix = format!("{dir}/");
        let under: Vec<&String> = files.iter().filter(|p| p.starts_with(&prefix)).collect();
        let all_on = !under.is_empty() && under.iter().all(|p| self.invite_sel.contains(*p));
        for p in under {
            if all_on {
                self.invite_sel.remove(p);
            } else {
                self.invite_sel.insert(p.clone());
            }
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
                self.app.subscribe(link.into());
                self.tab = Tab::Transfers;
                self.set_notice(format!("Subscribed to “{name}”"), cx);
            }
            Some(Err(e)) => self.set_notice(format!("Clipboard is not a share link: {e}"), cx),
            None => self.set_notice("Clipboard is empty — copy a share link first", cx),
        }
    }

    /// Toggle the "Browse public shares" panel; opening it kicks a tracker
    /// directory refresh.
    pub(crate) fn toggle_directory(&mut self, cx: &mut Context<Self>) {
        self.show_directory = !self.show_directory;
        if self.show_directory {
            if self.state.settings.rendezvous_url.is_none() {
                self.set_notice(
                    "Set a Rendezvous / tracker URL in Settings to browse shares",
                    cx,
                );
            }
            self.app.refresh_directory();
        }
        cx.notify();
    }

    pub(crate) fn refresh_directory(&mut self, cx: &mut Context<Self>) {
        self.app.refresh_directory();
        self.set_notice("Refreshing shared folders…", cx);
    }

    /// Subscribe to a public share discovered on the tracker.
    pub(crate) fn join_discovered(
        &mut self,
        manifest_id: Hash,
        name: String,
        cx: &mut Context<Self>,
    ) {
        self.app.subscribe_discovered(manifest_id, name.clone());
        self.tab = Tab::Transfers;
        self.set_notice(format!("Joining “{name}”…"), cx);
    }

    pub(crate) fn copy_text(&mut self, text: String, note: &str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        clipboard::copy(&text);
        self.set_notice(note.to_string(), cx);
    }

    pub(crate) fn copy_link(&mut self, row: &TransferRow, cx: &mut Context<Self>) {
        if row.share_addrs.is_empty() {
            self.set_notice("Share is still coming online — try again in a moment", cx);
            return;
        }
        if row.private {
            self.set_notice("Private share — mint an invite from the row's ▸ panel", cx);
            return;
        }
        let link =
            ShareLink::new(row.name.clone(), row.manifest_id, row.share_addrs.clone()).encode();
        self.copy_text(link, "Share link copied to clipboard", cx);
    }

    pub(crate) fn rescan(&mut self, id: TransferId, cx: &mut Context<Self>) {
        self.app.rescan_share(id);
        self.set_notice("Rescanning folder…", cx);
    }

    /// Open a completed download's folder in the system file manager.
    pub(crate) fn open_output_dir(&mut self, id: TransferId, cx: &mut Context<Self>) {
        match self.state.get(id).and_then(|r| r.output_dir.clone()) {
            Some(dir) => {
                cx.open_with_system(&dir);
                self.set_notice("Opening download folder…", cx);
            }
            None => self.set_notice("This transfer has no folder yet", cx),
        }
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
        let all: Vec<String> = self
            .state
            .get(seed_id)
            .map(|r| r.file_paths.as_ref().clone())
            .unwrap_or_default();
        // Everything ticked ⇒ a whole-folder grant; otherwise a per-file scope.
        let scope = if !all.is_empty() && all.iter().all(|p| self.invite_sel.contains(p)) {
            Scope::All
        } else {
            Scope::files(self.invite_sel.iter().cloned())
        };
        self.app.mint_invite(seed_id, scope, self.invite_expiry.as_unix());
        self.set_notice("Minting invite…", cx);
    }

    pub(crate) fn apply_settings(&mut self, cx: &mut Context<Self>) {
        let dir = self.set_dir.read(cx).value().to_string();
        let dl = parse_rate_mib(&self.set_dl.read(cx).value());
        let ul = parse_rate_mib(&self.set_ul.read(cx).value());
        let store = parse_size_gib(&self.set_store.read(cx).value());
        let resync = parse_minutes(&self.set_resync.read(cx).value());
        let seed_cache = parse_size_mib(&self.set_seed_cache.read(cx).value());
        let relay = self.set_relay.read(cx).value().trim().to_string();
        let rendezvous = self.set_rendezvous.read(cx).value().trim().to_string();

        let mut next = self.state.settings.clone();
        if !dir.trim().is_empty() {
            next.download_dir = dir.trim().into();
        }
        next.download_cap_bps = dl;
        next.upload_cap_bps = ul;
        next.storage_cap_bytes = store;
        next.auto_resync_secs = resync;
        // Blank / unparseable keeps the current budget (core enforces a floor).
        next.seed_cache_bytes = seed_cache.unwrap_or(self.state.settings.seed_cache_bytes);
        next.public_relay = if relay.is_empty() { None } else { Some(relay) };
        next.rendezvous_url = if rendezvous.is_empty() { None } else { Some(rendezvous) };
        self.app.update_settings(next);
        self.set_notice("Settings saved", cx);
    }

    /// Flip the advanced UI surface (Accelerator + Logs tabs, editable
    /// Reachability fields). Turning it off while on one of the now-hidden tabs
    /// bounces back to Transfers.
    pub(crate) fn toggle_advanced_ui(&mut self, cx: &mut Context<Self>) {
        let next = !self.state.settings.advanced_ui;
        self.state.settings.advanced_ui = next;
        self.app.update_settings(Settings { advanced_ui: next, ..self.state.settings.clone() });
        if !next && matches!(self.tab, Tab::Accelerator | Tab::Logs) {
            self.tab = Tab::Transfers;
        }
        self.set_notice(
            if next {
                "Advanced mode on — Accelerator & Logs tabs shown"
            } else {
                "Advanced mode off"
            },
            cx,
        );
    }

    /// Normal-mode Reachability: apply a `gagglenet1…` link from the clipboard
    /// to the relay + rendezvous settings (and the still-hidden edit fields, so
    /// they're correct if Advanced mode is turned on later).
    pub(crate) fn paste_reachability(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = cx.read_from_clipboard().and_then(|c| c.text());
        match text.as_deref().map(str::trim).map(ReachLink::parse) {
            Some(Ok(link)) => {
                let mut next = self.state.settings.clone();
                next.public_relay = link.public_relay.clone();
                next.rendezvous_url = link.rendezvous_url.clone();
                self.state.settings = next.clone();
                self.app.update_settings(next);

                let relay = link.public_relay.unwrap_or_default();
                let rendezvous = link.rendezvous_url.unwrap_or_default();
                self.set_relay.update(cx, |st, cx| st.set_value(relay, window, cx));
                self.set_rendezvous.update(cx, |st, cx| st.set_value(rendezvous, window, cx));
                self.set_notice("Reachability settings applied from clipboard", cx);
            }
            Some(Err(e)) => self.set_notice(format!("Clipboard is not a reachability link: {e}"), cx),
            None => self.set_notice("Clipboard is empty — copy a reachability link first", cx),
        }
    }

    /// Advanced-mode Reachability: bundle the two edit fields into a short
    /// `gagglenet1…` link on the clipboard, to paste on another device.
    pub(crate) fn copy_reachability(&mut self, cx: &mut Context<Self>) {
        let relay = self.set_relay.read(cx).value().to_string();
        let rendezvous = self.set_rendezvous.read(cx).value().to_string();
        let link = ReachLink::from_fields(&relay, &rendezvous);
        if link.is_empty() {
            self.set_notice("Set a relay address or rendezvous URL first", cx);
            return;
        }
        self.copy_text(link.encode(), "Reachability link copied to clipboard", cx);
    }

    pub(crate) fn toggle_persist_shares(&mut self, cx: &mut Context<Self>) {
        let next = !self.state.settings.persist_shares;
        // Reflect it locally right away, same as `set_theme` — the manager's
        // echo would otherwise take a poll cycle to show the new state.
        self.state.settings.persist_shares = next;
        self.app.update_settings(Settings { persist_shares: next, ..self.state.settings.clone() });
        self.set_notice(
            if next {
                "Shares & transfers will be remembered across restarts"
            } else {
                "Shares & transfers will no longer be remembered"
            },
            cx,
        );
    }

    pub(crate) fn toggle_seed_after_download(&mut self, cx: &mut Context<Self>) {
        let next = !self.state.settings.seed_after_download;
        self.state.settings.seed_after_download = next;
        self.app.update_settings(Settings {
            seed_after_download: next,
            ..self.state.settings.clone()
        });
        self.set_notice(
            if next {
                "Finished downloads will keep seeding"
            } else {
                "Finished downloads will stop after downloading"
            },
            cx,
        );
    }

    pub(crate) fn toggle_seed_while_downloading(&mut self, cx: &mut Context<Self>) {
        let next = !self.state.settings.seed_while_downloading;
        self.state.settings.seed_while_downloading = next;
        self.app.update_settings(Settings {
            seed_while_downloading: next,
            ..self.state.settings.clone()
        });
        self.set_notice(
            if next {
                "Downloads will seed the chunks they already have"
            } else {
                "Downloads will only seed once complete"
            },
            cx,
        );
    }

    pub(crate) fn run_benchmark(&mut self, cx: &mut Context<Self>) {
        self.app.benchmark();
        self.set_notice("Benchmarking the download volume…", cx);
    }

    /// Parse the (possibly multi-line) share-link field into links.
    fn accel_links(&self, cx: &Context<Self>) -> (Vec<ShareLink>, usize) {
        let raw = self.accel_link.read(cx).value().to_string();
        let mut links = Vec::new();
        let mut bad = 0;
        for line in raw.split(['\n', ' ']).map(str::trim).filter(|l| !l.is_empty()) {
            match ShareLink::parse(line) {
                Ok(l) => links.push(l),
                Err(_) => bad += 1,
            }
        }
        (links, bad)
    }

    pub(crate) fn start_relay(&mut self, cx: &mut Context<Self>) {
        let cache_mib = self.accel_cache.read(cx).value().trim().parse::<u64>().unwrap_or(256);
        let (shares, bad) = self.accel_links(cx);
        if bad > 0 {
            self.set_notice(format!("{bad} share link(s) could not be parsed"), cx);
            return;
        }
        self.app.start_accelerator(AcceleratorRequest::Relay {
            cache_bytes: cache_mib.max(16) * 1024 * 1024,
            shares,
        });
        self.set_notice("Starting relay accelerator…", cx);
    }

    pub(crate) fn start_nas(&mut self, cx: &mut Context<Self>) {
        let dir = self.accel_dir.read(cx).value().trim().to_string();
        let (shares, bad) = self.accel_links(cx);
        if dir.is_empty() {
            self.set_notice("NAS mode needs a replica directory", cx);
        } else if shares.is_empty() || bad > 0 {
            self.set_notice("NAS mode needs at least one valid share link", cx);
        } else {
            self.app.start_accelerator(AcceleratorRequest::Nas {
                dir: dir.into(),
                shares,
                paused: vec![],
            });
            self.set_notice("Starting NAS replica…", cx);
        }
    }

    pub(crate) fn stop_accelerator(&mut self, cx: &mut Context<Self>) {
        self.app.stop_accelerator();
        self.set_notice("Accelerator stopped", cx);
    }

    pub(crate) fn accel_add_share(&mut self, cx: &mut Context<Self>) {
        let token = self.accel_add.read(cx).value().trim().to_string();
        if token.is_empty() {
            return;
        }
        self.app.accel_add_share(token);
        self.set_notice("Adding share to the accelerator…", cx);
    }

    /// Pause / resume serving one NAS-accelerator share.
    pub(crate) fn accel_set_seeding(&mut self, manifest_id: String, on: bool, cx: &mut Context<Self>) {
        self.app.accel_set_seeding(manifest_id, on);
        self.set_notice(if on { "Resuming share…" } else { "Paused — replica kept on disk" }, cx);
    }

    /// Pause / resume one share on a registered remote accelerator.
    pub(crate) fn remote_set_share_seeding(
        &mut self,
        label: String,
        manifest_id: String,
        on: bool,
        cx: &mut Context<Self>,
    ) {
        self.app.remote_set_share_seeding(label, manifest_id, on);
        self.set_notice(
            if on { "Resuming share on remote…" } else { "Pausing share on remote…" },
            cx,
        );
    }

    pub(crate) fn copy_operator_key(&mut self, cx: &mut Context<Self>) {
        let key = self.app.operator_public_key();
        self.copy_text(key, "Operator key copied to clipboard", cx);
    }

    pub(crate) fn add_remote(&mut self, cx: &mut Context<Self>) {
        let label = self.remote_label.read(cx).value().trim().to_string();
        let url = self.remote_url.read(cx).value().trim().to_string();
        if label.is_empty() || url.is_empty() {
            self.set_notice("A remote accelerator needs a label and an admin URL", cx);
            return;
        }
        self.app.add_remote_accelerator(label, url);
        self.set_notice("Registering remote accelerator…", cx);
    }

    pub(crate) fn remove_remote(&mut self, label: String, cx: &mut Context<Self>) {
        self.app.remove_remote_accelerator(label);
        cx.notify();
    }

    pub(crate) fn remote_add_share(&mut self, label: String, cx: &mut Context<Self>) {
        let token = match self.remote_add_inputs.get(&label) {
            Some(input) => input.read(cx).value().trim().to_string(),
            None => return,
        };
        if token.is_empty() {
            self.set_notice("Paste a share link first", cx);
            return;
        }
        self.app.remote_add_share(label, token);
        self.set_notice("Sending share to the remote accelerator…", cx);
    }

    /// Apply the replica-folder + storage-cap edit form for one remote NAS
    /// daemon. A blank folder keeps the current one; a blank cap clears it
    /// (`Some(0)` — the admin API's "unlimited" sentinel).
    pub(crate) fn remote_set_storage(&mut self, label: String, cx: &mut Context<Self>) {
        let Some(inputs) = self.remote_storage_inputs.get(&label) else { return };
        let dir = inputs.dir.read(cx).value().trim().to_string();
        let cap_text = inputs.cap_gib.read(cx).value().trim().to_string();

        let replica_dir = (!dir.is_empty()).then_some(dir);
        let storage_cap_bytes = match cap_text.parse::<f64>() {
            Ok(gib) if gib > 0.0 => Some((gib * (1u64 << 30) as f64) as u64),
            _ => Some(0), // blank / zero / unparseable → clear the cap
        };
        if replica_dir.is_none() && storage_cap_bytes == Some(0) && cap_text.is_empty() {
            self.set_notice("Nothing to apply — enter a folder or a cap", cx);
            return;
        }
        self.app.remote_set_storage(label, replica_dir, storage_cap_bytes);
        self.set_notice("Applying storage settings to the remote…", cx);
    }

    /// An open dropdown's `on_mouse_down_out` closed it. Records *when* so that a
    /// `toggle_*` firing a beat later — the trigger's own `on_click`, when the
    /// outside click landed on the trigger — is recognised as the tail of the
    /// same gesture and swallowed rather than re-opening the menu.
    pub(crate) fn note_menu_dismissed(&mut self, cx: &mut Context<Self>) {
        self.theme_menu_open = false;
        self.launcher_menu_open = false;
        self.stats_source_menu_open = false;
        self.menu_dismissed_at = Some(Instant::now());
        cx.notify();
    }

    /// Close any open dropdown when the scrollable body scrolls. The menus are
    /// `deferred` so they paint above everything — including the header the
    /// trigger scrolls under — which looks broken; native selects close on
    /// scroll too. No `menu_dismissed_at` stamp: a scroll is not a trigger
    /// click, so there is nothing to swallow afterwards.
    pub(crate) fn close_menus_on_scroll(&mut self, cx: &mut Context<Self>) {
        if self.theme_menu_open || self.launcher_menu_open || self.stats_source_menu_open {
            self.theme_menu_open = false;
            self.launcher_menu_open = false;
            self.stats_source_menu_open = false;
            cx.notify();
        }
    }

    /// True if a dropdown was dismissed by an outside click within the last
    /// beat — so this `toggle_*` is that same click reaching the trigger and
    /// must not re-open. Consumes the marker either way.
    fn menu_click_swallowed(&mut self) -> bool {
        self.menu_dismissed_at
            .take()
            .is_some_and(|t| t.elapsed() < Duration::from_millis(250))
    }

    pub(crate) fn toggle_theme_menu(&mut self, cx: &mut Context<Self>) {
        self.theme_menu_open = !self.menu_click_swallowed() && !self.theme_menu_open;
        cx.notify();
    }

    pub(crate) fn toggle_launcher_menu(&mut self, cx: &mut Context<Self>) {
        self.launcher_menu_open = !self.menu_click_swallowed() && !self.launcher_menu_open;
        cx.notify();
    }

    /// Write the chosen release channel into the launcher's `launcher.json`.
    /// It takes effect the next time `gaggle-launcher` runs its update check
    /// (e.g. next launch from the shortcut); the running GUI is untouched.
    pub(crate) fn set_launcher_channel(&mut self, channel: LauncherChannel, cx: &mut Context<Self>) {
        self.launcher_menu_open = false;
        let Some(path) = app_state::launcher_channel::default_path() else {
            self.set_notice("No launcher config location on this platform", cx);
            return;
        };
        match app_state::launcher_channel::write(&path, channel) {
            Ok(()) => {
                self.launcher_channel = channel;
                self.set_notice(
                    format!(
                        "Update channel → {} — applies on the launcher's next run",
                        channel.label()
                    ),
                    cx,
                );
            }
            Err(e) => self.set_notice(format!("Couldn't write launcher config: {e}"), cx),
        }
    }

    pub(crate) fn set_stats_window(&mut self, window: Duration, cx: &mut Context<Self>) {
        self.stats_window = window;
        cx.notify();
    }

    pub(crate) fn toggle_stats_source_menu(&mut self, cx: &mut Context<Self>) {
        self.stats_source_menu_open = !self.menu_click_swallowed() && !self.stats_source_menu_open;
        cx.notify();
    }

    pub(crate) fn set_stats_source(&mut self, source: StatsSource, cx: &mut Context<Self>) {
        self.stats_source = source;
        self.stats_source_menu_open = false;
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
        theme::apply_mode(mode, Some(window), cx);
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
            theme::apply_mode(mode, Some(&mut *window), cx);
        }
        let t = theme::active();
        self.sync_remote_inputs(window, cx);

        // The `window_border` frame is drawn by the wrapping `gpui_component::Root`.
        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(t.bg)
            .text_color(t.fg)
            .text_sm()
            .child(ui::chrome::header(self, window, cx))
            .child({
                // The Logs tab manages its own scroll region (a virtualized
                // `uniform_list`, which needs a *bounded* height to compute a
                // viewport from) rather than growing to fit its content, so it
                // can't sit in this generic auto-height scroller with every
                // other tab — that would either collapse it to zero height or
                // give it two nested scrollbars.
                let logs_tab = self.tab == Tab::Logs;
                div()
                    .id("body")
                    .flex_1()
                    .min_h_0()
                    .p_4()
                    .when(logs_tab, |d| d.flex().flex_col().overflow_hidden())
                    .when(!logs_tab, |d| {
                        d.overflow_y_scroll().on_scroll_wheel(cx.listener(|this, _, _, cx| {
                            this.close_menus_on_scroll(cx)
                        }))
                    })
                    .child(ui::views::body(self.tab, self, cx))
            })
            .child(ui::chrome::status_bar(self))
            .children(ui::views::confirm_modal(self, cx))
    }
}
