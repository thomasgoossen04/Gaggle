use wasm_bindgen::prelude::*;
use yew::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "core"])]
    async fn invoke(cmd: &str, args: JsValue) -> JsValue;
}

#[function_component(App)]
pub fn app() -> Html {
    html! {
        <main class="text-primary flex items-center justify-center">
            <div class="">
                <h1 class="text-3xl font-bold">
                    { "Tailwind v4 + Yew + Tauri" }
                </h1>
            </div>
        </main>
    }
}
