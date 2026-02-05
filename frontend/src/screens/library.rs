use std::collections::{HashMap, HashSet};

use js_sys::{Function, Reflect};
use serde::Deserialize;
use serde_json;
use serde_wasm_bindgen;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};
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

    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "core"], js_name = invoke, catch)]
    async fn invoke_safe(cmd: &str, args: JsValue) -> Result<JsValue, JsValue>;
}

#[derive(Clone, PartialEq, Deserialize)]
struct AppInfo {
    id: String,
    name: String,
    description: String,
    version: String,
    archive_size: i64,
    has_archive: bool,
    #[serde(default)]
    executable: Option<String>,
}

#[derive(Clone, PartialEq, Deserialize)]
struct PlaytimeEntry {
    app_id: String,
    total_seconds: i64,
    last_played: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunAppResult {
    duration_seconds: u64,
    _exit_code: Option<i32>,
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
    #[serde(default)]
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
    let playtime = use_state(|| HashMap::<String, PlaytimeEntry>::new());
    let search = use_state(String::new);
    let sort = use_state(|| "downloaded".to_string());
    let carousel_ref = use_node_ref();
    let on_carousel_wheel = {
        let carousel_ref = carousel_ref.clone();
        Callback::from(move |event: WheelEvent| {
            event.prevent_default();
            if let Some(element) = carousel_ref.cast::<web_sys::HtmlElement>() {
                let delta = if event.delta_y().abs() > event.delta_x().abs() {
                    event.delta_y()
                } else {
                    event.delta_x()
                };
                let _ = element.scroll_by_with_x_and_y(delta, 0.0);
            }
        })
    };

    {
        let install_dir = install_dir.clone();
        let toast = toast.clone();
        use_effect_with((), move |_| {
            spawn_local(async move {
                if let Some(saved) = get_local_storage_item(INSTALL_DIR_KEY) {
                    install_dir.set(saved);
                    return;
                }
                match invoke("get_default_apps_dir", JsValue::NULL)
                    .await
                    .as_string()
                {
                    Some(path) => {
                        install_dir.set(path.clone());
                        set_local_storage_item(INSTALL_DIR_KEY, &path);
                    }
                    None => toast.toast(
                        "Failed to load default install folder.",
                        ToastVariant::Error,
                        Some(3000),
                    ),
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
        let playtime = playtime.clone();
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
                let playtime = playtime.clone();
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
                    let playtime_url = build_http_url(&server_ip, &server_port, "apps/playtime");
                    if let Ok(list) =
                        get_json::<Vec<PlaytimeEntry>>(&playtime_url, Some(&token)).await
                    {
                        let mut next = HashMap::new();
                        for entry in list {
                            next.insert(entry.app_id.clone(), entry);
                        }
                        playtime.set(next);
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
                toast.toast(
                    "Missing server address or token.",
                    ToastVariant::Warning,
                    Some(2500),
                );
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
        let server_ip = server_ip.clone();
        let server_port = server_port.clone();
        let token = token.clone();
        let refresh_tick = refresh_tick.clone();
        use_effect_with(
            (server_ip.clone(), server_port.clone(), token.clone()),
            move |_| {
                if server_ip.is_empty() || server_port.is_empty() || token.is_empty() {
                    return ();
                }
                let server_ip = server_ip.clone();
                let server_port = server_port.clone();
                let token = token.clone();
                let refresh_tick = refresh_tick.clone();
                spawn_local(async move {
                    let url = build_http_url(&server_ip, &server_port, "apps/refresh");
                    if let Ok(resp) = send_json("POST", &url, Some(&token), None).await {
                        if resp.ok() {
                            refresh_tick.set(*refresh_tick + 1);
                        }
                    }
                });
                ()
            },
        );
    }

    let on_search_input = {
        let search = search.clone();
        Callback::from(move |event: InputEvent| {
            let input: web_sys::HtmlInputElement = event.target_unchecked_into();
            search.set(input.value());
        })
    };

    let on_sort_change = {
        let sort = sort.clone();
        Callback::from(move |event: Event| {
            let input: web_sys::HtmlSelectElement = event.target_unchecked_into();
            sort.set(input.value());
        })
    };

    let on_scroll_prev = {
        let carousel_ref = carousel_ref.clone();
        Callback::from(move |_| {
            if let Some(element) = carousel_ref.cast::<web_sys::HtmlElement>() {
                let width = element.client_width() as f64;
                let _ = element.scroll_by_with_x_and_y(-width, 0.0);
            }
        })
    };

    let on_scroll_next = {
        let carousel_ref = carousel_ref.clone();
        Callback::from(move |_| {
            if let Some(element) = carousel_ref.cast::<web_sys::HtmlElement>() {
                let width = element.client_width() as f64;
                let _ = element.scroll_by_with_x_and_y(width, 0.0);
            }
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
                    if let Ok(list) = serde_wasm_bindgen::from_value::<Vec<DownloadEvent>>(initial)
                    {
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

                let callback =
                    Closure::<dyn FnMut(JsValue)>::wrap(Box::new(move |value: JsValue| {
                        let payload = Reflect::get(&value, &JsValue::from_str("payload"))
                            .unwrap_or(JsValue::NULL);
                        let event: Result<DownloadEvent, _> =
                            serde_wasm_bindgen::from_value(payload);
                        if let Ok(event) = event {
                            let mut next = (*downloads).clone();
                            let status = event.status.clone();
                            let mut entry = next.get(&event.id).cloned().unwrap_or_default();
                            let now = js_sys::Date::now();
                            let mut speed = event.speed_bps;
                            if speed <= 0.0 {
                                if let Some((last_time, last_bytes)) = entry.last_tick {
                                    let delta_t = (now - last_time) / 1000.0;
                                    if delta_t > 0.0 {
                                        let delta_b =
                                            event.downloaded.saturating_sub(last_bytes) as f64;
                                        speed = delta_b / delta_t;
                                    }
                                }
                            }
                            if speed > 0.0 {
                                entry.speed_bps = speed;
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

    let on_download = {
        let server_ip = server_ip.clone();
        let server_port = server_port.clone();
        let install_dir = install_dir.clone();
        let downloads = downloads.clone();
        let token = token.clone();
        let toast = toast.clone();
        Callback::from(move |app: AppInfo| {
            if server_ip.is_empty() || server_port.is_empty() || token.is_empty() {
                toast.toast(
                    "Missing server address or token.",
                    ToastVariant::Warning,
                    Some(2500),
                );
                return;
            }
            let dest_dir = (*install_dir).clone();
            if dest_dir.trim().is_empty() {
                toast.toast(
                    "Choose an install folder first.",
                    ToastVariant::Warning,
                    Some(2500),
                );
                return;
            }
            if (*downloads)
                .values()
                .any(|d| d.status == "downloading" || d.status == "paused")
            {
                toast.toast(
                    "Finish or cancel the current download before starting another.",
                    ToastVariant::Warning,
                    Some(3000),
                );
                return;
            }

            let archive_url = build_http_url(
                &server_ip,
                &server_port,
                &format!("apps/{}/archive", app.id),
            );
            let config_url =
                build_http_url(&server_ip, &server_port, &format!("apps/{}/config", app.id));
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
        let toast = toast.clone();
        Callback::from(move |id: String| {
            let install_dir = (*install_dir).clone();
            if install_dir.trim().is_empty() {
                toast.toast(
                    "No install folder configured.",
                    ToastVariant::Warning,
                    Some(2500),
                );
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
                let payload =
                    serde_wasm_bindgen::to_value(&serde_json::json!({ "id": id })).unwrap();
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

    let on_open_folder = {
        let install_dir = install_dir.clone();
        let toast = toast.clone();
        Callback::from(move |id: String| {
            let install_dir = (*install_dir).clone();
            let toast = toast.clone();
            if install_dir.trim().is_empty() {
                toast.toast(
                    "No install folder configured.",
                    ToastVariant::Warning,
                    Some(2500),
                );
                return;
            }
            spawn_local(async move {
                let payload = serde_wasm_bindgen::to_value(&serde_json::json!({
                    "request": {
                        "id": id,
                        "destDir": install_dir
                    }
                }))
                .unwrap_or(JsValue::NULL);
                let _ = invoke("open_app_folder", payload).await;
            });
        })
    };

    let on_run_app = {
        let install_dir = install_dir.clone();
        let toast = toast.clone();
        let server_ip = server_ip.clone();
        let server_port = server_port.clone();
        let token = token.clone();
        let playtime = playtime.clone();
        Callback::from(move |(id, executable, name): (String, String, String)| {
            let install_dir = (*install_dir).clone();
            let toast = toast.clone();
            let server_ip = server_ip.clone();
            let server_port = server_port.clone();
            let token = token.clone();
            let playtime = playtime.clone();
            let app_name = name.clone();
            if install_dir.trim().is_empty() {
                toast.toast(
                    "No install folder configured.",
                    ToastVariant::Warning,
                    Some(2500),
                );
                return;
            }
            if executable.trim().is_empty() {
                toast.toast(
                    "Executable not configured for this app.",
                    ToastVariant::Warning,
                    Some(2500),
                );
                return;
            }
            spawn_local(async move {
                let app_id = id.clone();
                if !server_ip.trim().is_empty()
                    && !server_port.trim().is_empty()
                    && !token.trim().is_empty()
                {
                    let status_url =
                        build_http_url(server_ip.trim(), server_port.trim(), "social/status");
                    let body = serde_json::json!({
                        "status": "playing",
                        "app_id": app_id,
                        "app_name": app_name
                    });
                    let _ = send_json("POST", &status_url, Some(token.trim()), Some(body)).await;
                }
                let payload = serde_wasm_bindgen::to_value(&serde_json::json!({
                    "request": {
                        "id": id,
                        "destDir": install_dir,
                        "executable": executable
                    }
                }))
                .unwrap_or(JsValue::NULL);
                let result = match invoke_safe("run_app_executable_tracked", payload).await {
                    Ok(value) => value,
                    Err(err) => {
                        let message = err
                            .as_string()
                            .unwrap_or_else(|| "Failed to launch executable.".to_string());
                        toast.toast(&message, ToastVariant::Error, Some(3000));
                        if !server_ip.trim().is_empty()
                            && !server_port.trim().is_empty()
                            && !token.trim().is_empty()
                        {
                            let status_url = build_http_url(
                                server_ip.trim(),
                                server_port.trim(),
                                "social/status",
                            );
                            let body = serde_json::json!({ "status": "online" });
                            let _ = send_json("POST", &status_url, Some(token.trim()), Some(body))
                                .await;
                        }
                        return;
                    }
                };
                let run: RunAppResult = match serde_wasm_bindgen::from_value(result) {
                    Ok(value) => value,
                    Err(_) => {
                        toast.toast(
                            "Failed to read play session.",
                            ToastVariant::Error,
                            Some(3000),
                        );
                        if !server_ip.trim().is_empty()
                            && !server_port.trim().is_empty()
                            && !token.trim().is_empty()
                        {
                            let status_url = build_http_url(
                                server_ip.trim(),
                                server_port.trim(),
                                "social/status",
                            );
                            let body = serde_json::json!({ "status": "online" });
                            let _ = send_json("POST", &status_url, Some(token.trim()), Some(body))
                                .await;
                        }
                        return;
                    }
                };
                let seconds = run.duration_seconds as i64;
                if seconds <= 0 {
                    if !server_ip.trim().is_empty()
                        && !server_port.trim().is_empty()
                        && !token.trim().is_empty()
                    {
                        let status_url =
                            build_http_url(server_ip.trim(), server_port.trim(), "social/status");
                        let body = serde_json::json!({ "status": "online" });
                        let _ =
                            send_json("POST", &status_url, Some(token.trim()), Some(body)).await;
                    }
                    return;
                }
                if server_ip.trim().is_empty()
                    || server_port.trim().is_empty()
                    || token.trim().is_empty()
                {
                    return;
                }
                let url = build_http_url(
                    server_ip.trim(),
                    server_port.trim(),
                    &format!("apps/{}/playtime", id),
                );
                let body = serde_json::json!({ "seconds": seconds });
                if let Ok(resp) = send_json("POST", &url, Some(token.trim()), Some(body)).await {
                    if resp.ok() {
                        if let Ok(promise) = resp.json() {
                            if let Ok(json) = JsFuture::from(promise).await {
                                if let Ok(entry) =
                                    serde_wasm_bindgen::from_value::<PlaytimeEntry>(json)
                                {
                                    let mut next = (*playtime).clone();
                                    next.insert(entry.app_id.clone(), entry);
                                    playtime.set(next);
                                }
                            }
                        }
                    } else {
                        toast.toast("Playtime upload failed.", ToastVariant::Warning, Some(2500));
                    }
                }
                let status_url =
                    build_http_url(server_ip.trim(), server_port.trim(), "social/status");
                let body = serde_json::json!({ "status": "online" });
                let _ = send_json("POST", &status_url, Some(token.trim()), Some(body)).await;
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
            <div class="mt-6 flex flex-wrap items-center gap-3">
                <div class="flex-1 min-w-[220px]">
                    <input
                        class="w-full rounded border border-ink/50 bg-ink/50 px-4 py-2 text-secondary placeholder:text-secondary/60 outline outline-1 outline-accent/50 focus:outline-none focus:ring-2 focus:ring-primary/40"
                        type="text"
                        placeholder="Search apps..."
                        value={(*search).clone()}
                        oninput={on_search_input}
                    />
                </div>
                <div class="min-w-[200px]">
                    <select
                        class="w-full rounded border border-ink/50 bg-ink/50 px-4 py-2 text-secondary outline outline-1 outline-accent/50 focus:outline-none focus:ring-2 focus:ring-primary/40"
                        value={(*sort).clone()}
                        onchange={on_sort_change}
                    >
                        <option value="downloaded">{ "Downloaded first" }</option>
                        <option value="name">{ "Name (A-Z)" }</option>
                        <option value="size">{ "Size (largest)" }</option>
                    </select>
                </div>
                <div class="flex items-center gap-2">
                    <Button
                        class={Some("border border-ink/50 bg-ink/40 text-secondary hover:bg-ink/50".to_string())}
                        onclick={on_scroll_prev}
                    >
                        { "Prev" }
                    </Button>
                    <Button
                        class={Some("border border-ink/50 bg-ink/40 text-secondary hover:bg-ink/50".to_string())}
                        onclick={on_scroll_next}
                    >
                        { "Next" }
                    </Button>
                </div>
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
                <div
                    ref={carousel_ref.clone()}
                    onwheel={on_carousel_wheel}
                    class="mt-8 flex gap-6 overflow-x-auto pb-4 snap-x snap-mandatory scrollbar-thin"
                >
                    { for {
                        let mut items: Vec<_> = apps
                            .iter()
                            .cloned()
                            .filter(|app| {
                                let query = (*search).trim().to_lowercase();
                                if query.is_empty() {
                                    return true;
                                }
                                let hay = format!("{} {} {}", app.name, app.description, app.id).to_lowercase();
                                hay.contains(&query)
                            })
                            .collect();
                        match (*sort).as_str() {
                            "name" => items.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
                            "size" => items.sort_by(|a, b| b.archive_size.cmp(&a.archive_size)),
                            _ => items.sort_by(|a, b| {
                                let a_installed = (*installed).contains(&a.id)
                                    || (*downloads).get(&a.id).map(|d| d.status == "completed").unwrap_or(false);
                                let b_installed = (*installed).contains(&b.id)
                                    || (*downloads).get(&b.id).map(|d| d.status == "completed").unwrap_or(false);
                                b_installed.cmp(&a_installed).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                            }),
                        }
                        items
                    }.into_iter().map(|app| {
                        let status = (*downloads).get(&app.id).cloned().unwrap_or_default();
                        let is_installed = (*installed).contains(&app.id)
                            || status.status == "completed";
                        let on_download = on_download.clone();
                        let on_pause = on_pause.clone();
                        let on_resume = on_resume.clone();
                        let on_cancel = on_cancel.clone();
                        let on_remove = on_remove.clone();
                        let on_open_folder = on_open_folder.clone();
                        let on_run_app = on_run_app.clone();
                        let app_id_pause = app.id.clone();
                        let app_id_resume = app.id.clone();
                        let app_id_cancel = app.id.clone();
                        let app_for_download = app.clone();
                        let app_for_remove = app.clone();
                        let app_for_open = app.clone();
                        let app_for_run = app.clone();
                        let playtime_label = (*playtime)
                            .get(&app.id)
                            .map(|entry| format_playtime(entry.total_seconds))
                            .unwrap_or_else(|| "Not played yet".to_string());
                        let is_busy = (*downloads)
                            .values()
                            .any(|d| d.status == "downloading" || d.status == "paused");
                        let has_exec = app
                            .executable
                            .as_ref()
                            .map(|value| !value.trim().is_empty())
                            .unwrap_or(false);
                        let action = if status.status == "installing" {
                            html! {
                                <div class="flex items-center gap-2">
                                    <Button
                                        class={Some("border border-ink/50 bg-ink/40 text-secondary hover:bg-ink/50".to_string())}
                                        onclick={Callback::from(|_| {})}
                                        disabled={true}
                                    >
                                        { "Installing..." }
                                    </Button>
                                </div>
                            }
                        } else if status.status == "downloading" {
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
                        } else if is_installed {
                            let primary = if has_exec {
                                html! {
                                    <Button
                                        class={Some("border border-accent/50 bg-accent/20 text-secondary hover:bg-accent/30".to_string())}
                                        onclick={Callback::from(move |_| on_run_app.emit((app_for_run.id.clone(), app_for_run.executable.clone().unwrap_or_default(), app_for_run.name.clone())))}
                                    >
                                        { "Run" }
                                    </Button>
                                }
                            } else {
                                html! {
                                    <Button
                                        class={Some("border border-ink/50 bg-ink/40 text-secondary hover:bg-ink/50".to_string())}
                                        onclick={Callback::from(move |_| on_open_folder.emit(app_for_open.id.clone()))}
                                    >
                                        { "Open Folder" }
                                    </Button>
                                }
                            };
                            html! {
                                <div class="flex items-center gap-2">
                                    { primary }
                                    <Button
                                        class={Some("border border-rose-400/60 bg-rose-500/20 text-rose-100 hover:bg-rose-500/30".to_string())}
                                        onclick={Callback::from(move |_| on_remove.emit(app_for_remove.id.clone()))}
                                    >
                                        { "Remove" }
                                    </Button>
                                </div>
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
                        let status_label = if status.status == "installing" {
                            "Installing"
                        } else if status.status == "downloading" {
                            "Downloading"
                        } else if status.status == "paused" {
                            "Paused"
                        } else {
                            ""
                        };

                        html! {
                            <div key={app.id.clone()} class="snap-start shrink-0 w-[min(92vw,28rem)] rounded-3xl border-2 border-ink/40 bg-ink/30 p-1 shadow-xl">
                                <div class="min-h-[22rem] rounded-2xl border border-ink/50 bg-inkLight p-6 flex flex-col">
                                <p class="text-xs uppercase tracking-wide text-accent/80">{ "App" }</p>
                                <p class="mt-4 text-lg font-semibold">{ app.name.clone() }</p>
                                <p class="mt-2 text-sm text-secondary/70">{ app.description.clone() }</p>
                                <div class="mt-4 flex items-center justify-between text-xs text-secondary/60">
                                    <span>{ format!("Version {}", if app.version.is_empty() { "-" } else { &app.version }) }</span>
                                    <span>{ format_size(app.archive_size) }</span>
                                </div>
                                <div class="mt-2 text-xs text-secondary/60">
                                    { format!("Playtime: {}", playtime_label) }
                                </div>
                                <div class="mt-auto flex items-center justify-between">
                                    { action }
                                    if !progress.is_empty() || !status_label.is_empty() {
                                        <span class="text-xs text-secondary/60">
                                            { if status_label.is_empty() {
                                                if speed.is_empty() { progress.clone() } else { format!("{progress} - {speed}") }
                                            } else if progress.is_empty() {
                                                status_label.to_string()
                                            } else if speed.is_empty() {
                                                format!("{status_label} - {progress}")
                                            } else {
                                                format!("{status_label} - {progress} - {speed}")
                                            } }
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

fn format_playtime(total_seconds: i64) -> String {
    if total_seconds <= 0 {
        return "Not played yet".to_string();
    }
    let minutes = total_seconds / 60;
    if minutes < 60 {
        return format!("{}m", minutes);
    }
    let hours = minutes / 60;
    let rem_minutes = minutes % 60;
    if hours < 100 {
        return format!("{}h {}m", hours, rem_minutes);
    }
    format!("{}h", hours)
}
