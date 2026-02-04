use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;
use web_sys::{console, HtmlInputElement};
use yew::prelude::*;

use crate::auth::{
    clear_query_param, get_local_storage_item, get_query_param, handle_login, handle_logout,
    remove_local_storage_item, set_local_storage_item, LOGIN_SUCCESS_KEY, SERVER_IP_KEY,
    SESSION_TOKEN_KEY,
};
use crate::components::{Button, Card};
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
    pub session_token: Option<String>,
}

type AppStateHandle = UseStateHandle<AppState>;

#[function_component(App)]
pub fn app() -> Html {
    let app_state = use_state(|| AppState {
        logged_in: false,
        server_ip: None,
        session_token: None,
    });

    {
        let app_state = app_state.clone();
        use_effect_with(
            (),
            move |_| {
                let server_ip = get_local_storage_item(SERVER_IP_KEY);
                let token_from_query = get_query_param("token");
                if let Some(token) = token_from_query.as_deref() {
                    set_local_storage_item(SESSION_TOKEN_KEY, token);
                    set_local_storage_item(LOGIN_SUCCESS_KEY, "1");
                }
                let token =
                    token_from_query.clone().or_else(|| get_local_storage_item(SESSION_TOKEN_KEY));

                if token.is_some() || server_ip.is_some() {
                    app_state.set(AppState {
                        logged_in: token.is_some(),
                        server_ip,
                        session_token: token,
                    });
                    if token_from_query.is_some() {
                        clear_query_param("token");
                    }
                }
                || ()
            },
        );
    }

    html! {
        <ToastProvider>
            <ContextProvider<AppStateHandle> context={app_state}>
                <AppRouter />
            </ContextProvider<AppStateHandle>>
        </ToastProvider>
    }
}

#[function_component(AppRouter)]
fn app_router() -> Html {
    let app_state = use_context::<AppStateHandle>()
        .expect("AppState context not found. Ensure AppRouter is under <ContextProvider>.");
    let toast = use_toast();

    {
        let toast = toast.clone();
        use_effect_with(
            (),
            move |_| {
                if get_local_storage_item(LOGIN_SUCCESS_KEY).is_some() {
                    remove_local_storage_item(LOGIN_SUCCESS_KEY);
                    toast.toast("Logged in successfully.", ToastVariant::Success, Some(2500));
                }
                || ()
            },
        );
    }

    if app_state.logged_in {
        html! { <MainScreen /> }
    } else {
        html! { <LoginScreen /> }
    }
}

#[function_component(LoginScreen)]
fn login_screen() -> Html {
    let server_ip = use_state(|| get_local_storage_item(SERVER_IP_KEY).unwrap_or_default());
    let toast = use_toast();
    let on_ip_input = {
        let server_ip = server_ip.clone();
        Callback::from(move |event: InputEvent| {
            let input: HtmlInputElement = event.target_unchecked_into();
            server_ip.set(input.value());
            console::log_1(&"login: ip input changed".into());
        })
    };
    let on_login_click = {
        let server_ip = server_ip.clone();
        let toast = toast.clone();
        Callback::from(move |_| {
            let ip = server_ip.as_str();
            if ip.trim().is_empty() {
                toast.toast(
                    "Please enter your server IP before logging in.",
                    ToastVariant::Warning,
                    Some(3000),
                );
                return;
            }
            handle_login(ip);
        })
    };

    html! {
        <main class="min-h-screen text-secondary flex items-center justify-center">
            <Card highlight={true}>
                <h1 class="text-3xl font-bold">
                    { "Tailwind v4 + Yew + Tauri" }
                </h1>
                <p class="mt-2 text-sm text-accent">
                    { "Enter your server IP to continue." }
                </p>
                <label class="mt-6 block text-xs uppercase tracking-wide text-accent/80">
                    { "Server IP" }
                </label>
                <input
                    class="mt-2 w-full rounded border border-ink/50 bg-ink/50 px-3 py-2 text-secondary placeholder:text-secondary/60 focus:outline-none focus:ring-2 focus:ring-primary/40"
                    type="text"
                    placeholder="192.168.1.10"
                    value={(*server_ip).clone()}
                    oninput={on_ip_input}
                />
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

#[function_component(MainScreen)]
fn main_screen() -> Html {
    let app_state = use_context::<AppStateHandle>()
        .expect("AppState context not found. Ensure MainScreen is under <ContextProvider>.");
    let toast = use_toast();
    let on_logout = {
        let app_state = app_state.clone();
        Callback::from(move |_| handle_logout(app_state.clone()))
    };
    let on_test_toast = {
        let toast = toast.clone();
        Callback::from(move |_| {
            toast.toast("Test toast: everything is wired.", ToastVariant::Info, Some(3000));
        })
    };

    html! {
        <main class="min-h-screen text-secondary flex items-center justify-center">
            <Card highlight={true}>
                <h1 class="text-3xl font-bold">
                    { "Welcome back!" }
                </h1>
                <p class="mt-2 text-sm text-accent">
                    { "You're signed in." }
                </p>
                <Button class={Some("mt-6".to_string())} onclick={on_test_toast}>
                    { "Show test toast" }
                </Button>
                <Button class={Some("mt-4".to_string())} onclick={on_logout}>
                    { "Log out" }
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
