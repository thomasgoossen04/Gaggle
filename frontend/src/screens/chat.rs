use yew::prelude::*;

#[function_component(ChatScreen)]
pub fn chat_screen() -> Html {
    html! {
        <div class="flex h-full flex-col">
            <div>
                <h1 class="text-2xl font-semibold">{ "Chat" }</h1>
                <p class="mt-2 text-sm text-accent">
                    { "Stay connected with your team." }
                </p>
            </div>
            <div class="mt-6 flex-1 rounded-2xl border border-ink/50 bg-inkLight p-6">
                <div class="text-sm text-secondary/70">
                    { "Chat history will appear here." }
                </div>
            </div>
            <div class="mt-4 rounded-2xl border border-ink/50 bg-inkLight p-4">
                <input
                    class="w-full bg-transparent text-sm text-secondary placeholder:text-secondary/50 focus:outline-none"
                    type="text"
                    placeholder="Type a message..."
                />
            </div>
        </div>
    }
}
