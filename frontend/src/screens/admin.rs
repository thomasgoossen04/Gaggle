use wasm_bindgen_futures::spawn_local;
use web_sys::{HtmlInputElement, HtmlTextAreaElement};
use yew::prelude::*;

use crate::api::{get_json, send_json};
use crate::app::AppState;
use crate::components::Button;
use crate::confirm::{use_confirm, ConfirmRequest};
use crate::net::build_http_url;
use crate::toast::{use_toast, ToastVariant};
use js_sys::{Function, Reflect};
use wasm_bindgen::prelude::JsValue;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "core"])]
    async fn invoke(cmd: &str, args: JsValue) -> JsValue;
}

#[derive(Clone, PartialEq, serde::Deserialize)]
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

#[derive(Clone, PartialEq, serde::Deserialize)]
struct UploadProgressEvent {
    id: String,
    sent: u64,
    total: u64,
    pct: f64,
}

#[derive(Clone, PartialEq, serde::Deserialize)]
struct UploadStageEvent {
    id: String,
    stage: String,
}

#[function_component(AdminScreen)]
pub fn admin_screen() -> Html {
    let app_state = use_context::<UseStateHandle<AppState>>()
        .expect("AppState context not found. Ensure AdminScreen is under <ContextProvider>.");
    let server_ip = app_state.server_ip.clone().unwrap_or_default();
    let server_port = app_state
        .server_port
        .clone()
        .unwrap_or_else(|| "2121".to_string());
    let token = app_state.session_token.clone().unwrap_or_default();

    let sessions = use_state(|| None::<i64>);
    let error = use_state(|| None::<String>);
    let loading = use_state(|| true);
    let clearing = use_state(|| false);
    let clearing_sessions = use_state(|| false);
    let confirm = use_confirm();
    let toast = use_toast();
    let reloading = use_state(|| false);
    let config_open = use_state(|| false);
    let config_loading = use_state(|| false);
    let config_saving = use_state(|| false);
    let config_raw = use_state(String::new);
    let admin_tab = use_state(|| AdminTab::Overview);
    let upload_id = use_state(String::new);
    let upload_name = use_state(String::new);
    let upload_desc = use_state(String::new);
    let upload_version = use_state(|| "0.1.0".to_string());
    let upload_exec = use_state(String::new);
    let upload_folder = use_state(String::new);
    let uploading = use_state(|| false);
    let upload_progress = use_state(|| 0.0);
    let upload_current_id = use_state(String::new);
    let upload_stage = use_state(|| "idle".to_string());
    let manage_apps = use_state(Vec::<AppInfo>::new);
    let manage_loading = use_state(|| false);
    let manage_error = use_state(|| None::<String>);
    let manage_open = use_state(|| false);
    let manage_config = use_state(String::new);
    let manage_app_id = use_state(String::new);
    let manage_saving = use_state(|| false);

    {
        let sessions = sessions.clone();
        let error = error.clone();
        let loading = loading.clone();
        let server_ip = server_ip.clone();
        let server_port = server_port.clone();
        let token = token.clone();
        use_effect_with(
            (server_ip.clone(), server_port.clone(), token.clone()),
            move |_| {
                if server_ip.is_empty() || server_port.is_empty() || token.is_empty() {
                    loading.set(false);
                    return ();
                }

                spawn_local(async move {
                    match fetch_admin_stats(&server_ip, &server_port, &token).await {
                        Ok(count) => sessions.set(Some(count)),
                        Err(msg) => error.set(Some(msg)),
                    }
                    loading.set(false);
                });

                ()
            },
        );
    }

    let on_clear_chat = {
        let clearing = clearing.clone();
        let error = error.clone();
        let server_ip = server_ip.clone();
        let server_port = server_port.clone();
        let token = token.clone();
        let confirm = confirm.clone();
        let toast = toast.clone();
        Callback::from(move |_| {
            if server_ip.is_empty() || server_port.is_empty() || token.is_empty() {
                return;
            }
            let clearing = clearing.clone();
            let error = error.clone();
            let server_ip = server_ip.clone();
            let server_port = server_port.clone();
            let token = token.clone();
            let toast = toast.clone();
            confirm.confirm(ConfirmRequest {
                title: "Clear chat messages?".to_string(),
                message: "This will permanently delete all chat messages for everyone.".to_string(),
                confirm_label: "Clear chat".to_string(),
                cancel_label: "Cancel".to_string(),
                on_confirm: Callback::from(move |_| {
                    clearing.set(true);
                    let clearing = clearing.clone();
                    let error = error.clone();
                    let server_ip = server_ip.clone();
                    let server_port = server_port.clone();
                    let token = token.clone();
                    let toast = toast.clone();
                    spawn_local(async move {
                        if let Err(msg) = clear_chat(&server_ip, &server_port, &token).await {
                            error.set(Some(msg.clone()));
                            toast.toast(msg, ToastVariant::Error, Some(3500));
                        } else {
                            toast.toast("Chat cleared.", ToastVariant::Success, Some(2500));
                        }
                        clearing.set(false);
                    });
                }),
            });
        })
    };

    let on_clear_sessions = {
        let clearing_sessions = clearing_sessions.clone();
        let server_ip = server_ip.clone();
        let server_port = server_port.clone();
        let token = token.clone();
        let toast = toast.clone();
        let sessions = sessions.clone();
        let confirm = confirm.clone();

        Callback::from(move |_| {
            if server_ip.is_empty() || server_port.is_empty() || token.is_empty() {
                toast.toast(
                    "Missing server address or token.",
                    ToastVariant::Warning,
                    Some(2500),
                );
                return;
            }

            let clearing_sessions = clearing_sessions.clone();
            let toast = toast.clone();
            let sessions = sessions.clone();

            // clone once per click
            let server_ip = server_ip.clone();
            let server_port = server_port.clone();
            let token = token.clone();

            confirm.confirm(ConfirmRequest {
                title: "Clear all sessions?".to_string(),
                message: "This logs out all users immediately.".to_string(),
                confirm_label: "Clear sessions".to_string(),
                cancel_label: "Cancel".to_string(),

                on_confirm: Callback::from({
                    let clearing_sessions = clearing_sessions.clone();
                    let toast = toast.clone();
                    let sessions = sessions.clone();

                    move |_| {
                        clearing_sessions.set(true);

                        // clone INSIDE the Fn callback
                        let clearing_sessions = clearing_sessions.clone();
                        let toast = toast.clone();
                        let sessions = sessions.clone();
                        let server_ip = server_ip.clone();
                        let server_port = server_port.clone();
                        let token = token.clone();

                        spawn_local(async move {
                            if let Err(msg) = clear_sessions(&server_ip, &server_port, &token).await
                            {
                                toast.toast(msg, ToastVariant::Error, Some(3000));
                            } else {
                                sessions.set(Some(0));
                                toast.toast("Sessions cleared.", ToastVariant::Success, Some(2500));
                            }
                            clearing_sessions.set(false);
                        });
                    }
                }),
            });
        })
    };

    let on_reload_config = {
        let server_ip = server_ip.clone();
        let server_port = server_port.clone();
        let token = token.clone();
        let toast = toast.clone();
        let reloading = reloading.clone();
        Callback::from(move |_| {
            if server_ip.is_empty() || server_port.is_empty() || token.is_empty() {
                return;
            }
            reloading.set(true);
            let server_ip = server_ip.clone();
            let server_port = server_port.clone();
            let token = token.clone();
            let toast = toast.clone();
            let reloading = reloading.clone();
            spawn_local(async move {
                let url = build_http_url(&server_ip, &server_port, "admin/reload-config");
                match send_json("POST", &url, Some(&token), None).await {
                    Ok(resp) if resp.ok() => {
                        toast.toast("Config reloaded.", ToastVariant::Success, Some(2000));
                    }
                    Ok(resp) => {
                        toast.toast(
                            format!("Reload failed (HTTP {}).", resp.status()),
                            ToastVariant::Error,
                            Some(3000),
                        );
                    }
                    Err(msg) => {
                        toast.toast(msg, ToastVariant::Error, Some(3000));
                    }
                }
                reloading.set(false);
            });
        })
    };

    let on_open_config = {
        let server_ip = server_ip.clone();
        let server_port = server_port.clone();
        let token = token.clone();
        let toast = toast.clone();
        let config_open = config_open.clone();
        let config_loading = config_loading.clone();
        let config_raw = config_raw.clone();
        Callback::from(move |_| {
            if server_ip.is_empty() || server_port.is_empty() || token.is_empty() {
                toast.toast(
                    "Missing server address or token.",
                    ToastVariant::Warning,
                    Some(2500),
                );
                return;
            }
            config_open.set(true);
            config_loading.set(true);
            let server_ip = server_ip.clone();
            let server_port = server_port.clone();
            let token = token.clone();
            let toast = toast.clone();
            let config_loading = config_loading.clone();
            let config_raw = config_raw.clone();
            spawn_local(async move {
                let url = build_http_url(&server_ip, &server_port, "admin/config/raw");
                match get_json::<RawConfigPayload>(&url, Some(&token)).await {
                    Ok(payload) => config_raw.set(payload.content),
                    Err(msg) => {
                        toast.toast(msg, ToastVariant::Error, Some(3000));
                    }
                }
                config_loading.set(false);
            });
        })
    };

    let on_close_config = {
        let config_open = config_open.clone();
        Callback::from(move |_| config_open.set(false))
    };

    let on_save_config = {
        let server_ip = server_ip.clone();
        let server_port = server_port.clone();
        let token = token.clone();
        let toast = toast.clone();
        let config_raw = config_raw.clone();
        let config_saving = config_saving.clone();
        let config_open = config_open.clone();
        Callback::from(move |_| {
            if server_ip.is_empty() || server_port.is_empty() || token.is_empty() {
                toast.toast(
                    "Missing server address or token.",
                    ToastVariant::Warning,
                    Some(2500),
                );
                return;
            }
            config_saving.set(true);
            let server_ip = server_ip.clone();
            let server_port = server_port.clone();
            let token = token.clone();
            let toast = toast.clone();
            let config_saving = config_saving.clone();
            let config_open = config_open.clone();
            let config_raw = (*config_raw).clone();
            spawn_local(async move {
                let url = build_http_url(&server_ip, &server_port, "admin/config/raw");
                let body = match serde_json::to_value(RawConfigPayload {
                    content: config_raw,
                }) {
                    Ok(body) => body,
                    Err(_) => {
                        toast.toast("Failed to encode config.", ToastVariant::Error, Some(3000));
                        config_saving.set(false);
                        return;
                    }
                };
                match send_json("PUT", &url, Some(&token), Some(body)).await {
                    Ok(resp) if resp.ok() => {
                        toast.toast("Config saved.", ToastVariant::Success, Some(2500));
                        config_open.set(false);
                    }
                    Ok(resp) => {
                        toast.toast(
                            format!("Save failed (HTTP {}).", resp.status()),
                            ToastVariant::Error,
                            Some(3000),
                        );
                    }
                    Err(msg) => {
                        toast.toast(msg, ToastVariant::Error, Some(3000));
                    }
                }
                config_saving.set(false);
            });
        })
    };

    let on_restart_backend = {
        let server_ip = server_ip.clone();
        let server_port = server_port.clone();
        let token = token.clone();
        let toast = toast.clone();
        let confirm = confirm.clone();
        Callback::from(move |_| {
            if server_ip.is_empty() || server_port.is_empty() || token.is_empty() {
                return;
            }
            let server_ip = server_ip.clone();
            let server_port = server_port.clone();
            let token = token.clone();
            let toast = toast.clone();
            confirm.confirm(ConfirmRequest {
                title: "Restart backend?".to_string(),
                message: "This will stop the backend process. Make sure it is supervised to restart automatically."
                    .to_string(),
                confirm_label: "Restart".to_string(),
                cancel_label: "Cancel".to_string(),
                on_confirm: Callback::from(move |_| {
                    let server_ip = server_ip.clone();
                    let server_port = server_port.clone();
                    let token = token.clone();
                    let toast = toast.clone();
                    spawn_local(async move {
                        let url = build_http_url(&server_ip, &server_port, "admin/restart");
                        match send_json("POST", &url, Some(&token), None).await {
                            Ok(resp) if resp.ok() => {
                                toast.toast("Restarting backend...", ToastVariant::Success, Some(2000));
                            }
                            Ok(resp) => {
                                toast.toast(
                                    format!("Restart failed (HTTP {}).", resp.status()),
                                    ToastVariant::Error,
                                    Some(3000),
                                );
                            }
                            Err(msg) => {
                                toast.toast(msg, ToastVariant::Error, Some(3000));
                            }
                        }
                    });
                }),
            });
        })
    };

    let on_pick_upload_folder = {
        let upload_folder = upload_folder.clone();
        let toast = toast.clone();
        Callback::from(move |_| {
            let upload_folder = upload_folder.clone();
            let toast = toast.clone();
            spawn_local(async move {
                let result = invoke("pick_upload_folder", JsValue::NULL).await;
                if let Ok(path) = serde_wasm_bindgen::from_value::<Option<String>>(result) {
                    if let Some(path) = path {
                        upload_folder.set(path);
                    }
                } else {
                    toast.toast(
                        "Failed to open folder picker.",
                        ToastVariant::Error,
                        Some(3000),
                    );
                }
            });
        })
    };

    {
        let upload_progress = upload_progress.clone();
        let upload_current_id = upload_current_id.clone();
        let upload_stage = upload_stage.clone();
        use_effect_with((), move |_| {
            let window = web_sys::window().unwrap();
            let tauri = Reflect::get(&window, &JsValue::from_str("__TAURI__"));
            if let Ok(tauri) = tauri {
                if let Ok(event) = Reflect::get(&tauri, &JsValue::from_str("event")) {
                    if let Ok(listen) = Reflect::get(&event, &JsValue::from_str("listen")) {
                        let listen_fn: Function = listen.dyn_into().unwrap();
                        let upload_current_id = upload_current_id.clone();
                        let upload_progress = upload_progress.clone();
                        let callback =
                            Closure::<dyn FnMut(JsValue)>::wrap(Box::new(move |value: JsValue| {
                                let payload = Reflect::get(&value, &JsValue::from_str("payload"))
                                    .unwrap_or(JsValue::NULL);
                                let event: Result<UploadProgressEvent, _> =
                                    serde_wasm_bindgen::from_value(payload);
                                if let Ok(event) = event {
                                    if event.id == (*upload_current_id).clone() {
                                        upload_progress.set(event.pct);
                                    }
                                }
                            }));
                        let _ = listen_fn.call2(
                            &event,
                            &JsValue::from_str("app_upload_progress"),
                            callback.as_ref().unchecked_ref(),
                        );
                        callback.forget();
                    }
                }
            }
            let tauri = Reflect::get(&window, &JsValue::from_str("__TAURI__"));
            if let Ok(tauri) = tauri {
                if let Ok(event) = Reflect::get(&tauri, &JsValue::from_str("event")) {
                    if let Ok(listen) = Reflect::get(&event, &JsValue::from_str("listen")) {
                        let listen_fn: Function = listen.dyn_into().unwrap();
                        let upload_current_id = upload_current_id.clone();
                        let upload_stage = upload_stage.clone();
                        let upload_progress = upload_progress.clone();
                        let callback =
                            Closure::<dyn FnMut(JsValue)>::wrap(Box::new(move |value: JsValue| {
                                let payload = Reflect::get(&value, &JsValue::from_str("payload"))
                                    .unwrap_or(JsValue::NULL);
                                let event: Result<UploadStageEvent, _> =
                                    serde_wasm_bindgen::from_value(payload);
                                if let Ok(event) = event {
                                    if event.id == (*upload_current_id).clone() {
                                        let stage = event.stage.clone();
                                        if stage == "uploading" {
                                            upload_progress.set(0.0);
                                        }
                                        upload_stage.set(stage);
                                    }
                                }
                            }));
                        let _ = listen_fn.call2(
                            &event,
                            &JsValue::from_str("app_upload_stage"),
                            callback.as_ref().unchecked_ref(),
                        );
                        callback.forget();
                    }
                }
            }
            || ()
        });
    }

    {
        let server_ip = server_ip.clone();
        let server_port = server_port.clone();
        let token = token.clone();
        let admin_tab = admin_tab.clone();
        let manage_apps = manage_apps.clone();
        let manage_loading = manage_loading.clone();
        let manage_error = manage_error.clone();
        use_effect_with(
            (
                admin_tab.clone(),
                server_ip.clone(),
                server_port.clone(),
                token.clone(),
            ),
            move |_| {
                if *admin_tab != AdminTab::Manage {
                    return ();
                }
                if server_ip.is_empty() || server_port.is_empty() || token.is_empty() {
                    return ();
                }
                manage_loading.set(true);
                manage_error.set(None);
                let server_ip = server_ip.clone();
                let server_port = server_port.clone();
                let token = token.clone();
                let manage_apps = manage_apps.clone();
                let manage_loading = manage_loading.clone();
                let manage_error = manage_error.clone();
                spawn_local(async move {
                    let url = build_http_url(&server_ip, &server_port, "apps");
                    match get_json::<Vec<AppInfo>>(&url, Some(&token)).await {
                        Ok(list) => manage_apps.set(list),
                        Err(msg) => manage_error.set(Some(msg)),
                    }
                    manage_loading.set(false);
                });
                ()
            },
        );
    }

    let on_upload_app = {
        let server_ip = server_ip.clone();
        let server_port = server_port.clone();
        let token = token.clone();
        let upload_id = upload_id.clone();
        let upload_name = upload_name.clone();
        let upload_desc = upload_desc.clone();
        let upload_version = upload_version.clone();
        let upload_exec = upload_exec.clone();
        let upload_folder = upload_folder.clone();
        let uploading = uploading.clone();
        let upload_progress = upload_progress.clone();
        let upload_current_id = upload_current_id.clone();
        let upload_stage = upload_stage.clone();
        let toast = toast.clone();
        Callback::from(move |_| {
            if server_ip.is_empty() || server_port.is_empty() || token.is_empty() {
                toast.toast(
                    "Missing server address or token.",
                    ToastVariant::Warning,
                    Some(2500),
                );
                return;
            }
            let id = upload_id.as_str().trim().to_string();
            if id.is_empty() {
                toast.toast("App ID is required.", ToastVariant::Warning, Some(2500));
                return;
            }
            let name = upload_name.as_str().trim().to_string();
            if name.is_empty() {
                toast.toast("App name is required.", ToastVariant::Warning, Some(2500));
                return;
            }
            let folder = upload_folder.as_str().trim().to_string();
            if folder.is_empty() {
                toast.toast(
                    "Pick an app folder to upload.",
                    ToastVariant::Warning,
                    Some(2500),
                );
                return;
            }

            let desc = upload_desc.as_str().trim().to_string();
            let version = upload_version.as_str().trim().to_string();
            let exec = upload_exec.as_str().trim().to_string();

            let esc = |value: &str| value.replace('\\', "\\\\").replace('"', "\\\"");
            let mut config = String::new();
            config.push_str(&format!("name = \"{}\"\n", esc(&name)));
            if !desc.is_empty() {
                config.push_str(&format!("description = \"{}\"\n", esc(&desc)));
            }
            if !version.is_empty() {
                config.push_str(&format!("version = \"{}\"\n", esc(&version)));
            }
            if !exec.is_empty() {
                config.push_str(&format!("executable = \"{}\"\n", esc(&exec)));
            }

            upload_current_id.set(id.clone());
            upload_progress.set(0.0);
            upload_stage.set("starting".to_string());
            uploading.set(true);
            let server_ip = server_ip.clone();
            let server_port = server_port.clone();
            let token = token.clone();
            let uploading = uploading.clone();
            let upload_progress = upload_progress.clone();
            let upload_stage = upload_stage.clone();
            let toast = toast.clone();
            spawn_local(async move {
                let payload = serde_wasm_bindgen::to_value(&serde_json::json!({
                    "request": {
                        "serverIp": server_ip,
                        "serverPort": server_port,
                        "token": token,
                        "id": id,
                        "configToml": config,
                        "folderPath": folder
                    }
                }))
                .unwrap_or(JsValue::NULL);
                let result = invoke("upload_app", payload).await;
                if result.is_null() || result.is_undefined() {
                    toast.toast("App uploaded.", ToastVariant::Success, Some(2500));
                    upload_progress.set(100.0);
                    upload_stage.set("done".to_string());
                } else if let Ok(msg) = serde_wasm_bindgen::from_value::<String>(result.clone()) {
                    if msg.is_empty() {
                        toast.toast("App uploaded.", ToastVariant::Success, Some(2500));
                        upload_progress.set(100.0);
                        upload_stage.set("done".to_string());
                    } else {
                        toast.toast(msg, ToastVariant::Error, Some(3000));
                        upload_stage.set("error".to_string());
                    }
                } else {
                    toast.toast("App uploaded.", ToastVariant::Success, Some(2500));
                    upload_progress.set(100.0);
                    upload_stage.set("done".to_string());
                }
                uploading.set(false);
            });
        })
    };

    let on_open_manage_config = {
        let server_ip = server_ip.clone();
        let server_port = server_port.clone();
        let token = token.clone();
        let manage_open = manage_open.clone();
        let manage_config = manage_config.clone();
        let manage_app_id = manage_app_id.clone();
        let toast = toast.clone();
        Callback::from(move |app_id: String| {
            if server_ip.is_empty() || server_port.is_empty() || token.is_empty() {
                toast.toast(
                    "Missing server address or token.",
                    ToastVariant::Warning,
                    Some(2500),
                );
                return;
            }
            manage_open.set(true);
            manage_config.set(String::new());
            manage_app_id.set(app_id.clone());
            let server_ip = server_ip.clone();
            let server_port = server_port.clone();
            let token = token.clone();
            let manage_config = manage_config.clone();
            let toast = toast.clone();
            spawn_local(async move {
                let url = build_http_url(
                    &server_ip,
                    &server_port,
                    &format!("admin/apps/{}/config", app_id),
                );
                match get_json::<RawConfigPayload>(&url, Some(&token)).await {
                    Ok(payload) => manage_config.set(payload.content),
                    Err(msg) => toast.toast(msg, ToastVariant::Error, Some(3000)),
                }
            });
        })
    };

    let on_save_manage_config = {
        let server_ip = server_ip.clone();
        let server_port = server_port.clone();
        let token = token.clone();
        let manage_app_id = manage_app_id.clone();
        let manage_config = manage_config.clone();
        let manage_saving = manage_saving.clone();
        let manage_open = manage_open.clone();
        let toast = toast.clone();
        Callback::from(move |_| {
            if server_ip.is_empty() || server_port.is_empty() || token.is_empty() {
                toast.toast(
                    "Missing server address or token.",
                    ToastVariant::Warning,
                    Some(2500),
                );
                return;
            }
            let app_id = manage_app_id.as_str().trim().to_string();
            if app_id.is_empty() {
                toast.toast("Missing app id.", ToastVariant::Warning, Some(2500));
                return;
            }
            manage_saving.set(true);
            let server_ip = server_ip.clone();
            let server_port = server_port.clone();
            let token = token.clone();
            let manage_config = manage_config.as_str().to_string();
            let manage_saving = manage_saving.clone();
            let manage_open = manage_open.clone();
            let toast = toast.clone();
            spawn_local(async move {
                let url = build_http_url(
                    &server_ip,
                    &server_port,
                    &format!("admin/apps/{}/config", app_id),
                );
                let body = serde_json::json!({ "content": manage_config });
                match send_json("PUT", &url, Some(&token), Some(body)).await {
                    Ok(resp) if resp.ok() => {
                        toast.toast("Config saved.", ToastVariant::Success, Some(2500));
                        manage_open.set(false);
                    }
                    Ok(resp) => {
                        toast.toast(
                            format!("Save failed (HTTP {}).", resp.status()),
                            ToastVariant::Error,
                            Some(3000),
                        );
                    }
                    Err(msg) => {
                        toast.toast(msg, ToastVariant::Error, Some(3000));
                    }
                }
                manage_saving.set(false);
            });
        })
    };

    let on_delete_app = {
        let server_ip = server_ip.clone();
        let server_port = server_port.clone();
        let token = token.clone();
        let confirm = confirm.clone();
        let toast = toast.clone();
        let manage_apps = manage_apps.clone();
        Callback::from(move |app_id: String| {
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
            let manage_apps = manage_apps.clone();
            confirm.confirm(ConfirmRequest {
                title: "Delete app?".to_string(),
                message: format!("This will delete the app \"{app_id}\" from the backend."),
                confirm_label: "Delete".to_string(),
                cancel_label: "Cancel".to_string(),
                on_confirm: Callback::from(move |_| {
                    let server_ip = server_ip.clone();
                    let server_port = server_port.clone();
                    let token = token.clone();
                    let toast = toast.clone();
                    let manage_apps = manage_apps.clone();
                    let app_id = app_id.clone();
                    spawn_local(async move {
                        let url = build_http_url(
                            &server_ip,
                            &server_port,
                            &format!("admin/apps/{}", app_id),
                        );
                        match send_json("DELETE", &url, Some(&token), None).await {
                            Ok(resp) if resp.ok() => {
                                toast.toast("App deleted.", ToastVariant::Success, Some(2500));
                                let mut next = (*manage_apps).clone();
                                next.retain(|app| app.id != app_id);
                                manage_apps.set(next);
                            }
                            Ok(resp) => {
                                toast.toast(
                                    format!("Delete failed (HTTP {}).", resp.status()),
                                    ToastVariant::Error,
                                    Some(3000),
                                );
                            }
                            Err(msg) => toast.toast(msg, ToastVariant::Error, Some(3000)),
                        }
                    });
                }),
            });
        })
    };

    html! {
        <div>
            <div>
                <h1 class="text-2xl font-semibold">{ "Admin" }</h1>
                <p class="mt-2 text-base text-accent">
                    { "Admin-only tools and diagnostics." }
                </p>
            </div>
            <div class="mt-6 flex flex-wrap items-center gap-2">
                <Button
                    class={Some(if *admin_tab == AdminTab::Overview { "border border-ink/50 bg-ink/40 text-secondary".to_string() } else { "border border-ink/30 bg-transparent text-secondary/80 hover:bg-ink/40".to_string() })}
                    onclick={{
                        let admin_tab = admin_tab.clone();
                        Callback::from(move |_| admin_tab.set(AdminTab::Overview))
                    }}
                >
                    { "Overview" }
                </Button>
                <Button
                    class={Some(if *admin_tab == AdminTab::Uploads { "border border-ink/50 bg-ink/40 text-secondary".to_string() } else { "border border-ink/30 bg-transparent text-secondary/80 hover:bg-ink/40".to_string() })}
                    onclick={{
                        let admin_tab = admin_tab.clone();
                        Callback::from(move |_| admin_tab.set(AdminTab::Uploads))
                    }}
                >
                    { "Uploads" }
                </Button>
                <Button
                    class={Some(if *admin_tab == AdminTab::Manage { "border border-ink/50 bg-ink/40 text-secondary".to_string() } else { "border border-ink/30 bg-transparent text-secondary/80 hover:bg-ink/40".to_string() })}
                    onclick={{
                        let admin_tab = admin_tab.clone();
                        Callback::from(move |_| admin_tab.set(AdminTab::Manage))
                    }}
                >
                    { "Manage Apps" }
                </Button>
            </div>
            if *admin_tab == AdminTab::Overview {
                <div class="mt-8 grid gap-6 md:grid-cols-2 xl:grid-cols-3">
                <div class="rounded-2xl border border-ink/50 bg-inkLight p-6">
                    <p class="text-xs uppercase tracking-wide text-accent/80">{ "Server Health" }</p>
                    <p class="mt-4 text-lg font-semibold">{ "OK" }</p>
                    <p class="mt-2 text-sm text-secondary/70">{ "Backend responding." }</p>
                </div>
                <div class="rounded-2xl border border-ink/50 bg-inkLight p-6">
                    <p class="text-xs uppercase tracking-wide text-accent/80">{ "Sessions" }</p>
                    if *loading {
                        <p class="mt-4 text-lg font-semibold">{ "Loading..." }</p>
                        <p class="mt-2 text-sm text-secondary/70">{ "Fetching live stats." }</p>
                    } else if let Some(err) = (*error).clone() {
                        <p class="mt-4 text-lg font-semibold text-rose-300">{ "Error" }</p>
                        <p class="mt-2 text-sm text-secondary/70">{ err }</p>
                    } else if let Some(count) = *sessions {
                        <p class="mt-4 text-lg font-semibold">{ count }</p>
                        <p class="mt-2 text-sm text-secondary/70">{ "Active sessions." }</p>
                    } else {
                        <p class="mt-4 text-lg font-semibold">{ "-" }</p>
                        <p class="mt-2 text-sm text-secondary/70">{ "No data." }</p>
                    }
                    <Button
                        class={Some("mt-4 border border-rose-400/60 bg-rose-500/20 text-rose-100 hover:bg-rose-500/30".to_string())}
                        onclick={on_clear_sessions}
                        disabled={*clearing_sessions}
                    >
                        { if *clearing_sessions { "Clearing..." } else { "Clear sessions" } }
                    </Button>
                </div>
                <div class="rounded-2xl border border-ink/50 bg-inkLight p-6">
                    <p class="text-xs uppercase tracking-wide text-accent/80">{ "Feature Flags" }</p>
                    <p class="mt-4 text-lg font-semibold">{ "Chat" }</p>
                    <p class="mt-2 text-sm text-secondary/70">{ "Controlled by backend config." }</p>
                    <Button
                        class={Some("mt-4 border border-rose-400/60 bg-rose-500/20 text-rose-100 hover:bg-rose-500/30".to_string())}
                        onclick={on_clear_chat}
                        disabled={*clearing}
                    >
                        { if *clearing { "Clearing..." } else { "Clear chat messages" } }
                    </Button>
                </div>
                <div class="rounded-2xl border border-ink/50 bg-inkLight p-6">
                    <p class="text-xs uppercase tracking-wide text-accent/80">{ "Configuration" }</p>
                    <p class="mt-4 text-lg font-semibold">{ "Reload" }</p>
                    <p class="mt-2 text-sm text-secondary/70">{ "Apply changes from config.toml." }</p>
                    <Button
                        class={Some("mt-4 border border-ink/50 bg-ink/40 text-secondary hover:bg-ink/50".to_string())}
                        onclick={on_reload_config}
                        disabled={*reloading}
                    >
                        { if *reloading { "Reloading..." } else { "Reload config" } }
                    </Button>
                    <Button
                        class={Some("mt-3 ml-3 border border-accent/50 bg-accent/20 text-secondary hover:bg-accent/30".to_string())}
                        onclick={on_open_config.clone()}
                    >
                        { "Edit config" }
                    </Button>
                </div>
                <div class="rounded-2xl border border-ink/50 bg-inkLight p-6">
                    <p class="text-xs uppercase tracking-wide text-accent/80">{ "Backend" }</p>
                    <p class="mt-4 text-lg font-semibold">{ "Restart" }</p>
                    <p class="mt-2 text-sm text-secondary/70">{ "Restarts the server process." }</p>
                    <Button
                        class={Some("mt-4 border border-rose-400/60 bg-rose-500/20 text-rose-100 hover:bg-rose-500/30".to_string())}
                        onclick={on_restart_backend}
                    >
                        { "Restart backend" }
                    </Button>
                </div>
                </div>
            } else if *admin_tab == AdminTab::Uploads {
                <div class="mt-8 rounded-2xl border border-ink/50 bg-inkLight p-6">
                    <p class="text-xs uppercase tracking-wide text-accent/80">{ "Upload App" }</p>
                    <p class="mt-2 text-sm text-secondary/70">
                        { "Fill in the metadata and choose a folder to package and upload." }
                    </p>
                    <div class="mt-6 grid gap-4 md:grid-cols-2">
                        <div>
                            <label class="text-xs uppercase tracking-wide text-accent/80">{ "App ID" }</label>
                            <input
                                class="mt-2 w-full rounded border border-ink/50 bg-ink/40 px-3 py-2 text-secondary placeholder:text-secondary/60 focus:outline-none focus:ring-2 focus:ring-primary/40"
                                type="text"
                                placeholder="my-app"
                                value={(*upload_id).clone()}
                                oninput={on_text_input(upload_id.clone())}
                            />
                        </div>
                        <div>
                            <label class="text-xs uppercase tracking-wide text-accent/80">{ "Name" }</label>
                            <input
                                class="mt-2 w-full rounded border border-ink/50 bg-ink/40 px-3 py-2 text-secondary placeholder:text-secondary/60 focus:outline-none focus:ring-2 focus:ring-primary/40"
                                type="text"
                                placeholder="My App"
                                value={(*upload_name).clone()}
                                oninput={on_text_input(upload_name.clone())}
                            />
                        </div>
                        <div class="md:col-span-2">
                            <label class="text-xs uppercase tracking-wide text-accent/80">{ "Description" }</label>
                            <input
                                class="mt-2 w-full rounded border border-ink/50 bg-ink/40 px-3 py-2 text-secondary placeholder:text-secondary/60 focus:outline-none focus:ring-2 focus:ring-primary/40"
                                type="text"
                                placeholder="Short description"
                                value={(*upload_desc).clone()}
                                oninput={on_text_input(upload_desc.clone())}
                            />
                        </div>
                        <div>
                            <label class="text-xs uppercase tracking-wide text-accent/80">{ "Version" }</label>
                            <input
                                class="mt-2 w-full rounded border border-ink/50 bg-ink/40 px-3 py-2 text-secondary placeholder:text-secondary/60 focus:outline-none focus:ring-2 focus:ring-primary/40"
                                type="text"
                                placeholder="0.1.0"
                                value={(*upload_version).clone()}
                                oninput={on_text_input(upload_version.clone())}
                            />
                        </div>
                        <div>
                            <label class="text-xs uppercase tracking-wide text-accent/80">{ "Executable (optional)" }</label>
                            <input
                                class="mt-2 w-full rounded border border-ink/50 bg-ink/40 px-3 py-2 text-secondary placeholder:text-secondary/60 focus:outline-none focus:ring-2 focus:ring-primary/40"
                                type="text"
                                placeholder="bin/MyApp.exe"
                                value={(*upload_exec).clone()}
                                oninput={on_text_input(upload_exec.clone())}
                            />
                        </div>
                    </div>
                    <div class="mt-6 flex flex-wrap items-center gap-3">
                        <Button
                            class={Some("border border-ink/50 bg-ink/40 text-secondary hover:bg-ink/50".to_string())}
                            onclick={on_pick_upload_folder}
                        >
                            { "Pick Folder" }
                        </Button>
                        <input
                            class="flex-1 min-w-[220px] rounded border border-ink/50 bg-ink/40 px-3 py-2 text-secondary placeholder:text-secondary/60 focus:outline-none focus:ring-2 focus:ring-primary/40"
                            type="text"
                            placeholder="C:\\path\\to\\app\\folder"
                            value={(*upload_folder).clone()}
                            oninput={on_text_input(upload_folder.clone())}
                        />
                    </div>
                    <div class="mt-6">
                        <Button
                            class={Some("border border-primary/60 bg-primary/30 text-secondary hover:bg-primary/40".to_string())}
                            onclick={on_upload_app}
                            disabled={*uploading}
                        >
                            { if *uploading { "Uploading..." } else { "Upload App" } }
                        </Button>
                        if *uploading {
                            <span class="ml-3 text-sm text-secondary/70">
                                { format!("{}% - {}", upload_progress.round() as i64, upload_stage.as_str()) }
                            </span>
                        }
                    </div>
                </div>
            } else {
                <div class="mt-8 rounded-2xl border border-ink/50 bg-inkLight p-6">
                    <p class="text-xs uppercase tracking-wide text-accent/80">{ "Manage Apps" }</p>
                    <p class="mt-2 text-sm text-secondary/70">{ "Edit metadata or delete apps from the backend." }</p>
                    if *manage_loading {
                        <p class="mt-4 text-sm text-secondary/70">{ "Loading apps..." }</p>
                    } else if let Some(err) = (*manage_error).clone() {
                        <p class="mt-4 text-sm text-rose-300">{ err }</p>
                    } else if manage_apps.is_empty() {
                        <p class="mt-4 text-sm text-secondary/70">{ "No apps found." }</p>
                    } else {
                        <div class="mt-4 flex max-h-[60vh] flex-col gap-3 overflow-y-auto pr-1 scrollbar-thin">
                            { for manage_apps.iter().cloned().map(|app| {
                                let on_open_manage_config = on_open_manage_config.clone();
                                let on_delete_app = on_delete_app.clone();
                                let app_id = app.id.clone();
                                html! {
                                    <div class="flex flex-wrap items-center justify-between gap-3 rounded-xl border border-ink/50 bg-ink/40 px-4 py-3">
                                        <div>
                                            <p class="text-sm font-semibold">{ app.name.clone() }</p>
                                            <p class="text-xs text-secondary/70">{ app.id.clone() }</p>
                                        </div>
                                        <div class="flex items-center gap-2">
                                            <Button
                                                class={Some("border border-accent/50 bg-accent/20 text-secondary hover:bg-accent/30".to_string())}
                                                onclick={Callback::from(move |_| on_open_manage_config.emit(app_id.clone()))}
                                            >
                                                { "Edit config" }
                                            </Button>
                                            <Button
                                                class={Some("border border-rose-400/60 bg-rose-500/20 text-rose-100 hover:bg-rose-500/30".to_string())}
                                                onclick={Callback::from(move |_| on_delete_app.emit(app.id.clone()))}
                                            >
                                                { "Delete" }
                                            </Button>
                                        </div>
                                    </div>
                                }
                            }) }
                        </div>
                    }
                </div>
            }
            if *config_open {
                <div class="fixed inset-0 z-50 flex items-center justify-center">
                    <div class="absolute inset-0 bg-ink" onclick={on_close_config.clone()} />
                    <div class="relative w-[min(94vw,52rem)] max-h-[90vh] overflow-hidden rounded-2xl border border-ink/50 bg-inkLight shadow-2xl">
                        <div class="flex items-center justify-between border-b border-ink/40 px-6 py-4">
                            <div>
                                <h2 class="text-lg font-semibold text-secondary">{ "Edit config.toml" }</h2>
                                <p class="text-xs text-accent">{ "Changes are saved to the backend config file." }</p>
                            </div>
                            <Button
                                class={Some("border border-ink/50 bg-ink/40 text-secondary hover:bg-ink/50".to_string())}
                                onclick={on_close_config.clone()}
                            >
                                { "Close" }
                            </Button>
                        </div>
                        <div class="max-h-[calc(90vh-8rem)] overflow-y-auto px-6 py-5 scrollbar-thin">
                            if *config_loading {
                                <p class="text-sm text-secondary/70">{ "Loading config..." }</p>
                            } else {
                                <div>
                                    <label class="text-xs uppercase tracking-wide text-accent/80">{ "config.toml" }</label>
                                    <textarea
                                        class="mt-2 h-[60vh] w-full resize-none rounded border border-ink/50 bg-ink/40 px-3 py-3 font-mono text-sm text-secondary placeholder:text-secondary/60 focus:outline-none focus:ring-2 focus:ring-primary/40"
                                        value={(*config_raw).clone()}
                                        oninput={on_raw_input(config_raw.clone())}
                                    />
                                </div>
                            }
                        </div>
                        <div class="flex items-center justify-end gap-3 border-t border-ink/40 px-6 py-4">
                            <Button
                                class={Some("border border-ink/50 bg-ink/40 text-secondary hover:bg-ink/50".to_string())}
                                onclick={on_close_config.clone()}
                            >
                                { "Cancel" }
                            </Button>
                            <Button
                                class={Some("border border-primary/60 bg-primary/30 text-secondary hover:bg-primary/40".to_string())}
                                onclick={on_save_config}
                                disabled={*config_saving}
                            >
                                { if *config_saving { "Saving..." } else { "Save config" } }
                            </Button>
                        </div>
                    </div>
                </div>
            }
            if *manage_open {
                <div class="fixed inset-0 z-50 flex items-center justify-center">
                    <div class="absolute inset-0 bg-ink" onclick={{
                        let manage_open = manage_open.clone();
                        Callback::from(move |_| manage_open.set(false))
                    }} />
                    <div class="relative w-[min(94vw,52rem)] max-h-[90vh] overflow-hidden rounded-2xl border border-ink/50 bg-inkLight shadow-2xl">
                        <div class="flex items-center justify-between border-b border-ink/40 px-6 py-4">
                            <div>
                                <h2 class="text-lg font-semibold text-secondary">{ "Edit app config" }</h2>
                                <p class="text-xs text-accent">{ manage_app_id.as_str() }</p>
                            </div>
                            <Button
                                class={Some("border border-ink/50 bg-ink/40 text-secondary hover:bg-ink/50".to_string())}
                                onclick={{
                                    let manage_open = manage_open.clone();
                                    Callback::from(move |_| manage_open.set(false))
                                }}
                            >
                                { "Close" }
                            </Button>
                        </div>
                        <div class="max-h-[calc(90vh-8rem)] overflow-y-auto px-6 py-5 scrollbar-thin">
                            <div>
                                <label class="text-xs uppercase tracking-wide text-accent/80">{ "config.toml" }</label>
                                <textarea
                                    class="mt-2 h-[60vh] w-full resize-none rounded border border-ink/50 bg-ink/40 px-3 py-3 font-mono text-sm text-secondary placeholder:text-secondary/60 focus:outline-none focus:ring-2 focus:ring-primary/40"
                                    value={(*manage_config).clone()}
                                    oninput={on_raw_input(manage_config.clone())}
                                />
                            </div>
                        </div>
                        <div class="flex items-center justify-end gap-3 border-t border-ink/40 px-6 py-4">
                            <Button
                                class={Some("border border-ink/50 bg-ink/40 text-secondary hover:bg-ink/50".to_string())}
                                onclick={{
                                    let manage_open = manage_open.clone();
                                    Callback::from(move |_| manage_open.set(false))
                                }}
                            >
                                { "Cancel" }
                            </Button>
                            <Button
                                class={Some("border border-primary/60 bg-primary/30 text-secondary hover:bg-primary/40".to_string())}
                                onclick={on_save_manage_config}
                                disabled={*manage_saving}
                            >
                                { if *manage_saving { "Saving..." } else { "Save config" } }
                            </Button>
                        </div>
                    </div>
                </div>
            }
        </div>
    }
}

#[derive(Clone, Copy, PartialEq)]
enum AdminTab {
    Overview,
    Uploads,
    Manage,
}

#[derive(serde::Deserialize)]
struct AdminStatsResponse {
    sessions: i64,
}

async fn fetch_admin_stats(server_ip: &str, server_port: &str, token: &str) -> Result<i64, String> {
    let url = build_http_url(server_ip, server_port, "admin/stats");
    let data: AdminStatsResponse = get_json(&url, Some(token)).await?;
    Ok(data.sessions)
}

async fn clear_chat(server_ip: &str, server_port: &str, token: &str) -> Result<(), String> {
    let url = build_http_url(server_ip, server_port, "admin/chat/messages");
    let resp = send_json("DELETE", &url, Some(token), None).await?;
    if !resp.ok() {
        return Err(format!("Clear chat error (HTTP {}).", resp.status()));
    }
    Ok(())
}

async fn clear_sessions(server_ip: &str, server_port: &str, token: &str) -> Result<(), String> {
    let url = build_http_url(server_ip, server_port, "admin/sessions");
    let resp = send_json("DELETE", &url, Some(token), None).await?;
    if !resp.ok() {
        return Err(format!("Clear sessions error (HTTP {}).", resp.status()));
    }
    Ok(())
}

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct RawConfigPayload {
    content: String,
}

fn on_raw_input(form: UseStateHandle<String>) -> Callback<InputEvent> {
    Callback::from(move |event: InputEvent| {
        let input: HtmlTextAreaElement = event.target_unchecked_into();
        form.set(input.value());
    })
}

fn on_text_input(form: UseStateHandle<String>) -> Callback<InputEvent> {
    Callback::from(move |event: InputEvent| {
        let input: HtmlInputElement = event.target_unchecked_into();
        form.set(input.value());
    })
}
