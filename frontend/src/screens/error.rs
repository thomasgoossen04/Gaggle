use yew::prelude::*;

use crate::components::{Button, Card};

#[derive(Properties, PartialEq)]
pub struct ErrorScreenProps {
    pub message: String,
    pub on_logout: Callback<MouseEvent>,
    pub on_retry: Callback<MouseEvent>,
}

#[function_component(ErrorScreen)]
pub fn error_screen(props: &ErrorScreenProps) -> Html {
    html! {
        <main class="min-h-screen bg-ink text-secondary flex items-center justify-center p-6">
            <Card highlight={true}>
                <h1 class="text-2xl font-semibold">{ "Connection lost" }</h1>
                <p class="mt-2 text-sm text-accent">
                    { "We couldn't reach the backend to validate your session." }
                </p>
                <div class="mt-4 rounded-lg border border-ink/50 bg-ink/60 p-3 text-sm text-secondary/80">
                    { props.message.clone() }
                </div>
                <div class="mt-6 flex items-center gap-3">
                    <Button onclick={props.on_retry.clone()}>
                        { "Try reconnect" }
                    </Button>
                    <Button class={Some("bg-inkLight border border-ink/50".to_string())} onclick={props.on_logout.clone()}>
                        { "Log out" }
                    </Button>
                </div>
            </Card>
        </main>
    }
}
