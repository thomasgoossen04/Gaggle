use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "core"])]
    async fn invoke(cmd: &str, args: JsValue) -> JsValue;
}

#[function_component(App)]
pub fn app() -> Html {
    html! {
        <div class="bg-zinc-900 text-zinc-100 flex items-center justify-center">
            <h1 class="text-3xl font-bold">
                { "Tailwind v4 + Yew + Tauri" }
            </h1>
        </div>
    }
}
