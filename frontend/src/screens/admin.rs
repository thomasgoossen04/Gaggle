use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlTextAreaElement;
use yew::prelude::*;

use crate::api::{get_json, send_json};
use crate::app::AppState;
use crate::components::Button;
use crate::confirm::{use_confirm, ConfirmRequest};
use crate::net::build_http_url;
use crate::toast::{use_toast, ToastVariant};

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
    let confirm = use_confirm();
    let toast = use_toast();
    let reloading = use_state(|| false);
    let config_open = use_state(|| false);
    let config_loading = use_state(|| false);
    let config_saving = use_state(|| false);
    let config_raw = use_state(String::new);

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

    html! {
        <div>
            <div>
                <h1 class="text-2xl font-semibold">{ "Admin" }</h1>
                <p class="mt-2 text-base text-accent">
                    { "Admin-only tools and diagnostics." }
                </p>
            </div>
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
        </div>
    }
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
