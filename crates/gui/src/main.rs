//! gpui + gpui-component frontend. Milestone 8 builds out the real views; this is
//! just a smoke test that renders a blank window.

use gpui::{
    App, AppContext, Application, Context, IntoElement, ParentElement, Render, Window, WindowOptions,
    div,
};

struct RootView;

impl Render for RootView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().child("folder-share")
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        cx.open_window(WindowOptions::default(), |_, cx| cx.new(|_| RootView))
            .unwrap();
    });
}
