use yew::prelude::*;

use crate::app::{apply_theme, fetch_theme};
use crate::auth::{get_local_storage_item, set_local_storage_item, INSTALL_DIR_KEY};
use crate::components::Button;
use crate::toast::{use_toast, ToastVariant};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

#[derive(Properties, PartialEq)]
pub struct SettingsScreenProps {
    pub on_logout: Callback<MouseEvent>,
    pub user: Option<crate::app::User>,
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "core"])]
    async fn invoke(cmd: &str, args: JsValue) -> JsValue;
}

#[function_component(SettingsScreen)]
pub fn settings_screen(props: &SettingsScreenProps) -> Html {
    let app_state = use_context::<UseStateHandle<crate::app::AppState>>()
        .expect("AppState context not found. Ensure SettingsScreen is under <ContextProvider>.");
    let server_ip = app_state.server_ip.clone();
    let server_port = app_state.server_port.clone();
    let toast = use_toast();
    let install_dir = use_state(|| get_local_storage_item(INSTALL_DIR_KEY).unwrap_or_default());

    {
        let install_dir = install_dir.clone();
        let toast = toast.clone();
        use_effect_with((), move |_| {
            if !install_dir.is_empty() {
                return ();
            }
            spawn_local(async move {
                match invoke("get_default_apps_dir", JsValue::NULL).await.as_string() {
                    Some(path) => {
                        set_local_storage_item(INSTALL_DIR_KEY, &path);
                        install_dir.set(path);
                    }
                    None => toast.toast("Failed to load default install folder.", ToastVariant::Error, Some(3000)),
                }
            });
            ()
        });
    }
    let on_reload_theme = {
        let server_ip = server_ip.clone();
        let server_port = server_port.clone();
        let toast = toast.clone();
        Callback::from(move |_| {
            let toast = toast.clone();
            if let (Some(server_ip), Some(server_port)) = (server_ip.clone(), server_port.clone()) {
                spawn_local(async move {
                    match fetch_theme(&server_ip, &server_port).await {
                        Ok(theme) => {
                            apply_theme(&theme);
                            toast.toast("Theme reloaded.", ToastVariant::Success, Some(2000));
                        }
                        Err(msg) => {
                            toast.toast(msg, ToastVariant::Error, Some(3000));
                        }
                    }
                });
            } else {
                toast.toast(
                    "No server address found.",
                    ToastVariant::Warning,
                    Some(2500),
                );
            }
        })
    };

    let on_install_dir_input = {
        let install_dir = install_dir.clone();
        Callback::from(move |event: InputEvent| {
            let input: web_sys::HtmlInputElement = event.target_unchecked_into();
            let value = input.value();
            set_local_storage_item(INSTALL_DIR_KEY, &value);
            install_dir.set(value);
        })
    };

    let on_browse = {
        let install_dir = install_dir.clone();
        let toast = toast.clone();
        Callback::from(move |_| {
            let install_dir = install_dir.clone();
            let toast = toast.clone();
            spawn_local(async move {
                let result = invoke("pick_install_dir", JsValue::NULL).await;
                if let Some(path) = result.as_string() {
                    if !path.is_empty() {
                        set_local_storage_item(INSTALL_DIR_KEY, &path);
                        install_dir.set(path);
                        return;
                    }
                }
                toast.toast("No folder selected.", ToastVariant::Warning, Some(2000));
            });
        })
    };

    html! {
        <div>
            <h1 class="text-2xl font-semibold">{ "Settings" }</h1>
            <p class="mt-2 text-sm text-accent">
                { "Manage preferences, accounts, and access." }
            </p>
            <div class="mt-6 rounded-2xl border border-ink/50 bg-inkLight p-6">
                <div class="flex items-center justify-between gap-4">
                    <div>
                        <h2 class="text-sm font-semibold">{ "Install Folder" }</h2>
                        <p class="mt-2 text-sm text-secondary/70">
                            { "Where downloaded apps are stored." }
                        </p>
                    </div>
                    <Button
                        class={Some("border border-accent/50 bg-accent/20 text-secondary hover:bg-accent/30".to_string())}
                        onclick={on_browse}
                    >
                        { "Browse" }
                    </Button>
                </div>
                <input
                    class="mt-4 w-full rounded border border-ink/50 bg-ink/40 px-3 py-2 text-secondary placeholder:text-secondary/60 outline outline-1 outline-accent/50 focus:outline-none focus:ring-2 focus:ring-primary/40"
                    type="text"
                    placeholder="Apps folder"
                    value={(*install_dir).clone()}
                    oninput={on_install_dir_input}
                />
            </div>
            <div class="mt-8 rounded-2xl border border-ink/50 bg-inkLight p-6">
                <div class="flex items-center justify-between">
                    <div>
                        <h2 class="text-sm font-semibold">{ "Account" }</h2>
                        <p class="mt-2 text-sm text-secondary/70">
                            { "Signed in with Discord." }
                        </p>
                        if let Some(user) = props.user.clone() {
                            <div class="mt-4 text-sm text-secondary/80">
                                <p>{ format!("Username: {}", user.username) }</p>
                                <p class="mt-1">{ format!("User ID: {}", user.id) }</p>
                                <p class="mt-1">{ format!("Role: {}", if user.is_admin { "Admin" } else { "User" }) }</p>
                            </div>
                        } else {
                            <div class="mt-4 text-sm text-secondary/60">
                                { "User info not loaded." }
                            </div>
                        }
                    </div>
                    <Button onclick={props.on_logout.clone()}>
                        { "Log out" }
                    </Button>
                </div>
            </div>
            <div class="mt-6 rounded-2xl border border-ink/50 bg-inkLight p-6">
                <div class="flex items-center justify-between">
                    <div>
                        <h2 class="text-sm font-semibold">{ "Theme" }</h2>
                        <p class="mt-2 text-sm text-secondary/70">
                            { "Reload colors and font from the backend." }
                        </p>
                    </div>
                    <Button onclick={on_reload_theme}>
                        { "Reload theme" }
                    </Button>
                </div>
            </div>
        </div>
    }
}
