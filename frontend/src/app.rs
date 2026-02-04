use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;
use yew::prelude::*;

use crate::components::Card;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "core"])]
    async fn invoke(cmd: &str, args: JsValue) -> JsValue;
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct AppState {
    pub logged_in: bool,
}

type AppStateHandle = UseStateHandle<AppState>;

#[function_component(App)]
pub fn app() -> Html {
    let app_state = use_state(|| AppState { logged_in: false });

    html! {
        <ContextProvider<AppStateHandle> context={app_state}>
            <AppRouter />
        </ContextProvider<AppStateHandle>>
    }
}

#[function_component(AppRouter)]
fn app_router() -> Html {
    let app_state = use_context::<AppStateHandle>()
        .expect("AppState context not found. Ensure AppRouter is under <ContextProvider>.");

    if app_state.logged_in {
        html! { <MainScreen /> }
    } else {
        html! { <LoginScreen /> }
    }
}

#[function_component(LoginScreen)]
fn login_screen() -> Html {
    let app_state = use_context::<AppStateHandle>()
        .expect("AppState context not found. Ensure LoginScreen is under <ContextProvider>.");
    let on_login = {
        let app_state = app_state.clone();
        Callback::from(move |_| app_state.set(AppState { logged_in: true }))
    };

    html! {
        <main class="min-h-screen text-secondary flex items-center justify-center">
            <Card highlight={true}>
                <h1 class="text-3xl font-bold">
                    { "Tailwind v4 + Yew + Tauri" }
                </h1>
                <p class="mt-2 text-sm text-accent">
                    { "Sign in to continue." }
                </p>
                <button
                    class="mt-6 rounded bg-primary px-4 py-2 text-white"
                    onclick={on_login}
                >
                    { "Log in" }
                </button>
            </Card>
        </main>
    }
}

#[function_component(MainScreen)]
fn main_screen() -> Html {
    let app_state = use_context::<AppStateHandle>()
        .expect("AppState context not found. Ensure MainScreen is under <ContextProvider>.");
    let on_logout = {
        let app_state = app_state.clone();
        Callback::from(move |_| app_state.set(AppState { logged_in: false }))
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
                <button
                    class="mt-6 rounded bg-primary px-4 py-2 text-white"
                    onclick={on_logout}
                >
                    { "Log out" }
                </button>
            </Card>
        </main>
    }
}
