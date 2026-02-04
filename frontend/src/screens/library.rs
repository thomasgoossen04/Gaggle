use std::collections::{HashMap, HashSet};

use js_sys::{Function, Reflect};
use serde::Deserialize;
use serde_json;
use serde_wasm_bindgen;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api::{get_json, send_json};
use crate::app::AppState;
use crate::auth::{get_local_storage_item, set_local_storage_item, INSTALL_DIR_KEY};
use crate::components::Button;
use crate::net::build_http_url;
use crate::toast::{use_toast, ToastVariant};

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "core"])]
    async fn invoke(cmd: &str, args: JsValue) -> JsValue;
}

#[derive(Clone, PartialEq, Deserialize)]
struct AppInfo {
    id: String,
    name: String,
    description: String,
    version: String,
    archive_size: i64,
    has_archive: bool,
}

#[derive(Clone, PartialEq, Default)]
struct DownloadUiState {
    status: String,
    downloaded: u64,
    total: Option<u64>,
    speed_bps: f64,
    last_tick: Option<(f64, u64)>,
}

#[derive(Deserialize)]
struct DownloadEvent {
    id: String,
    downloaded: u64,
    total: Option<u64>,
    status: String,
    speed_bps: f64,
}

#[derive(serde::Serialize)]
struct StartDownloadArgs {
    id: String,
    archive_url: String,
    config_url: String,
    dest_dir: String,
    token: String,
}

#[function_component(LibraryScreen)]
pub fn library_screen() -> Html {
    let app_state = use_context::<UseStateHandle<AppState>>()
        .expect("AppState context not found. Ensure LibraryScreen is under <ContextProvider>.");
    let server_ip = app_state.server_ip.clone().unwrap_or_default();
    let server_port = app_state
        .server_port
        .clone()
        .unwrap_or_else(|| "2121".to_string());
    let token = app_state.session_token.clone().unwrap_or_default();
    let toast = use_toast();

    let apps = use_state(Vec::<AppInfo>::new);
    let loading = use_state(|| true);
    let error = use_state(|| None::<String>);
    let install_dir = use_state(String::new);
    let downloads = use_state(|| HashMap::<String, DownloadUiState>::new());
    let refresh_tick = use_state(|| 0u32);
    let installed = use_state(|| HashSet::<String>::new());

    {
        let install_dir = install_dir.clone();
        let toast = toast.clone();
        use_effect_with((), move |_| {
            spawn_local(async move {
                if let Some(saved) = get_local_storage_item(INSTALL_DIR_KEY) {
                    install_dir.set(saved);
                    return;
                }
                match invoke("get_default_apps_dir", JsValue::NULL).await.as_string() {
                    Some(path) => {
                        install_dir.set(path.clone());
                        set_local_storage_item(INSTALL_DIR_KEY, &path);
                    }
                    None => toast.toast("Failed to load default install folder.", ToastVariant::Error, Some(3000)),
                }
            });
            || ()
        });
    }

    {
        let server_ip = server_ip.clone();
        let server_port = server_port.clone();
        let apps = apps.clone();
        let loading = loading.clone();
        let error = error.clone();
        let refresh_tick = refresh_tick.clone();
        let install_dir = install_dir.clone();
        let installed = installed.clone();
        let token = token.clone();
        use_effect_with(
            (
                server_ip.clone(),
                server_port.clone(),
                token.clone(),
                *refresh_tick,
                (*install_dir).clone(),
            ),
            move |_| {
            if server_ip.is_empty() || server_port.is_empty() || token.is_empty() {
                loading.set(false);
                if token.is_empty() {
                    error.set(Some("Missing session token.".to_string()));
                }
                return ();
            }
            loading.set(true);
            let apps = apps.clone();
            let loading = loading.clone();
            let error = error.clone();
            let install_dir = install_dir.clone();
            let installed = installed.clone();
            let token = token.clone();
            spawn_local(async move {
                let url = build_http_url(&server_ip, &server_port, "apps");
                match get_json::<Vec<AppInfo>>(&url, Some(&token)).await {
                    Ok(list) => {
                        apps.set(list);
                        error.set(None);
                    }
                    Err(msg) => {
                        error.set(Some(msg));
                    }
                }
                if !install_dir.is_empty() {
                    let dest_dir: String = install_dir.as_str().to_string();
                    let payload = serde_wasm_bindgen::to_value(&serde_json::json!({
                        "request": {
                            "destDir": dest_dir
                        }
                    }))
                    .unwrap_or(JsValue::NULL);
                    let result = invoke("list_installed_apps", payload).await;
                    if let Ok(list) = serde_wasm_bindgen::from_value::<Vec<String>>(result) {
                        installed.set(list.into_iter().collect());
                    }
                }
                loading.set(false);
            });
            ()
        },
        );
    }

    let on_refresh = {
        let server_ip = server_ip.clone();
        let server_port = server_port.clone();
        let token = token.clone();
        let toast = toast.clone();
        let refresh_tick = refresh_tick.clone();
        Callback::from(move |_| {
            if server_ip.is_empty() || server_port.is_empty() || token.is_empty() {
                toast.toast("Missing server address or token.", ToastVariant::Warning, Some(2500));
                return;
            }
            let server_ip = server_ip.clone();
            let server_port = server_port.clone();
            let token = token.clone();
            let toast = toast.clone();
            let refresh_tick = refresh_tick.clone();
            spawn_local(async move {
                let url = build_http_url(&server_ip, &server_port, "apps/refresh");
                match send_json("POST", &url, Some(&token), None).await {
                    Ok(resp) if resp.ok() => {
                        refresh_tick.set(*refresh_tick + 1);
                    }
                    Ok(resp) => {
                        toast.toast(
                            format!("Refresh failed (HTTP {}).", resp.status()),
                            ToastVariant::Error,
                            Some(3000),
                        );
                    }
                    Err(msg) => {
                        toast.toast(msg, ToastVariant::Error, Some(3000));
                    }
                }
            });
        })
    };

    {
        let downloads = downloads.clone();
        let installed = installed.clone();
        let refresh_tick = refresh_tick.clone();
        let install_dir = install_dir.clone();
        use_effect_with(install_dir.clone(), move |install_dir| {
            let downloads = downloads.clone();
            let installed = installed.clone();
            let refresh_tick = refresh_tick.clone();
            let install_dir = (*install_dir).clone();
            spawn_local(async move {
                if !install_dir.is_empty() {
                    let dest_dir: String = install_dir.as_str().to_string();
                    let payload = serde_wasm_bindgen::to_value(&serde_json::json!({
                        "request": { "destDir": dest_dir }
                    }))
                    .unwrap_or(JsValue::NULL);
                    let initial = invoke("list_downloads", payload).await;
                    if let Ok(list) = serde_wasm_bindgen::from_value::<Vec<DownloadEvent>>(initial) {
                        let mut next = (*downloads).clone();
                        for item in list {
                            next.insert(
                                item.id.clone(),
                                DownloadUiState {
                                    status: item.status,
                                    downloaded: item.downloaded,
                                    total: item.total,
                                    speed_bps: 0.0,
                                    last_tick: None,
                                },
                            );
                        }
                        downloads.set(next);
                    }
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

                let callback = Closure::<dyn FnMut(JsValue)>::wrap(Box::new(move |value: JsValue| {
                    let payload = Reflect::get(&value, &JsValue::from_str("payload")).unwrap_or(JsValue::NULL);
                    let event: Result<DownloadEvent, _> = serde_wasm_bindgen::from_value(payload);
                    if let Ok(event) = event {
                        let mut next = (*downloads).clone();
                        let status = event.status.clone();
                        let mut entry = next.get(&event.id).cloned().unwrap_or_default();
                        let now = js_sys::Date::now();
                        if let Some((last_time, last_bytes)) = entry.last_tick {
                            let delta_t = (now - last_time) / 1000.0;
                            if delta_t > 0.0 {
                                let delta_b = event.downloaded.saturating_sub(last_bytes) as f64;
                                entry.speed_bps = delta_b / delta_t;
                            }
                        }
                        entry.last_tick = Some((now, event.downloaded));
                        entry.status = status.clone();
                        entry.downloaded = event.downloaded;
                        entry.total = event.total;
                        next.insert(event.id.clone(), entry);
                        downloads.set(next);
                        if status == "completed" {
                            let mut installed_next = (*installed).clone();
                            installed_next.insert(event.id.clone());
                            installed.set(installed_next);
                            refresh_tick.set(*refresh_tick + 1);
                        }
                    }
                }));

                let _ = listen_fn
                    .call2(&event, &JsValue::from_str("app_download_progress"), callback.as_ref().unchecked_ref());
                callback.forget();
            });
            || ()
        });
    }

    let on_download = {
        let server_ip = server_ip.clone();
        let server_port = server_port.clone();
        let install_dir = install_dir.clone();
        let downloads = downloads.clone();
        let token = token.clone();
        let toast = toast.clone();
        Callback::from(move |app: AppInfo| {
            if server_ip.is_empty() || server_port.is_empty() || token.is_empty() {
                toast.toast("Missing server address or token.", ToastVariant::Warning, Some(2500));
                return;
            }
            let dest_dir = (*install_dir).clone();
            if dest_dir.trim().is_empty() {
                toast.toast("Choose an install folder first.", ToastVariant::Warning, Some(2500));
                return;
            }
            if (*downloads).values().any(|d| d.status == "downloading" || d.status == "paused") {
                toast.toast(
                    "Finish or cancel the current download before starting another.",
                    ToastVariant::Warning,
                    Some(3000),
                );
                return;
            }

            let archive_url = build_http_url(&server_ip, &server_port, &format!("apps/{}/archive", app.id));
            let config_url = build_http_url(&server_ip, &server_port, &format!("apps/{}/config", app.id));
            let args = StartDownloadArgs {
                id: app.id.clone(),
                archive_url,
                config_url,
                dest_dir,
                token: token.clone(),
            };
            let downloads = downloads.clone();
            let toast = toast.clone();
            spawn_local(async move {
                let payload = serde_wasm_bindgen::to_value(&serde_json::json!({
                    "request": {
                        "id": args.id.clone(),
                        "archiveUrl": args.archive_url,
                        "configUrl": args.config_url,
                        "destDir": args.dest_dir,
                        "token": args.token
                    }
                }))
                    .unwrap_or(JsValue::NULL);
                let result = invoke("start_app_download", payload).await;
                if result.is_null() {
                    toast.toast("Failed to start download.", ToastVariant::Error, Some(3000));
                    return;
                }
                let mut next = (*downloads).clone();
                next.insert(
                    args.id.clone(),
                    DownloadUiState {
                        status: "downloading".to_string(),
                        downloaded: 0,
                        total: None,
                        speed_bps: 0.0,
                        last_tick: None,
                    },
                );
                downloads.set(next);
            });
        })
    };

    let on_remove = {
        let install_dir = install_dir.clone();
        let installed = installed.clone();
        let downloads = downloads.clone();
        Callback::from(move |id: String| {
            let install_dir = (*install_dir).clone();
            if install_dir.trim().is_empty() {
                toast.toast("No install folder configured.", ToastVariant::Warning, Some(2500));
                return;
            }
            let installed = installed.clone();
            let downloads = downloads.clone();
            spawn_local(async move {
                let payload = serde_wasm_bindgen::to_value(&serde_json::json!({
                    "request": {
                        "id": id.clone(),
                        "destDir": install_dir
                    }
                }))
                .unwrap_or(JsValue::NULL);
                let _ = invoke("remove_installed_app", payload).await;
                let mut next = (*installed).clone();
                next.remove(&id);
                installed.set(next);
                let mut downloads_next = (*downloads).clone();
                downloads_next.remove(&id);
                downloads.set(downloads_next);
            });
        })
    };

    let on_pause = {
        let downloads = downloads.clone();
        Callback::from(move |id: String| {
            let downloads = downloads.clone();
            spawn_local(async move {
                let payload = serde_wasm_bindgen::to_value(&serde_json::json!({ "id": id })).unwrap();
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
        let token = token.clone();
        Callback::from(move |id: String| {
            let downloads = downloads.clone();
            let install_dir = (*install_dir).clone();
            let token = token.clone();
            spawn_local(async move {
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
        let installed = installed.clone();
        Callback::from(move |id: String| {
            let downloads = downloads.clone();
            let installed = installed.clone();
            spawn_local(async move {
                let payload = serde_wasm_bindgen::to_value(&serde_json::json!({ "id": id })).unwrap();
                let _ = invoke("cancel_download", payload).await;
                let mut next = (*downloads).clone();
                next.remove(&id);
                downloads.set(next);
                let mut installed_next = (*installed).clone();
                installed_next.remove(&id);
                installed.set(installed_next);
            });
        })
    };

    html! {
        <div>
            <div class="flex items-center justify-between gap-4">
                <div>
                    <h1 class="text-2xl font-semibold">{ "Library" }</h1>
                    <p class="mt-2 text-sm text-accent">
                        { "Manage launchable apps, tools, and presets." }
                    </p>
                </div>
                <Button
                    class={Some("border border-ink/50 bg-ink/40 text-secondary hover:bg-ink/50".to_string())}
                    onclick={on_refresh}
                >
                    { "Refresh" }
                </Button>
            </div>
            if *loading {
                <div class="mt-6 rounded-2xl border border-ink/50 bg-inkLight p-6 text-sm text-secondary/70">
                    { "Loading apps..." }
                </div>
            } else if let Some(message) = (*error).clone() {
                <div class="mt-6 rounded-2xl border border-rose-400/60 bg-inkLight p-6 text-sm text-secondary/80">
                    { message }
                </div>
            } else {
                <div class="mt-8 grid gap-6 md:grid-cols-2 xl:grid-cols-3">
                    { for apps.iter().cloned().map(|app| {
                        let status = (*downloads).get(&app.id).cloned().unwrap_or_default();
                        let is_installed = (*installed).contains(&app.id);
                        let on_download = on_download.clone();
                        let on_pause = on_pause.clone();
                        let on_resume = on_resume.clone();
                        let on_cancel = on_cancel.clone();
                        let on_remove = on_remove.clone();
                        let app_id_pause = app.id.clone();
                        let app_id_resume = app.id.clone();
                        let app_id_cancel = app.id.clone();
                        let app_for_download = app.clone();
                        let app_for_remove = app.clone();
                        let is_busy = (*downloads)
                            .values()
                            .any(|d| d.status == "downloading" || d.status == "paused");
                        let action = if status.status == "downloading" {
                            html! {
                                <div class="flex items-center gap-2">
                                    <Button
                                        class={Some("border border-ink/50 bg-ink/40 text-secondary hover:bg-ink/50".to_string())}
                                        onclick={Callback::from(move |_| on_pause.emit(app_id_pause.clone()))}
                                    >
                                        { "Pause" }
                                    </Button>
                                    <Button
                                        class={Some("border border-rose-400/60 bg-rose-500/20 text-rose-100 hover:bg-rose-500/30".to_string())}
                                        onclick={Callback::from(move |_| on_cancel.emit(app_id_cancel.clone()))}
                                    >
                                        { "Cancel" }
                                    </Button>
                                </div>
                            }
                        } else if status.status == "paused" {
                            html! {
                                <div class="flex items-center gap-2">
                                    <Button
                                        class={Some("border border-accent/50 bg-accent/20 text-secondary hover:bg-accent/30".to_string())}
                                        onclick={Callback::from(move |_| on_resume.emit(app_id_resume.clone()))}
                                    >
                                        { "Resume" }
                                    </Button>
                                    <Button
                                        class={Some("border border-rose-400/60 bg-rose-500/20 text-rose-100 hover:bg-rose-500/30".to_string())}
                                        onclick={Callback::from(move |_| on_cancel.emit(app_id_cancel.clone()))}
                                    >
                                        { "Cancel" }
                                    </Button>
                                </div>
                            }
                        } else if is_installed || status.status == "completed" {
                            html! {
                                <Button
                                    class={Some("border border-rose-400/60 bg-rose-500/20 text-rose-100 hover:bg-rose-500/30".to_string())}
                                    onclick={Callback::from(move |_| on_remove.emit(app_for_remove.id.clone()))}
                                >
                                    { "Remove" }
                                </Button>
                            }
                        } else {
                            html! {
                                <Button
                                    class={Some("border border-primary/60 bg-primary/30 text-secondary hover:bg-primary/40".to_string())}
                                    onclick={Callback::from(move |_| on_download.emit(app_for_download.clone()))}
                                    disabled={!app.has_archive || is_busy}
                                >
                                    { if !app.has_archive { "Unavailable" } else if is_busy { "Busy" } else { "Download" } }
                                </Button>
                            }
                        };

                        let progress = if let Some(total) = status.total {
                            let pct = if total > 0 {
                                (status.downloaded as f64 / total as f64) * 100.0
                            } else {
                                0.0
                            };
                            format!("{:.0}%", pct.min(100.0))
                        } else {
                            String::new()
                        };
                        let speed = if status.speed_bps > 0.0 {
                            format!("{}/s", format_size(status.speed_bps as i64))
                        } else {
                            String::new()
                        };

                        html! {
                            <div class="rounded-3xl border-2 border-ink/40 bg-ink/30 p-1 shadow-xl">
                                <div class="rounded-2xl border border-ink/50 bg-inkLight p-6">
                                <p class="text-xs uppercase tracking-wide text-accent/80">{ "App" }</p>
                                <p class="mt-4 text-lg font-semibold">{ app.name.clone() }</p>
                                <p class="mt-2 text-sm text-secondary/70">{ app.description.clone() }</p>
                                <div class="mt-4 flex items-center justify-between text-xs text-secondary/60">
                                    <span>{ format!("Version {}", if app.version.is_empty() { "—" } else { &app.version }) }</span>
                                    <span>{ format_size(app.archive_size) }</span>
                                </div>
                                <div class="mt-4 flex items-center justify-between">
                                    { action }
                                    if !progress.is_empty() {
                                        <span class="text-xs text-secondary/60">
                                            { if speed.is_empty() { progress } else { format!("{progress} · {speed}") } }
                                        </span>
                                    }
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
        return "—".to_string();
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
