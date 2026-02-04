use yew::prelude::*;

use crate::app::AppState;
use crate::auth::handle_logout;
use crate::components::Button;
use wasm_bindgen_futures::spawn_local;
use crate::api::get_json;
use crate::screens::{
    admin::AdminScreen, chat::ChatScreen, library::LibraryScreen, settings::SettingsScreen,
};

type AppStateHandle = UseStateHandle<AppState>;

#[derive(serde::Deserialize)]
struct FeaturesResponse {
    chat_enabled: bool,
}

async fn fetch_features(server_ip: &str) -> Result<FeaturesResponse, String> {
    let url = format!("http://{server_ip}:2121/features");
    get_json(&url, None).await
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Library,
    Chat,
    Settings,
    Admin,
}

#[function_component(Dashboard)]
pub fn dashboard() -> Html {
    let app_state = use_context::<AppStateHandle>()
        .expect("AppState context not found. Ensure Dashboard is under <ContextProvider>.");
    let active_tab = use_state(|| Tab::Library);
    let unread_chat = use_state(|| 0usize);
    let chat_enabled = use_state(|| true);

    let on_logout = {
        let app_state = app_state.clone();
        Callback::from(move |_| handle_logout(app_state.clone()))
    };

    {
        let server_ip = app_state.server_ip.clone().unwrap_or_default();
        let chat_enabled = chat_enabled.clone();
        use_effect_with(server_ip.clone(), move |_| {
            if server_ip.is_empty() {
                chat_enabled.set(true);
                return ();
            }
            let chat_enabled = chat_enabled.clone();
            spawn_local(async move {
                if let Ok(features) = fetch_features(&server_ip).await {
                    chat_enabled.set(features.chat_enabled);
                }
            });
            ()
        });
    }

    let set_tab = {
        let active_tab = active_tab.clone();
        let unread_chat = unread_chat.clone();
        Callback::from(move |tab: Tab| {
            active_tab.set(tab);
            if tab == Tab::Chat {
                unread_chat.set(0);
            }
        })
    };

    let tab_button = |label: Html, tab: Tab| {
        let active_tab = active_tab.clone();
        let set_tab = set_tab.clone();
        let is_active = *active_tab == tab;
        let class = if is_active {
            "rounded-xl bg-ink/70 px-4 py-3 text-left text-secondary text-base font-semibold shadow-lg hover:brightness-100"
        } else {
            "rounded-xl bg-transparent px-4 py-3 text-left text-secondary/80 text-base transition hover:bg-ink/60 hover:text-secondary hover:shadow-md hover:brightness-100"
        };
        html! {
            <Button
                class={Some(class.to_string())}
                onclick={Callback::from(move |_| set_tab.emit(tab))}
            >
                { label }
            </Button>
        }
    };

    let chat_badge = if *unread_chat > 0 {
        html! { <span class="rounded-full bg-primary px-2 py-0.5 text-xs text-white">{ *unread_chat }</span> }
    } else {
        html! {}
    };
    let chat_label = html! {
        <span class="flex items-center gap-2">
            <span>{ "Chat" }</span>
            { chat_badge }
        </span>
    };

    let on_unread = {
        let unread_chat = unread_chat.clone();
        Callback::from(move |count: usize| {
            let current = *unread_chat;
            unread_chat.set(current + count);
        })
    };

    html! {
        <main class="min-h-screen bg-ink text-secondary">
            <div class="flex min-h-screen">
                <aside class="w-64 border-r border-ink/40 bg-inkLight/90 p-6 shadow-2xl">
                    <h2 class="text-6xl font-semibold">{ "Gaggle" }</h2>
                    <nav class="mt-8 flex flex-col gap-2 text-sm">
                        { tab_button(html! { "Library" }, Tab::Library) }
                        if *chat_enabled {
                            { tab_button(chat_label, Tab::Chat) }
                        }
                        { tab_button(html! { "Settings" }, Tab::Settings) }
                        if app_state.user.as_ref().map(|u| u.is_admin).unwrap_or(false) {
                            { tab_button(html! { "Admin" }, Tab::Admin) }
                        }
                    </nav>
                </aside>
                <section class="flex-1 bg-ink/70 p-10 min-h-screen overflow-hidden">
                    <div class="h-full min-h-0 rounded-3xl border border-ink/40 bg-ink/40 p-8 shadow-2xl backdrop-blur flex flex-col">
                        <div class={if *active_tab == Tab::Library { "" } else { "hidden" }}>
                            <LibraryScreen />
                        </div>
                        <div class={if *active_tab == Tab::Chat { "" } else { "hidden" }}>
                            <ChatScreen active={*active_tab == Tab::Chat} on_unread={on_unread} />
                        </div>
                        <div class={if *active_tab == Tab::Settings { "" } else { "hidden" }}>
                            <SettingsScreen on_logout={on_logout.clone()} user={app_state.user.clone()} />
                        </div>
                        <div class={if *active_tab == Tab::Admin { "" } else { "hidden" }}>
                            <AdminScreen />
                        </div>
                    </div>
                </section>
            </div>
        </main>
    }
}
