mod app;
mod api;
mod auth;
mod components;
mod confirm;
mod screens;
mod toast;

use app::App;

fn main() {
    console_error_panic_hook::set_once();
    yew::Renderer::<App>::new().render();
}
