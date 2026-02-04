use yew::prelude::*;

use crate::components::Button;

#[derive(Properties, PartialEq)]
pub struct SettingsScreenProps {
    pub on_logout: Callback<MouseEvent>,
}

#[function_component(SettingsScreen)]
pub fn settings_screen(props: &SettingsScreenProps) -> Html {
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
                    </div>
                    <Button onclick={props.on_logout.clone()}>
                        { "Log out" }
                    </Button>
                </div>
            </div>
        </div>
    }
}
