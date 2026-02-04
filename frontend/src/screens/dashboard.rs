use yew::prelude::*;

use crate::app::AppState;
use crate::auth::handle_logout;
use crate::components::Button;
use crate::screens::{chat::ChatScreen, library::LibraryScreen, settings::SettingsScreen};
use crate::toast::{use_toast, ToastVariant};

type AppStateHandle = UseStateHandle<AppState>;

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Library,
    Chat,
    Settings,
}

#[function_component(Dashboard)]
pub fn dashboard() -> Html {
    let app_state = use_context::<AppStateHandle>()
        .expect("AppState context not found. Ensure Dashboard is under <ContextProvider>.");
    let toast = use_toast();
    let active_tab = use_state(|| Tab::Library);

    let on_logout = {
        let app_state = app_state.clone();
        Callback::from(move |_| handle_logout(app_state.clone()))
    };
    let on_test_toast = {
        let toast = toast.clone();
        Callback::from(move |_| {
            toast.toast(
                "Test toast: everything is wired.",
                ToastVariant::Info,
                Some(3000),
            );
        })
    };

    let tab_button = |label: &'static str, tab: Tab| {
        let active_tab = active_tab.clone();
        let is_active = *active_tab == tab;
        let class = if is_active {
            "rounded-xl bg-ink/70 px-4 py-3 text-left text-secondary text-base font-semibold shadow-lg"
        } else {
            "rounded-xl px-4 py-3 text-left text-secondary/80 text-base transition hover:bg-ink/60 hover:text-secondary hover:shadow-md"
        };
        html! {
            <button
                class={class}
                type="button"
                onclick={Callback::from(move |_| active_tab.set(tab))}
            >
                { label }
            </button>
        }
    };

    let content = match *active_tab {
        Tab::Library => html! { <LibraryScreen /> },
        Tab::Chat => html! { <ChatScreen /> },
        Tab::Settings => html! { <SettingsScreen on_logout={on_logout.clone()} user={app_state.user.clone()} /> },
    };

    html! {
        <main class="min-h-screen bg-ink text-secondary">
            <div class="flex min-h-screen">
                <aside class="w-64 border-r border-ink/40 bg-inkLight/90 p-6 shadow-2xl">
                    <h2 class="text-6xl font-semibold">{ "Gaggle" }</h2>
                    <nav class="mt-8 flex flex-col gap-2 text-sm">
                        { tab_button("Library", Tab::Library) }
                        { tab_button("Chat", Tab::Chat) }
                        { tab_button("Settings", Tab::Settings) }
                    </nav>
                    <div class="mt-10">
                        <Button onclick={on_test_toast}>
                            { "Show test toast" }
                        </Button>
                    </div>
                </aside>
                <section class="flex-1 bg-ink/70 p-10 min-h-screen overflow-hidden">
                    <div class="h-full min-h-0 rounded-3xl border border-ink/40 bg-ink/40 p-8 shadow-2xl backdrop-blur flex flex-col">
                        { content }
                    </div>
                </section>
            </div>
        </main>
    }
}
