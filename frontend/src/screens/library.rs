use yew::prelude::*;

#[function_component(LibraryScreen)]
pub fn library_screen() -> Html {
    html! {
        <div>
            <h1 class="text-2xl font-semibold">{ "Library" }</h1>
            <p class="mt-2 text-sm text-accent">
                { "Manage launchable apps, tools, and presets." }
            </p>
            <div class="mt-8 grid gap-6 md:grid-cols-2 xl:grid-cols-3">
                <div class="rounded-2xl border border-ink/50 bg-inkLight p-6">
                    <p class="text-xs uppercase tracking-wide text-accent/80">{ "Recently Used" }</p>
                    <p class="mt-4 text-lg font-semibold">{ "Starter Stack" }</p>
                    <p class="mt-2 text-sm text-secondary/70">{ "Launch last used apps quickly." }</p>
                </div>
                <div class="rounded-2xl border border-ink/50 bg-inkLight p-6">
                    <p class="text-xs uppercase tracking-wide text-accent/80">{ "Collections" }</p>
                    <p class="mt-4 text-lg font-semibold">{ "Game Night" }</p>
                    <p class="mt-2 text-sm text-secondary/70">{ "Group launches for events." }</p>
                </div>
                <div class="rounded-2xl border border-ink/50 bg-inkLight p-6">
                    <p class="text-xs uppercase tracking-wide text-accent/80">{ "Pinned" }</p>
                    <p class="mt-4 text-lg font-semibold">{ "Creator Suite" }</p>
                    <p class="mt-2 text-sm text-secondary/70">{ "Your always-on tools." }</p>
                </div>
            </div>
        </div>
    }
}
