use js_sys::{ArrayBuffer, Uint8Array};
use serde_json;
use serde_wasm_bindgen;
use wasm_bindgen::{closure::Closure, JsCast, JsValue};
use wasm_bindgen_futures::spawn_local;
use web_sys::{BinaryType, Blob, MessageEvent, WebSocket};
use yew::prelude::*;

use crate::app::AppState;
use crate::net::build_ws_url;

#[derive(Clone, PartialEq, serde::Deserialize)]
struct SocialUser {
    user_id: String,
    username: String,
    status: String,
    #[serde(default)]
    app_id: String,
    #[serde(default)]
    app_name: String,
    updated_at: i64,
}

#[derive(Clone, PartialEq, serde::Deserialize)]
struct SocialEvent {
    #[serde(rename = "type")]
    event_type: String,
    users: Option<Vec<SocialUser>>,
}

#[function_component(SocialScreen)]
pub fn social_screen() -> Html {
    let app_state = use_context::<UseStateHandle<AppState>>()
        .expect("AppState context not found. Ensure SocialScreen is under <ContextProvider>.");
    let server_ip = app_state.server_ip.clone().unwrap_or_default();
    let server_port = app_state
        .server_port
        .clone()
        .unwrap_or_else(|| "2121".to_string());
    let token = app_state.session_token.clone().unwrap_or_default();

    let users = use_state(Vec::<SocialUser>::new);
    let error = use_state(|| None::<String>);
    let loading = use_state(|| true);

    {
        let users = users.clone();
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
                    return Box::new(|| ()) as Box<dyn FnOnce()>;
                }

                let ws_url = build_ws_url(
                    &server_ip,
                    &server_port,
                    &format!("social/ws?token={token}"),
                );
                let ws = match WebSocket::new(&ws_url) {
                    Ok(ws) => ws,
                    Err(_) => {
                        error.set(Some("Failed to open social socket.".to_string()));
                        loading.set(false);
                        return Box::new(|| ()) as Box<dyn FnOnce()>;
                    }
                };
                ws.set_binary_type(BinaryType::Arraybuffer);
                loading.set(false);

                let onmessage = {
                    let users = users.clone();
                    let error = error.clone();
                    Closure::<dyn FnMut(MessageEvent)>::wrap(Box::new(
                        move |event: MessageEvent| {
                            let data = event.data();
                            let users = users.clone();
                            let error = error.clone();

                            if let Some(event) = parse_social_event(data.clone()) {
                                apply_social_event(event, &users);
                                return;
                            }

                            if data.is_instance_of::<Blob>() {
                                let blob: Blob = data.unchecked_into();
                                let users = users.clone();
                                let error = error.clone();
                                spawn_local(async move {
                                    if let Ok(buf) =
                                        wasm_bindgen_futures::JsFuture::from(blob.array_buffer())
                                            .await
                                    {
                                        if let Some(event) = parse_social_event(buf) {
                                            apply_social_event(event, &users);
                                        } else {
                                            error.set(Some("Invalid social event.".to_string()));
                                        }
                                    }
                                });
                                return;
                            }

                            error.set(Some("Invalid social event.".to_string()));
                        },
                    ))
                };
                ws.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
                onmessage.forget();

                let onerror = {
                    let error = error.clone();
                    Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(move |_event| {
                        error.set(Some("Social connection error.".to_string()));
                    }))
                };
                ws.set_onerror(Some(onerror.as_ref().unchecked_ref()));
                onerror.forget();

                let ws_ref = ws.clone();
                Box::new(move || {
                    let _ = ws_ref.close();
                }) as Box<dyn FnOnce()>
            },
        );
    }

    html! {
        <div class="flex-1 min-h-0 flex flex-col overflow-hidden">
            <div>
                <h1 class="text-3xl font-semibold">{ "Social" }</h1>
                <p class="mt-2 text-sm text-accent">
                    { "See who is online and what they are playing." }
                </p>
            </div>
            if *loading {
                <div class="mt-6 rounded-2xl border border-ink/50 bg-inkLight p-6 text-base text-secondary/70">
                    { "Connecting..." }
                </div>
            } else if let Some(msg) = (*error).clone() {
                <div class="mt-6 rounded-2xl border border-rose-400/60 bg-inkLight p-6 text-base text-secondary/80">
                    { msg }
                </div>
            } else if users.is_empty() {
                <div class="mt-6 rounded-2xl border border-ink/50 bg-inkLight p-6 text-base text-secondary/70">
                    { "No one is online yet." }
                </div>
            } else {
                <div class="mt-6 h-[calc(100vh-22rem)] overflow-y-auto pr-2 scrollbar-thin">
                    <div class="grid gap-4 md:grid-cols-2">
                    { for users.iter().map(|user| {
                        let status_label = if user.status == "playing" {
                            if user.app_name.is_empty() {
                                "Playing".to_string()
                            } else {
                                format!("Playing {}", user.app_name)
                            }
                        } else {
                            "Online".to_string()
                        };
                        html! {
                            <div class="rounded-2xl border border-ink/50 bg-inkLight p-5">
                                <div class="flex items-center justify-between">
                                    <div>
                                        <p class="text-lg font-semibold">{ user.username.clone() }</p>
                                        <p class="mt-1 text-xs uppercase tracking-wide text-accent/80">{ status_label }</p>
                                    </div>
                                    <span class={if user.status == "playing" { "h-3 w-3 rounded-full bg-primary" } else { "h-3 w-3 rounded-full bg-emerald-400" }} />
                                </div>
                            </div>
                        }
                    }) }
                    </div>
                </div>
            }
        </div>
    }
}

fn parse_social_event(data: JsValue) -> Option<SocialEvent> {
    if let Some(text) = data.as_string() {
        return serde_json::from_str(&text).ok();
    }
    if let Ok(buf) = data.clone().dyn_into::<ArrayBuffer>() {
        let bytes = Uint8Array::new(&buf).to_vec();
        if let Ok(text) = String::from_utf8(bytes) {
            return serde_json::from_str(&text).ok();
        }
    }
    serde_wasm_bindgen::from_value(data).ok()
}

fn apply_social_event(event: SocialEvent, users: &UseStateHandle<Vec<SocialUser>>) {
    if event.event_type == "snapshot" {
        if let Some(list) = event.users {
            users.set(list);
        }
    }
}
