use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api::{get_json, send_json};
use crate::app::AppState;
use crate::confirm::{use_confirm, ConfirmRequest};
use crate::toast::{use_toast, ToastVariant};

#[function_component(AdminScreen)]
pub fn admin_screen() -> Html {
    let app_state = use_context::<UseStateHandle<AppState>>()
        .expect("AppState context not found. Ensure AdminScreen is under <ContextProvider>.");
    let server_ip = app_state.server_ip.clone().unwrap_or_default();
    let token = app_state.session_token.clone().unwrap_or_default();

    let sessions = use_state(|| None::<i64>);
    let error = use_state(|| None::<String>);
    let loading = use_state(|| true);
    let clearing = use_state(|| false);
    let confirm = use_confirm();
    let toast = use_toast();

    {
        let sessions = sessions.clone();
        let error = error.clone();
        let loading = loading.clone();
        let server_ip = server_ip.clone();
        let token = token.clone();
        use_effect_with((server_ip.clone(), token.clone()), move |_| {
            if server_ip.is_empty() || token.is_empty() {
                loading.set(false);
                return ();
            }

            spawn_local(async move {
                match fetch_admin_stats(&server_ip, &token).await {
                    Ok(count) => sessions.set(Some(count)),
                    Err(msg) => error.set(Some(msg)),
                }
                loading.set(false);
            });

            ()
        });
    }

    let on_clear_chat = {
        let clearing = clearing.clone();
        let error = error.clone();
        let server_ip = server_ip.clone();
        let token = token.clone();
        let confirm = confirm.clone();
        let toast = toast.clone();
        Callback::from(move |_| {
            if server_ip.is_empty() || token.is_empty() {
                return;
            }
            let clearing = clearing.clone();
            let error = error.clone();
            let server_ip = server_ip.clone();
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
                    let token = token.clone();
                    let toast = toast.clone();
                    spawn_local(async move {
                        if let Err(msg) = clear_chat(&server_ip, &token).await {
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

    html! {
        <div>
            <h1 class="text-2xl font-semibold">{ "Admin" }</h1>
            <p class="mt-2 text-base text-accent">
                { "Admin-only tools and diagnostics." }
            </p>
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
                        <p class="mt-4 text-lg font-semibold">{ "—" }</p>
                        <p class="mt-2 text-sm text-secondary/70">{ "No data." }</p>
                    }
                </div>
                <div class="rounded-2xl border border-ink/50 bg-inkLight p-6">
                    <p class="text-xs uppercase tracking-wide text-accent/80">{ "Feature Flags" }</p>
                    <p class="mt-4 text-lg font-semibold">{ "Chat" }</p>
                    <p class="mt-2 text-sm text-secondary/70">{ "Controlled by backend config." }</p>
                    <button
                        class="mt-4 inline-flex items-center gap-2 rounded-xl border border-rose-400/60 bg-rose-500/20 px-3 py-2 text-xs font-semibold text-rose-100 transition hover:bg-rose-500/30"
                        type="button"
                        onclick={on_clear_chat}
                        disabled={*clearing}
                    >
                        { if *clearing { "Clearing..." } else { "Clear chat messages" } }
                    </button>
                </div>
            </div>
        </div>
    }
}

#[derive(serde::Deserialize)]
struct AdminStatsResponse {
    sessions: i64,
}

async fn fetch_admin_stats(server_ip: &str, token: &str) -> Result<i64, String> {
    let url = format!("http://{server_ip}:2121/admin/stats");
    let data: AdminStatsResponse = get_json(&url, Some(token)).await?;
    Ok(data.sessions)
}

async fn clear_chat(server_ip: &str, token: &str) -> Result<(), String> {
    let url = format!("http://{server_ip}:2121/admin/chat/messages");
    let resp = send_json("DELETE", &url, Some(token), None).await?;
    if !resp.ok() {
        return Err(format!("Clear chat error (HTTP {}).", resp.status()));
    }
    Ok(())
}
