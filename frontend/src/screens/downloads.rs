use std::collections::HashMap;

use js_sys::{Function, Reflect};
use serde::Deserialize;
use serde_json;
use serde_wasm_bindgen;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::auth::{get_local_storage_item, INSTALL_DIR_KEY, SESSION_TOKEN_KEY};
use crate::components::DownloadSpeedGraph;
use crate::components::{Button, IndeterminateBar};
use crate::toast::{use_toast, ToastVariant};

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "core"])]
    async fn invoke(cmd: &str, args: JsValue) -> JsValue;
}

#[derive(Clone, PartialEq, Default)]
struct DownloadView {
    status: String,
    name: String,
    downloaded: u64,
    total: Option<u64>,
    speeds: Vec<f64>,
    speed_points: Vec<(f64, f64)>, // (timestamp_secs, speed)
    last_tick: Option<(f64, u64)>,
    logical_time: f64,
}

#[derive(Deserialize)]
struct DownloadEvent {
    id: String,
    name: String,
    downloaded: u64,
    total: Option<u64>,
    status: String,
    speed_bps: f64,
}

#[derive(Deserialize)]
struct DownloadSnapshot {
    id: String,
    name: String,
    downloaded: u64,
    total: Option<u64>,
    status: String,
    speed_bps: f64,
}

fn now_secs() -> f64 {
    js_sys::Date::now() / 1000.0
}

#[function_component(DownloadsScreen)]
pub fn downloads_screen() -> Html {
    let downloads = use_state(|| HashMap::<String, DownloadView>::new());
    let install_dir = use_state(|| get_local_storage_item(INSTALL_DIR_KEY).unwrap_or_default());
    let session_token = use_state(|| get_local_storage_item(SESSION_TOKEN_KEY).unwrap_or_default());
    let toast = use_toast();

    let on_pause = {
        let downloads = downloads.clone();
        Callback::from(move |id: String| {
            let downloads = downloads.clone();
            spawn_local(async move {
                let payload =
                    serde_wasm_bindgen::to_value(&serde_json::json!({ "id": id })).unwrap();
                let _ = invoke("pause_download", payload).await;
                let mut next = (*downloads).clone();
                if let Some(entry) = next.get_mut(&id) {
                    entry.status = "paused".to_string();
                }
                downloads.set(next);
            });
        })
    };

    let on_resume = {
        let downloads = downloads.clone();
        let install_dir = install_dir.clone();
        let session_token = session_token.clone();
        let toast = toast.clone();
        Callback::from(move |id: String| {
            let downloads = downloads.clone();
            let install_dir = (*install_dir).clone();
            let token = (*session_token).clone();
            let toast = toast.clone();
            spawn_local(async move {
                if token.trim().is_empty() {
                    toast.toast("Missing session token.", ToastVariant::Warning, Some(2500));
                    return;
                }
                let payload = serde_wasm_bindgen::to_value(&serde_json::json!({
                    "request": {
                        "id": id.clone(),
                        "destDir": install_dir,
                        "token": token
                    }
                }))
                .unwrap();
                let _ = invoke("resume_download", payload).await;
                let mut next = (*downloads).clone();
                if let Some(entry) = next.get_mut(&id) {
                    entry.status = "downloading".to_string();
                }
                downloads.set(next);
            });
        })
    };

    let on_cancel = {
        let downloads = downloads.clone();
        Callback::from(move |id: String| {
            let downloads = downloads.clone();
            spawn_local(async move {
                let payload =
                    serde_wasm_bindgen::to_value(&serde_json::json!({ "id": id })).unwrap();
                let _ = invoke("cancel_download", payload).await;
                let mut next = (*downloads).clone();
                next.remove(&id);
                downloads.set(next);
            });
        })
    };

    {
        let downloads = downloads.clone();
        let install_dir = install_dir.clone();
        let toast = toast.clone();
        use_effect_with((), move |_| {
            let downloads = downloads.clone();
            let install_dir = install_dir.clone();
            let toast = toast.clone();
            spawn_local(async move {
                let mut dir = (*install_dir).clone();
                if dir.is_empty() {
                    if let Some(path) = invoke("get_default_apps_dir", JsValue::NULL)
                        .await
                        .as_string()
                    {
                        dir = path;
                        install_dir.set(dir.clone());
                    } else {
                        toast.toast(
                            "Failed to load install folder.",
                            ToastVariant::Error,
                            Some(3000),
                        );
                        return;
                    }
                }
                let payload = serde_wasm_bindgen::to_value(&serde_json::json!({
                    "request": { "destDir": dir }
                }))
                .unwrap_or(JsValue::NULL);
                let initial = invoke("list_downloads", payload).await;
                if let Ok(list) = serde_wasm_bindgen::from_value::<Vec<DownloadSnapshot>>(initial) {
                    let mut next = (*downloads).clone();
                    for snapshot in list {
                        let view = next
                            .entry(snapshot.id.clone())
                            .or_insert_with(DownloadView::default);
                        view.status = snapshot.status;
                        view.name = snapshot.name;
                        view.downloaded = snapshot.downloaded;
                        view.total = snapshot.total;
                        if snapshot.speed_bps > 0.0 {
                            view.speeds.push(snapshot.speed_bps);
                            if view.speeds.len() > 60 {
                                view.speeds.remove(0);
                            }
                        }
                        view.last_tick = Some((js_sys::Date::now(), snapshot.downloaded));
                    }
                    downloads.set(next);
                }

                let window = web_sys::window().unwrap();
                let tauri = Reflect::get(&window, &JsValue::from_str("__TAURI__"));
                if tauri.is_err() {
                    return;
                }
                let tauri = tauri.unwrap();
                let event = Reflect::get(&tauri, &JsValue::from_str("event"));
                if event.is_err() {
                    return;
                }
                let event = event.unwrap();
                let listen = Reflect::get(&event, &JsValue::from_str("listen"));
                if listen.is_err() {
                    return;
                }
                let listen = listen.unwrap();
                let listen_fn: Function = listen.dyn_into().unwrap();

                let callback =
                    Closure::<dyn FnMut(JsValue)>::wrap(Box::new(move |value: JsValue| {
                        let payload = Reflect::get(&value, &JsValue::from_str("payload"))
                            .unwrap_or(JsValue::NULL);
                        let event: Result<DownloadEvent, _> =
                            serde_wasm_bindgen::from_value(payload);
                        if let Ok(event) = event {
                            let mut next = (*downloads).clone();
                            if event.status == "cancelled" || event.status == "completed" {
                                next.remove(&event.id);
                                downloads.set(next);
                                return;
                            }
                            let view = next
                                .entry(event.id.clone())
                                .or_insert_with(DownloadView::default);
                            let now = js_sys::Date::now();
                            let mut speed = event.speed_bps;
                            if speed <= 0.0 {
                                if let Some((last_time, last_bytes)) = view.last_tick {
                                    let delta_t = (now - last_time) / 1000.0;
                                    if delta_t > 0.0 {
                                        let delta_b =
                                            event.downloaded.saturating_sub(last_bytes) as f64;
                                        speed = delta_b / delta_t;
                                    }
                                }
                            }
                            if speed > 0.0 {
                                view.speeds.push(speed);
                                if view.speeds.len() > 60 {
                                    view.speeds.remove(0);
                                }

                                let now = now_secs();
                                view.speed_points.push((now, speed));
                                let cutoff = now - 10.0;
                                view.speed_points.retain(|(t, _)| *t >= cutoff);
                            }
                            view.last_tick = Some((now, event.downloaded));
                            view.name = event.name;
                            view.status = event.status;
                            view.downloaded = event.downloaded;
                            view.total = event.total;
                            downloads.set(next);
                        }
                    }));

                let _ = listen_fn.call2(
                    &event,
                    &JsValue::from_str("app_download_progress"),
                    callback.as_ref().unchecked_ref(),
                );
                callback.forget();
            });
            || ()
        });
    }

    // Live updates come from the download events; list_downloads only seeds on mount.

    html! {
        <div>
            <h1 class="text-2xl font-semibold">{ "Downloads" }</h1>
            <p class="mt-2 text-sm text-accent">
                { "Track in-progress downloads and speed." }
            </p>
            if downloads.is_empty() {
                <div class="mt-6 rounded-2xl border border-ink/50 bg-inkLight p-6 text-sm text-secondary/70">
                    { "No active downloads." }
                </div>
            } else {
                <div class="mt-6 flex flex-col gap-6">
                    { for {
                        let mut items: Vec<_> = downloads.iter().collect();
                        items.sort_by(|(a, _), (b, _)| a.cmp(b));
                        items
                    }.into_iter().map(|(id, view)| {
                        let total = view.total.unwrap_or(0);
                        let progress = if total == 0 { 0.0 } else { (view.downloaded as f64 / total as f64) * 100.0 };
                        let current_speed = view.speeds.last().cloned().unwrap_or(0.0);
                        let avg_speed = if view.speeds.is_empty() {
                            0.0
                        } else {
                            view.speeds.iter().sum::<f64>() / view.speeds.len() as f64
                        };
                        let eta = if current_speed > 0.0 && total > 0 {
                            let remaining = total.saturating_sub(view.downloaded) as f64;
                            Some(remaining / current_speed)
                        } else {
                            None
                        };
                        let on_pause = on_pause.clone();
                        let on_resume = on_resume.clone();
                        let on_cancel = on_cancel.clone();
                        let download_id_pause = id.clone();
                        let download_id_resume = id.clone();
                        let download_id_cancel = id.clone();
                        let action_row = if view.status == "paused" {
                            html! {
                                <div class="flex items-center gap-2">
                                    <Button
                                        class={Some("border border-accent/50 bg-accent/20 text-secondary hover:bg-accent/30".to_string())}
                                        onclick={Callback::from(move |_| on_resume.emit(download_id_resume.clone()))}
                                    >
                                        { "Resume" }
                                    </Button>
                                    <Button
                                        class={Some("border border-rose-400/60 bg-rose-500/20 text-rose-100 hover:bg-rose-500/30".to_string())}
                                        onclick={Callback::from(move |_| on_cancel.emit(download_id_cancel.clone()))}
                                    >
                                        { "Cancel" }
                                    </Button>
                                </div>
                            }
                        } else {
                            html! {
                                <div class="flex items-center gap-2">
                                    <Button
                                        class={Some("border border-ink/50 bg-ink/40 text-secondary hover:bg-ink/50".to_string())}
                                        onclick={Callback::from(move |_| on_pause.emit(download_id_pause.clone()))}
                                    >
                                        { "Pause" }
                                    </Button>
                                    <Button
                                        class={Some("border border-rose-400/60 bg-rose-500/20 text-rose-100 hover:bg-rose-500/30".to_string())}
                                        onclick={Callback::from(move |_| on_cancel.emit(download_id_cancel.clone()))}
                                    >
                                        { "Cancel" }
                                    </Button>
                                </div>
                            }
                        };
                        html! {
                            <div key={id.clone()} class="w-full rounded-3xl border-2 border-ink/40 bg-ink/30 p-1 shadow-xl">
                                <div class="rounded-2xl border border-ink/50 bg-inkLight p-6">
                                    <div class="flex flex-wrap items-center justify-between gap-4">
                                        <div>
                                            <p class="text-xs uppercase tracking-wide text-accent/80">{ "Download" }</p>
                                            <p class="mt-2 text-xl font-semibold">{ view.name.clone() }</p>
                                            <p class="mt-1 text-sm text-secondary/70">
                                                { format!("{} - {}", format_size(view.downloaded as i64), format_size(total as i64)) }
                                            </p>
                                        </div>
                                        <div class="text-right">
                                            <p class="text-sm text-secondary/70">{ "Progress" }</p>
                                            <p class="text-2xl font-semibold">{ format!("{:.0}%", progress.min(100.0)) }</p>
                                        </div>
                                    </div>
                                    <div class="mt-5">
                                        <DownloadSpeedGraph
                                            points={view.speed_points.clone()}
                                            paused={view.status=="paused"}
                                        />

                                        if view.status == "installing" {
                                            <div class="mt-3">
                                                <p class="mb-2 text-xs uppercase tracking-wide text-accent/70">{ "Installing" }</p>
                                                <IndeterminateBar />
                                            </div>
                                        }
                                    </div>
                                    <div class="mt-4 grid gap-3 md:grid-cols-4">
                                        <div class="rounded-xl border border-ink/50 bg-ink/40 px-4 py-3">
                                            <p class="text-xs uppercase tracking-wide text-accent/70">{ "Status" }</p>
                                            <p class="mt-1 text-sm text-secondary/90">
                                                { if view.status.is_empty() { "unknown" } else { &view.status } }
                                            </p>
                                        </div>
                                        <div class="rounded-xl border border-ink/50 bg-ink/40 px-4 py-3">
                                            <p class="text-xs uppercase tracking-wide text-accent/70">{ "Current" }</p>
                                            <p class="mt-1 text-sm text-secondary/90">
                                                { if current_speed > 0.0 {
                                                    format!("{}/s", format_size(current_speed as i64))
                                                } else { "-".to_string() } }
                                            </p>
                                        </div>
                                        <div class="rounded-xl border border-ink/50 bg-ink/40 px-4 py-3">
                                            <p class="text-xs uppercase tracking-wide text-accent/70">{ "Average" }</p>
                                            <p class="mt-1 text-sm text-secondary/90">
                                                { if avg_speed > 0.0 {
                                                    format!("{}/s", format_size(avg_speed as i64))
                                                } else { "-".to_string() } }
                                            </p>
                                        </div>
                                        <div class="rounded-xl border border-ink/50 bg-ink/40 px-4 py-3">
                                            <p class="text-xs uppercase tracking-wide text-accent/70">{ "ETA" }</p>
                                            <p class="mt-1 text-sm text-secondary/90">
                                                { eta.map(format_duration).unwrap_or_else(|| "-".to_string()) }
                                            </p>
                                        </div>
                                    </div>
                                    <div class="mt-4">
                                        { action_row }
                                    </div>
                                </div>
                            </div>
                        }
                    }) }
                </div>
            }
        </div>
    }
}

fn format_size(size: i64) -> String {
    if size <= 0 {
        return "-".to_string();
    }
    let size = size as f64;
    let units = ["B", "KB", "MB", "GB"];
    let mut value = size;
    let mut unit = units[0];
    for next in units.iter().skip(1) {
        if value < 1024.0 {
            break;
        }
        value /= 1024.0;
        unit = next;
    }
    format!("{:.1} {}", value, unit)
}

fn format_duration(seconds: f64) -> String {
    if !seconds.is_finite() || seconds <= 0.0 {
        return "-".to_string();
    }
    let total = seconds.round() as i64;
    let hrs = total / 3600;
    let mins = (total % 3600) / 60;
    let secs = total % 60;
    if hrs > 0 {
        format!("{hrs}h {mins}m")
    } else if mins > 0 {
        format!("{mins}m {secs}s")
    } else {
        format!("{secs}s")
    }
}
