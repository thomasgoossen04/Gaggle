use wasm_bindgen::{closure::Closure, JsCast, JsValue};
use js_sys::{ArrayBuffer, Uint8Array};
use wasm_bindgen_futures::spawn_local;
use web_sys::{BinaryType, Blob, MessageEvent, WebSocket};
use yew::prelude::*;

use crate::api::{get_json, send_json};
use crate::app::AppState;
use crate::components::Button;
use crate::confirm::{use_confirm, ConfirmRequest};
use crate::net::{build_http_url, build_ws_url};
use crate::toast::{use_toast, ToastVariant};

#[derive(Properties, PartialEq)]
pub struct ChatScreenProps {
    pub active: bool,
    pub on_unread: Callback<usize>,
}

#[function_component(ChatScreen)]
pub fn chat_screen(props: &ChatScreenProps) -> Html {
    let app_state = use_context::<UseStateHandle<AppState>>()
        .expect("AppState context not found. Ensure ChatScreen is under <ContextProvider>.");
    let server_ip = app_state.server_ip.clone().unwrap_or_default();
    let server_port = app_state
        .server_port
        .clone()
        .unwrap_or_else(|| "2121".to_string());
    let token = app_state.session_token.clone().unwrap_or_default();
    let is_admin = app_state.user.as_ref().map(|u| u.is_admin).unwrap_or(false);

    let messages = use_state(Vec::<ChatMessage>::new);
    let online_users = use_state(Vec::<String>::new);
    let input = use_state(String::new);
    let enabled = use_state(|| true);
    let error = use_state(|| None::<String>);
    let loading = use_state(|| true);
    let toast = use_toast();
    let active_ref = use_mut_ref(|| props.active);

    {
        let active_ref = active_ref.clone();
        let active = props.active;
        use_effect_with(active, move |active| {
            *active_ref.borrow_mut() = *active;
            ()
        });
    }

    {
        let messages = messages.clone();
        let enabled = enabled.clone();
        let error = error.clone();
        let loading = loading.clone();
        let server_ip = server_ip.clone();
        let server_port = server_port.clone();
        let token = token.clone();
        use_effect_with((server_ip.clone(), server_port.clone(), token.clone()), move |_| {
            if server_ip.is_empty() || server_port.is_empty() || token.is_empty() {
                loading.set(false);
                return ();
            }

            spawn_local(async move {
                let chat_enabled = match fetch_features(&server_ip, &server_port).await {
                    Ok(chat_enabled) => {
                        enabled.set(chat_enabled);
                        chat_enabled
                    }
                    Err(msg) => {
                        error.set(Some(msg));
                        loading.set(false);
                        return;
                    }
                };

                if chat_enabled {
                    match fetch_messages(&server_ip, &server_port, &token).await {
                        Ok(msgs) => {
                            let mut next = (*messages).clone();
                            for msg in msgs {
                                if !next.iter().any(|m| m.id == msg.id) {
                                    next.push(msg);
                                }
                            }
                            next.sort_by_key(|m| m.timestamp);
                            messages.set(next);
                        }
                        Err(msg) => error.set(Some(msg)),
                    }
                }
                loading.set(false);
            });
            ()
        });
    }

    {
        let messages = messages.clone();
        let online_users = online_users.clone();
        let error = error.clone();
        let enabled = enabled.clone();
        let loading = loading.clone();
        let server_ip = server_ip.clone();
        let server_port = server_port.clone();
        let token = token.clone();
        let active_ref = active_ref.clone();
        let on_unread = props.on_unread.clone();

        use_effect_with(
            (server_ip.clone(), server_port.clone(), token.clone(), *enabled, *loading),
            move |_| {
                if server_ip.is_empty()
                    || server_port.is_empty()
                    || token.is_empty()
                    || *loading
                    || !*enabled
                {
                return Box::new(|| ()) as Box<dyn FnOnce()>;
            }

            let ws_url =
                build_ws_url(&server_ip, &server_port, &format!("chat/ws?token={token}"));
            let ws = match WebSocket::new(&ws_url) {
                Ok(ws) => ws,
                Err(_) => {
                    error.set(Some("Failed to open chat socket.".to_string()));
                    return Box::new(|| ()) as Box<dyn FnOnce()>;
                }
            };
            ws.set_binary_type(BinaryType::Arraybuffer);

            let onmessage = {
                let messages = messages.clone();
                let online_users = online_users.clone();
                let error = error.clone();
                let active_ref = active_ref.clone();
                let on_unread = on_unread.clone();
                Closure::<dyn FnMut(MessageEvent)>::wrap(Box::new(move |event: MessageEvent| {
                    let data = event.data();
                    let messages = messages.clone();
                    let online_users = online_users.clone();
                    let active_ref = active_ref.clone();
                    let on_unread = on_unread.clone();
                    let error = error.clone();

                    if let Some(event) = parse_chat_event(data.clone()) {
                        let is_active = *active_ref.borrow();
                        apply_chat_event(
                            event,
                            &messages,
                            &online_users,
                            is_active,
                            &on_unread,
                        );
                        return;
                    }

                    if data.is_instance_of::<Blob>() {
                        let blob: Blob = data.unchecked_into();
                        let messages = messages.clone();
                        let online_users = online_users.clone();
                        let active_ref = active_ref.clone();
                        let on_unread = on_unread.clone();
                        let error = error.clone();
                        spawn_local(async move {
                            if let Ok(buf) = wasm_bindgen_futures::JsFuture::from(blob.array_buffer()).await {
                                if let Some(event) = parse_chat_event(buf) {
                                    let is_active = *active_ref.borrow();
                                    apply_chat_event(
                                        event,
                                        &messages,
                                        &online_users,
                                        is_active,
                                        &on_unread,
                                    );
                                } else {
                                    error.set(Some("Invalid chat event.".to_string()));
                                }
                            }
                        });
                        return;
                    }

                    error.set(Some("Invalid chat event.".to_string()));
                }))
            };

            ws.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
            onmessage.forget();

            let onerror = {
                let error = error.clone();
                Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(move |_event| {
                    error.set(Some("Chat connection error.".to_string()));
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

    let on_input = {
        let input = input.clone();
        Callback::from(move |event: InputEvent| {
            let target: web_sys::HtmlInputElement = event.target_unchecked_into();
            input.set(target.value());
        })
    };

    let on_send = {
        let input = input.clone();
        let error = error.clone();
        let server_ip = server_ip.clone();
        let server_port = server_port.clone();
        let token = token.clone();
        Callback::from(move |_| {
            let text = input.trim().to_string();
            if text.is_empty() {
                return;
            }
            input.set(String::new());
            let error = error.clone();
            let server_ip = server_ip.clone();
            let server_port = server_port.clone();
            let token = token.clone();
            spawn_local(async move {
                match post_message(&server_ip, &server_port, &token, &text).await {
                    Ok(_) => {}
                    Err(msg) => error.set(Some(msg)),
                }
            });
        })
    };

    let confirm = use_confirm();
    let on_delete = {
        let messages = messages.clone();
        let error = error.clone();
        let server_ip = server_ip.clone();
        let server_port = server_port.clone();
        let token = token.clone();
        let confirm = confirm.clone();
        let toast = toast.clone();
        Callback::from(move |id: String| {
            let messages = messages.clone();
            let error = error.clone();
            let server_ip = server_ip.clone();
            let server_port = server_port.clone();
            let token = token.clone();
            let toast = toast.clone();
            confirm.confirm(ConfirmRequest {
                title: "Delete message?".to_string(),
                message: "This will permanently remove the message.".to_string(),
                confirm_label: "Delete".to_string(),
                cancel_label: "Cancel".to_string(),
                on_confirm: Callback::from(move |_| {
                    let messages = messages.clone();
                    let error = error.clone();
                    let server_ip = server_ip.clone();
                    let server_port = server_port.clone();
                    let token = token.clone();
                    let id = id.clone();
                    let toast = toast.clone();
                    spawn_local(async move {
                        match delete_message(&server_ip, &server_port, &token, &id).await {
                            Ok(_) => {
                                let mut next = (*messages).clone();
                                next.retain(|msg| msg.id != id);
                                messages.set(next);
                                toast.toast("Message deleted.", ToastVariant::Success, Some(2000));
                            }
                            Err(msg) => error.set(Some(msg)),
                        }
                    });
                }),
            });
        })
    };

    let on_keydown = {
        let on_send = on_send.clone();
        Callback::from(move |event: KeyboardEvent| {
            if event.key() == "Enter" && !event.shift_key() {
                event.prevent_default();
                on_send.emit(());
            }
        })
    };

    let on_send_click = {
        let on_send = on_send.clone();
        Callback::from(move |_: MouseEvent| on_send.emit(()))
    };

    html! {
        <div class="flex h-full min-h-0 flex-col overflow-hidden">
            <div>
                <h1 class="text-3xl font-semibold">{ "Chat" }</h1>
            </div>
            if *loading {
                <div class="mt-6 rounded-2xl border border-ink/50 bg-inkLight p-6 text-base text-secondary/70">
                    { "Loading chat..." }
                </div>
            } else if !*enabled {
                <div class="mt-6 rounded-2xl border border-ink/50 bg-inkLight p-6 text-base text-secondary/70">
                    { "Chat is disabled on the backend." }
                </div>
            } else if let Some(msg) = (*error).clone() {
                <div class="mt-6 rounded-2xl border border-rose-400/60 bg-inkLight p-6 text-base text-secondary/80">
                    { msg }
                </div>
            } else {
                <div class="mt-6 h-[calc(100vh-18rem)] min-h-[18rem] overflow-hidden rounded-2xl border border-ink/50 bg-inkLight/90 p-6">
                    if messages.is_empty() {
                        <div class="flex h-full min-h-0 gap-6">
                            <aside class="w-48 shrink-0 rounded-xl border border-ink/50 bg-ink/40 p-3">
                                <p class="text-xs uppercase tracking-wide text-accent/80">{ "Online" }</p>
                                <div class="mt-3 flex flex-col gap-2 text-sm text-secondary/80">
                                    if online_users.is_empty() {
                                        <span class="text-secondary/60">{ "No one yet" }</span>
                                    } else {
                                        { for online_users.iter().map(|name| html! {
                                            <div class="flex items-center gap-2">
                                                <span class="h-2 w-2 rounded-full bg-primary" />
                                                <span>{ name }</span>
                                            </div>
                                        }) }
                                    }
                                </div>
                            </aside>
                            <div class="h-full min-h-0 flex-1 overflow-y-auto pr-2 scrollbar-thin">
                                <div class="text-base text-secondary/70">
                                    { "No messages yet. Start the conversation." }
                                </div>
                            </div>
                        </div>
                    } else {
                        <div class="flex h-full min-h-0 gap-6">
                            <aside class="w-48 shrink-0 rounded-xl border border-ink/50 bg-ink/40 p-3">
                                <p class="text-xs uppercase tracking-wide text-accent/80">{ "Online" }</p>
                                <div class="mt-3 flex flex-col gap-2 text-sm text-secondary/80">
                                    if online_users.is_empty() {
                                        <span class="text-secondary/60">{ "No one yet" }</span>
                                    } else {
                                        { for online_users.iter().map(|name| html! {
                                            <div class="flex items-center gap-2">
                                                <span class="h-2 w-2 rounded-full bg-primary" />
                                                <span>{ name }</span>
                                            </div>
                                        }) }
                                    }
                                </div>
                            </aside>
                            <div class="h-full min-h-0 flex-1 overflow-y-auto pr-2 scrollbar-thin">
                                <div class="flex flex-col gap-4">
                            { for messages.iter().map(|msg| {
                                let on_delete = on_delete.clone();
                                let msg_id = msg.id.clone();
                                let on_delete_click = Callback::from(move |_| {
                                    on_delete.emit(msg_id.clone())
                                });
                                        html! {
                                            <div key={msg.id.clone()} class="rounded-2xl border border-ink/60 bg-ink/30 p-4 shadow-lg">
                                                <div class="flex items-start justify-between gap-3">
                                                    <p class="text-xs uppercase tracking-wide text-accent/80">
                                                        { format!("{} · {}", msg.username, format_time(msg.timestamp)) }
                                                    </p>
                                            if is_admin {
                                                <Button
                                                    class={Some("bg-transparent px-0 py-0 text-primary/80 hover:text-primary hover:brightness-100 shadow-none".to_string())}
                                                    onclick={on_delete_click}
                                                >
                                                    <TrashIcon />
                                                </Button>
                                            }
                                        </div>
                                        <p class="mt-2 text-base text-secondary/95">
                                            { msg.message.clone() }
                                        </p>
                                    </div>
                                }
                            })}
                                </div>
                            </div>
                        </div>
                    }
                </div>
                <div class="mt-4 rounded-2xl border border-ink/50 bg-inkLight/90 p-4">
                    <div class="flex items-center gap-3">
                        <input
                            class="w-full rounded-xl border border-accent/60 bg-ink/40 px-4 py-3 text-base text-secondary placeholder:text-secondary/50 focus:outline-none focus:border-primary focus:ring-2 focus:ring-primary/40"
                            type="text"
                            placeholder="Type a message..."
                            value={(*input).clone()}
                            oninput={on_input}
                            onkeydown={on_keydown}
                        />
                        <Button
                            class={Some("rounded-xl px-4 py-3 text-sm".to_string())}
                            onclick={on_send_click}
                        >
                            <SendIcon />
                            { "Send" }
                        </Button>
                    </div>
                </div>
            }
        </div>
    }
}

fn parse_chat_event(data: JsValue) -> Option<ChatEvent> {
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

#[function_component(SendIcon)]
fn send_icon() -> Html {
    html! {
        <svg
            class="h-4 w-4"
            viewBox="0 0 24 24"
            fill="currentColor"
            aria-hidden="true"
        >
            <path d="M3.4 20.3l17.7-8.1c.8-.4.8-1.7 0-2.1L3.4 2c-.8-.4-1.7.3-1.5 1.2l2.4 7.6c.1.4.5.7.9.7l7.3.6c.5 0 .5.8 0 .8l-7.3.6c-.4 0-.8.3-.9.7l-2.4 7.6c-.3.9.7 1.6 1.5 1.2Z"/>
        </svg>
    }
}

#[function_component(TrashIcon)]
fn trash_icon() -> Html {
    html! {
        <svg
            class="h-4 w-4"
            viewBox="0 0 24 24"
            fill="currentColor"
            aria-hidden="true"
        >
            <path d="M9 3h6l1 2h4v2H4V5h4l1-2Zm1 6h2v9h-2V9Zm4 0h2v9h-2V9ZM7 9h2v9H7V9Z"/>
        </svg>
    }
}

#[derive(Clone, PartialEq, serde::Deserialize)]
struct ChatMessage {
    pub id: String,
    pub user_id: String,
    pub username: String,
    pub message: String,
    pub timestamp: i64,
}

#[derive(serde::Deserialize)]
struct FeaturesResponse {
    chat_enabled: bool,
}

#[derive(serde::Deserialize)]
struct ChatListResponse {
    messages: Vec<ChatMessage>,
}

fn apply_chat_event(
    event: ChatEvent,
    messages: &UseStateHandle<Vec<ChatMessage>>,
    online_users: &UseStateHandle<Vec<String>>,
    is_active: bool,
    on_unread: &Callback<usize>,
) {
    match event.event_type.as_str() {
        "snapshot" => {
            if let Some(list) = event.messages {
                messages.set(list);
            }
        }
        "presence" => {
            if let Some(users) = event.users {
                online_users.set(users);
            }
        }
        "message" => {
            if let Some(_) = event.message {
                if !is_active {
                    on_unread.emit(1);
                }
            }
        }
        _ => {}
    }
}

fn format_time(ts: i64) -> String {
    let secs = ts / 1000;
    let mins = (secs / 60) % 60;
    let hours = (secs / 3600) % 24;
    format!("{:02}:{:02}", hours, mins)
}

async fn fetch_features(server_ip: &str, server_port: &str) -> Result<bool, String> {
    let url = build_http_url(server_ip, server_port, "features");
    let data: FeaturesResponse = get_json(&url, None).await?;
    Ok(data.chat_enabled)
}

async fn fetch_messages(
    server_ip: &str,
    server_port: &str,
    token: &str,
) -> Result<Vec<ChatMessage>, String> {
    let url = build_http_url(server_ip, server_port, "chat/messages");
    let data: ChatListResponse = get_json(&url, Some(token)).await?;
    Ok(data.messages)
}

async fn post_message(
    server_ip: &str,
    server_port: &str,
    token: &str,
    message: &str,
) -> Result<ChatMessage, String> {
    let url = build_http_url(server_ip, server_port, "chat/messages");
    let body = serde_json::json!({ "message": message });
    let resp = send_json("POST", &url, Some(token), Some(body)).await?;
    if !resp.ok() {
        return Err(format!("Chat error (HTTP {}).", resp.status()));
    }
    let json = wasm_bindgen_futures::JsFuture::from(
        resp.json()
            .map_err(|_| "Failed to parse chat JSON.".to_string())?,
    )
    .await
    .map_err(|_| "Failed to parse chat JSON.".to_string())?;
    let data: ChatMessage =
        serde_wasm_bindgen::from_value(json).map_err(|_| "Invalid chat response.".to_string())?;

    Ok(data)
}

async fn delete_message(
    server_ip: &str,
    server_port: &str,
    token: &str,
    id: &str,
) -> Result<(), String> {
    let url = build_http_url(server_ip, server_port, &format!("admin/chat/messages/{id}"));
    let resp = send_json("DELETE", &url, Some(token), None).await?;
    if !resp.ok() {
        return Err(format!("Delete error (HTTP {}).", resp.status()));
    }

    Ok(())
}

#[derive(Clone, PartialEq, serde::Deserialize)]
struct ChatEvent {
    #[serde(rename = "type")]
    event_type: String,
    message: Option<ChatMessage>,
    messages: Option<Vec<ChatMessage>>,
    deleted_id: Option<String>,
    users: Option<Vec<String>>,
}
