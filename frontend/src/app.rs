use futures_util::stream::StreamExt;
use gloo_timers::future::IntervalStream;
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;
use web_sys::{console, HtmlInputElement};
use yew::prelude::*;

use crate::api::send_request;
use crate::auth::{
    check_session, clear_query_param, fetch_me, get_local_storage_item, get_query_param,
    handle_login, handle_logout, remove_local_storage_item, set_local_storage_item,
    LOGIN_SUCCESS_KEY, SERVER_IP_KEY, SERVER_PORT_KEY, SESSION_TOKEN_KEY,
};
use crate::components::{Button, Card};
use crate::confirm::ConfirmProvider;
use crate::net::build_http_url;
use crate::screens::dashboard::Dashboard;
use crate::screens::error::ErrorScreen;
use crate::toast::{use_toast, ToastProvider, ToastVariant};

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "core"])]
    async fn invoke(cmd: &str, args: JsValue) -> JsValue;
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct AppState {
    pub logged_in: bool,
    pub server_ip: Option<String>,
    pub server_port: Option<String>,
    pub session_token: Option<String>,
    pub auth_error: Option<String>,
    pub user: Option<User>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct User {
    pub id: String,
    pub username: String,
    pub is_admin: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Theme {
    pub primary: String,
    pub secondary: String,
    pub accent: String,
    pub ink: String,
    pub ink_light: String,
    pub font: String,
    #[serde(default)]
    pub radius: Option<String>,
}

type AppStateHandle = UseStateHandle<AppState>;

#[function_component(App)]
pub fn app() -> Html {
    let app_state = use_state(|| AppState {
        logged_in: false,
        server_ip: None,
        server_port: None,
        session_token: None,
        auth_error: None,
        user: None,
    });

    {
        let app_state = app_state.clone();
        use_effect_with((), move |_| {
            let server_ip = get_local_storage_item(SERVER_IP_KEY);
            let server_port =
                get_local_storage_item(SERVER_PORT_KEY).or_else(|| Some("2121".to_string()));
            let token_from_query = get_query_param("token");
            if let Some(token) = token_from_query.as_deref() {
                set_local_storage_item(SESSION_TOKEN_KEY, token);
                set_local_storage_item(LOGIN_SUCCESS_KEY, "1");
            }
            let token = token_from_query
                .clone()
                .or_else(|| get_local_storage_item(SESSION_TOKEN_KEY));

            if token.is_some() || server_ip.is_some() {
                app_state.set(AppState {
                    logged_in: token.is_some(),
                    server_ip,
                    server_port,
                    session_token: token,
                    auth_error: None,
                    user: None,
                });
                if token_from_query.is_some() {
                    clear_query_param("token");
                }
            }
            || ()
        });
    }

    {
        let server_ip = app_state.server_ip.clone();
        let server_port = app_state.server_port.clone();
        use_effect_with((server_ip, server_port), move |(server_ip, server_port)| {
            if let (Some(server_ip), Some(server_port)) = (server_ip.clone(), server_port.clone()) {
                spawn_local(async move {
                    if let Ok(theme) = fetch_theme(&server_ip, &server_port).await {
                        apply_theme(&theme);
                    }
                });
            }
            ()
        });
    }

    html! {
        <ToastProvider>
            <ConfirmProvider>
                <ContextProvider<AppStateHandle> context={app_state}>
                    <AppRouter />
                </ContextProvider<AppStateHandle>>
            </ConfirmProvider>
        </ToastProvider>
    }
}

pub async fn fetch_theme(server_ip: &str, server_port: &str) -> Result<Theme, String> {
    let url = build_http_url(server_ip, server_port, "theme");
    let resp = send_request("GET", &url, None, None).await?;
    if resp.status() == 204 {
        return Err("No theme configured.".to_string());
    }
    if !resp.ok() {
        return Err(format!("Theme error (HTTP {}).", resp.status()));
    }
    let json = wasm_bindgen_futures::JsFuture::from(
        resp.json()
            .map_err(|_| "Failed to parse theme JSON.".to_string())?,
    )
    .await
    .map_err(|_| "Failed to parse theme JSON.".to_string())?;
    let theme: Theme =
        serde_wasm_bindgen::from_value(json).map_err(|_| "Invalid theme response.".to_string())?;
    Ok(theme)
}

pub fn apply_theme(theme: &Theme) {
    if let Some(win) = web_sys::window() {
        if let Some(doc) = win.document() {
            let font_stack = format!("\"{}\", ui-sans-serif, system-ui, sans-serif", theme.font);
            let style = format!(
                "--color-primary:{};\
                --color-secondary:{};\
                --color-accent:{};\
                --color-ink:{};\
                --color-inkLight:{};\
                --font-sans:{};\
                --radius-base:{};",
                theme.primary,
                theme.secondary,
                theme.accent,
                theme.ink,
                theme.ink_light,
                font_stack,
                theme
                    .radius
                    .clone()
                    .unwrap_or_else(|| "18px".to_string())
            );
            if let Some(root) = doc.document_element() {
                let _ = root.set_attribute("style", &style);
            }
            if let Some(body) = doc.body() {
                let _ = body.set_attribute("style", &style);
            }
        }
    }
}

#[function_component(AppRouter)]
fn app_router() -> Html {
    let app_state = use_context::<AppStateHandle>()
        .expect("AppState context not found. Ensure AppRouter is under <ContextProvider>.");
    let toast = use_toast();

    {
        let toast = toast.clone();
        use_effect_with((), move |_| {
            if get_local_storage_item(LOGIN_SUCCESS_KEY).is_some() {
                remove_local_storage_item(LOGIN_SUCCESS_KEY);
                toast.toast("Logged in successfully.", ToastVariant::Success, Some(2500));
            }
            || ()
        });
    }

    {
        let app_state = app_state.clone();
        let cancelled = use_mut_ref(|| false);
        use_effect_with(
            (
                app_state.logged_in,
                app_state.server_ip.clone(),
                app_state.server_port.clone(),
                app_state.session_token.clone(),
                app_state.auth_error.clone(),
            ),
            move |_| {
                *cancelled.borrow_mut() = false;
                if !app_state.logged_in
                    || app_state.server_ip.is_none()
                    || app_state.server_port.is_none()
                    || app_state.session_token.is_none()
                    || app_state.auth_error.is_some()
                {
                    let cancelled = cancelled.clone();
                    return Box::new(move || {
                        *cancelled.borrow_mut() = true;
                    }) as Box<dyn FnOnce()>;
                }

                let server_ip = app_state.server_ip.clone().unwrap();
                let server_port = app_state.server_port.clone().unwrap();
                let token = app_state.session_token.clone().unwrap();
                let app_state_interval = app_state.clone();
                let cancelled_task = cancelled.clone();
                let cancelled_cleanup = cancelled.clone();

                spawn_local(async move {
                    if *cancelled_task.borrow() {
                        return;
                    }
                    if app_state_interval.user.is_none() {
                        match fetch_me(&server_ip, &server_port, &token).await {
                            Ok(user) => {
                                let mut next = (*app_state_interval).clone();
                                next.user = Some(user);
                                app_state_interval.set(next);
                            }
                            Err(message) => {
                                let mut next = (*app_state_interval).clone();
                                next.auth_error = Some(message);
                                app_state_interval.set(next);
                                return;
                            }
                        }
                    }

                    if let Err(message) = check_session(&server_ip, &server_port, &token).await {
                        let mut next = (*app_state_interval).clone();
                        next.auth_error = Some(message);
                        app_state_interval.set(next);
                        return;
                    }

                    let mut interval = IntervalStream::new(30_000);
                    while interval.next().await.is_some() {
                        if *cancelled_task.borrow() {
                            break;
                        }
                        if let Err(message) = check_session(&server_ip, &server_port, &token).await
                        {
                            let mut next = (*app_state_interval).clone();
                            next.auth_error = Some(message);
                            app_state_interval.set(next);
                            break;
                        }
                    }
                });

                Box::new(move || {
                    *cancelled_cleanup.borrow_mut() = true;
                }) as Box<dyn FnOnce()>
            },
        );
    }

    if let Some(message) = app_state.auth_error.clone() {
        let on_logout = {
            let app_state = app_state.clone();
            Callback::from(move |_| handle_logout(app_state.clone()))
        };
        let on_retry = {
            let app_state = app_state.clone();
            Callback::from(move |_| {
                let app_state = app_state.clone();
                let server_ip = app_state.server_ip.clone();
                let server_port = app_state.server_port.clone();
                let token = app_state.session_token.clone();
                if let (Some(server_ip), Some(server_port), Some(token)) =
                    (server_ip, server_port, token)
                {
                    spawn_local(async move {
                        if let Ok(user) = fetch_me(&server_ip, &server_port, &token).await {
                            let mut next = (*app_state).clone();
                            next.user = Some(user);
                            next.auth_error = None;
                            app_state.set(next);
                        } else if let Err(message) =
                            check_session(&server_ip, &server_port, &token).await
                        {
                            let mut next = (*app_state).clone();
                            next.auth_error = Some(message);
                            app_state.set(next);
                        }
                    });
                }
            })
        };
        return html! { <ErrorScreen message={message} on_logout={on_logout} on_retry={on_retry} /> };
    }

    if app_state.logged_in {
        html! { <Dashboard /> }
    } else {
        html! { <LoginScreen /> }
    }
}

#[function_component(LoginScreen)]
fn login_screen() -> Html {
    let server_ip = use_state(|| get_local_storage_item(SERVER_IP_KEY).unwrap_or_default());
    let password = use_state(String::new);
    let password_required = use_state(|| false);
    let toast = use_toast();
    let feature_req_id = use_mut_ref(|| 0u64);
    let on_ip_input = {
        let server_ip = server_ip.clone();
        Callback::from(move |event: InputEvent| {
            let input: HtmlInputElement = event.target_unchecked_into();
            server_ip.set(input.value());
            console::log_1(&"login: ip input changed".into());
        })
    };

    {
        let server_ip = server_ip.clone();
        let password_required = password_required.clone();
        let feature_req_id = feature_req_id.clone();
        use_effect_with(server_ip.clone(), move |server_ip| {
            let server_ip = server_ip.clone();
            if server_ip.trim().is_empty() {
                password_required.set(false);
                return ();
            }
            *feature_req_id.borrow_mut() += 1;
            let req_id = *feature_req_id.borrow();
            spawn_local(async move {
                let features_url = build_http_url(&server_ip, "2121", "features");
                let mut requires_password = false;
                if let Ok(resp) = send_request("GET", &features_url, None, None).await {
                    if resp.ok() {
                        if let Ok(json) = resp.json() {
                            if let Ok(value) = wasm_bindgen_futures::JsFuture::from(json).await {
                                if let Ok(features) =
                                    serde_wasm_bindgen::from_value::<serde_json::Value>(value)
                                {
                                    requires_password = features
                                        .get("login_password_required")
                                        .and_then(|v| v.as_bool())
                                        .unwrap_or(false);
                                }
                            }
                        }
                    }
                }
                if *feature_req_id.borrow() == req_id {
                    password_required.set(requires_password);
                }
            });
            ()
        });
    }
    let on_login_click = {
        let server_ip = server_ip.clone();
        let password = password.clone();
        let password_required = password_required.clone();
        let toast = toast.clone();
        Callback::from(move |_| {
            let ip = server_ip.as_str().trim().to_string();
            if ip.is_empty() {
                toast.toast(
                    "Please enter your server URL before logging in.",
                    ToastVariant::Warning,
                    Some(3000),
                );
                return;
            }
            let password_value = password.as_str().trim().to_string();

            let toast = toast.clone();
            let password_required = password_required.clone();
            spawn_local(async move {
                let features_url = build_http_url(&ip, "2121", "features");
                let mut requires_password = false;
                if let Ok(resp) = send_request("GET", &features_url, None, None).await {
                    if resp.ok() {
                        if let Ok(json) = resp.json() {
                            if let Ok(value) = wasm_bindgen_futures::JsFuture::from(json).await {
                                if let Ok(features) =
                                    serde_wasm_bindgen::from_value::<serde_json::Value>(value)
                                {
                                    requires_password = features
                                        .get("login_password_required")
                                        .and_then(|v| v.as_bool())
                                        .unwrap_or(false);
                                }
                            }
                        }
                    }
                }
                password_required.set(requires_password);
                if requires_password && password_value.is_empty() {
                    toast.toast(
                        "This server requires a login password.",
                        ToastVariant::Warning,
                        Some(3000),
                    );
                    return;
                }

                let ping_url = build_http_url(&ip, "2121", "ping");
                match send_request("GET", &ping_url, None, None).await {
                    Ok(resp) if resp.ok() => handle_login(&ip, Some(&password_value)),
                    Ok(resp) => {
                        toast.toast(
                            &format!("Backend error (HTTP {}).", resp.status()),
                            ToastVariant::Error,
                            Some(3000),
                        );
                    }
                    Err(_) => {
                        toast.toast(
                            "Backend could not be reached.",
                            ToastVariant::Error,
                            Some(3000),
                        );
                    }
                }
            });
        })
    };
    let on_password_input = {
        let password = password.clone();
        Callback::from(move |event: InputEvent| {
            let input: HtmlInputElement = event.target_unchecked_into();
            password.set(input.value());
        })
    };

    html! {
        <main class="min-h-screen text-secondary flex items-center justify-center">
            <Card highlight={true}>
                <h1 class="text-4xl font-bold">
                    { "Gaggle launcher" }
                </h1>
                <p class="mt-2 text-sm text-accent">
                    { "Enter your server URL to continue." }
                </p>
                <div class="mt-6 text-xs uppercase tracking-wide text-accent/80">
                    { "Server URL" }
                </div>
                <div class="mt-2">
                    <input
                        class="w-full rounded border border-ink/50 bg-ink/50 px-3 py-2 text-secondary placeholder:text-secondary/60 outline outline-1 outline-accent/50 focus:outline-none focus:ring-2 focus:ring-primary/40"
                        type="text"
                        placeholder="https://gaggle.example.com"
                        value={(*server_ip).clone()}
                        oninput={on_ip_input}
                    />
                </div>
                if *password_required {
                    <div class="mt-4 text-xs uppercase tracking-wide text-accent/80">
                        { "Access Password" }
                    </div>
                    <div class="mt-2">
                        <input
                            class="w-full rounded border border-ink/50 bg-ink/50 px-3 py-2 text-secondary placeholder:text-secondary/60 outline outline-1 outline-accent/50 focus:outline-none focus:ring-2 focus:ring-primary/40"
                            type="password"
                            placeholder="Enter server access password"
                            value={(*password).clone()}
                            oninput={on_password_input}
                        />
                    </div>
                }
                <Button
                    class={Some("mt-6 relative z-10".to_string())}
                    onclick={on_login_click}
                >
                    <DiscordIcon />
                    { "Log in with Discord" }
                </Button>
            </Card>
        </main>
    }
}

#[function_component(DiscordIcon)]
fn discord_icon() -> Html {
    html! {
        <svg
            class="h-5 w-5 pointer-events-none"
            viewBox="0 0 640 512"
            fill="currentColor"
            aria-hidden="true"
        >
            <path d="M524.5 69.8a1.5 1.5 0 0 0-.8-.7A485.1 485.1 0 0 0 404.1 32a1.8 1.8 0 0 0-2 1 337.5 337.5 0 0 0-14.9 30.6 447.8 447.8 0 0 0-134.4 0A309.5 309.5 0 0 0 237.1 33a1.7 1.7 0 0 0-2-1 483.8 483.8 0 0 0-119.8 37.1 1.7 1.7 0 0 0-.8.7C39.5 183.6 18.7 294.7 29.1 405.1a2 2 0 0 0 .8 1.4A487.7 487.7 0 0 0 171.3 477a1.7 1.7 0 0 0 2.3-.7 348.2 348.2 0 0 0 30.1-49.1 1.8 1.8 0 0 0-1-2.5 321.2 321.2 0 0 1-45.5-21.6 1.8 1.8 0 0 1-.2-3 253.1 253.1 0 0 0 9.4-7.5 1.8 1.8 0 0 1 1.9-.2c95.1 43.5 198.5 43.5 292.5 0a1.8 1.8 0 0 1 2 .2 214.5 214.5 0 0 0 9.3 7.5 1.8 1.8 0 0 1-.2 3 301.4 301.4 0 0 1-45.6 21.6 1.8 1.8 0 0 0-1 2.5 391.1 391.1 0 0 0 30.1 49.1 1.6 1.6 0 0 0 2.3.7A486 486 0 0 0 610.1 406a1.6 1.6 0 0 0 .8-1.4c12.4-123.7-20.7-236.2-86.4-334.8ZM222.9 337.6c-28.4 0-51.5-25.8-51.5-57.5s22.8-57.6 51.5-57.6c28.9 0 52 26.1 51.5 57.6 0 31.7-22.8 57.5-51.5 57.5Zm186.2 0c-28.4 0-51.5-25.8-51.5-57.5s22.8-57.6 51.5-57.6c28.9 0 52 26.1 51.5 57.6 0 31.7-22.8 57.5-51.5 57.5Z"/>
        </svg>
    }
}
