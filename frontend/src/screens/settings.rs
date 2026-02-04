use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::app::{apply_theme, fetch_theme};
use crate::components::Button;
use crate::toast::{use_toast, ToastVariant};

#[derive(Properties, PartialEq)]
pub struct SettingsScreenProps {
    pub on_logout: Callback<MouseEvent>,
    pub user: Option<crate::app::User>,
}

#[function_component(SettingsScreen)]
pub fn settings_screen(props: &SettingsScreenProps) -> Html {
    let app_state = use_context::<UseStateHandle<crate::app::AppState>>()
        .expect("AppState context not found. Ensure SettingsScreen is under <ContextProvider>.");
    let server_ip = app_state.server_ip.clone();
    let toast = use_toast();
    let on_reload_theme = {
        let server_ip = server_ip.clone();
        let toast = toast.clone();
        Callback::from(move |_| {
            let toast = toast.clone();
            if let Some(server_ip) = server_ip.clone() {
                spawn_local(async move {
                    match fetch_theme(&server_ip).await {
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
                toast.toast("No server IP found.", ToastVariant::Warning, Some(2500));
            }
        })
    };

    html! {
        <div>
            <h1 class="text-2xl font-semibold">{ "Settings" }</h1>
            <p class="mt-2 text-sm text-accent">
                { "Manage preferences, accounts, and access." }
            </p>
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
