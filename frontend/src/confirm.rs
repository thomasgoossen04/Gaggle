use yew::prelude::*;

#[derive(Clone, PartialEq)]
pub struct ConfirmRequest {
    pub title: String,
    pub message: String,
    pub confirm_label: String,
    pub cancel_label: String,
    pub on_confirm: Callback<()>,
}

#[derive(Clone, PartialEq)]
pub struct ConfirmContext {
    pub open: Callback<ConfirmRequest>,
}

impl ConfirmContext {
    pub fn confirm(&self, req: ConfirmRequest) {
        self.open.emit(req);
    }
}

#[derive(Properties, PartialEq)]
pub struct ConfirmProviderProps {
    #[prop_or_default]
    pub children: Children,
}

#[function_component(ConfirmProvider)]
pub fn confirm_provider(props: &ConfirmProviderProps) -> Html {
    let active = use_state(|| None::<ConfirmRequest>);
    let open = {
        let active = active.clone();
        Callback::from(move |req: ConfirmRequest| active.set(Some(req)))
    };

    let on_cancel = {
        let active = active.clone();
        Callback::from(move |_| active.set(None))
    };

    let on_confirm = {
        let active = active.clone();
        Callback::from(move |_| {
            if let Some(req) = (*active).clone() {
                req.on_confirm.emit(());
            }
            active.set(None);
        })
    };

    html! {
        <ContextProvider<ConfirmContext> context={ConfirmContext { open }}>
            { for props.children.iter() }
            if let Some(req) = (*active).clone() {
                <div class="fixed inset-0 z-50 flex items-center justify-center">
                    <div class="absolute inset-0 bg-ink/70 backdrop-blur-sm" onclick={on_cancel.clone()} />
                    <div class="relative w-[min(92vw,28rem)] rounded-2xl border border-ink/50 bg-inkLight p-6 shadow-2xl">
                        <h2 class="text-lg font-semibold text-secondary">{ req.title }</h2>
                        <p class="mt-2 text-sm text-accent">{ req.message }</p>
                        <div class="mt-6 flex items-center justify-end gap-3">
                            <button
                                class="rounded-xl border border-ink/50 bg-ink/40 px-4 py-2 text-sm font-semibold text-secondary transition hover:bg-ink/50"
                                type="button"
                                onclick={on_cancel}
                            >
                                { req.cancel_label }
                            </button>
                            <button
                                class="rounded-xl border border-rose-400/60 bg-rose-500/30 px-4 py-2 text-sm font-semibold text-rose-100 transition hover:bg-rose-500/40"
                                type="button"
                                onclick={on_confirm}
                            >
                                { req.confirm_label }
                            </button>
                        </div>
                    </div>
                </div>
            }
        </ContextProvider<ConfirmContext>>
    }
}

#[hook]
pub fn use_confirm() -> ConfirmContext {
    use_context::<ConfirmContext>()
        .expect("ConfirmContext not found. Ensure you are within <ConfirmProvider>.")
}
