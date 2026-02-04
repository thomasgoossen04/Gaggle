use gloo_timers::future::TimeoutFuture;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;
use yew::hook;

#[derive(Clone, PartialEq)]
pub enum ToastVariant {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Clone, PartialEq)]
pub struct ToastRequest {
    pub message: String,
    pub variant: ToastVariant,
    pub timeout_ms: Option<u32>,
}

#[derive(Clone, PartialEq)]
pub struct ToastContext {
    pub push: Callback<ToastRequest>,
}

impl ToastContext {
    pub fn toast(&self, message: impl Into<String>, variant: ToastVariant, timeout_ms: Option<u32>) {
        self.push.emit(ToastRequest {
            message: message.into(),
            variant,
            timeout_ms,
        });
    }
}

#[derive(Clone, PartialEq)]
struct Toast {
    id: u32,
    message: String,
    variant: ToastVariant,
    timeout_ms: Option<u32>,
}

#[derive(Properties, PartialEq)]
pub struct ToastProviderProps {
    #[prop_or_default]
    pub children: Children,
}

#[function_component(ToastProvider)]
pub fn toast_provider(props: &ToastProviderProps) -> Html {
    let toasts = use_state(Vec::<Toast>::new);
    let counter = use_mut_ref(|| 0u32);

    let push = {
        let toasts = toasts.clone();
        let counter = counter.clone();
        Callback::from(move |req: ToastRequest| {
            let id = {
                let mut count = counter.borrow_mut();
                *count += 1;
                *count
            };
            let toast = Toast {
                id,
                message: req.message,
                variant: req.variant,
                timeout_ms: req.timeout_ms,
            };
            let mut next = (*toasts).clone();
            next.push(toast.clone());
            toasts.set(next);

            if let Some(ms) = toast.timeout_ms {
                let toasts = toasts.clone();
                spawn_local(async move {
                    TimeoutFuture::new(ms).await;
                    remove_toast(id, toasts);
                });
            }
        })
    };

    let on_dismiss = {
        let toasts = toasts.clone();
        Callback::from(move |id: u32| remove_toast(id, toasts.clone()))
    };

    html! {
        <ContextProvider<ToastContext> context={ToastContext { push }}>
            { for props.children.iter() }
            <ToastViewport toasts={(*toasts).clone()} on_dismiss={on_dismiss} />
        </ContextProvider<ToastContext>>
    }
}

#[derive(Properties, PartialEq)]
struct ToastViewportProps {
    pub toasts: Vec<Toast>,
    pub on_dismiss: Callback<u32>,
}

#[function_component(ToastViewport)]
fn toast_viewport(props: &ToastViewportProps) -> Html {
    html! {
        <div class="pointer-events-none fixed bottom-6 left-1/2 z-50 flex w-[min(92vw,24rem)] -translate-x-1/2 flex-col gap-3">
            { for props.toasts.iter().map(|toast| {
                let on_dismiss = {
                    let on_dismiss = props.on_dismiss.clone();
                    let id = toast.id;
                    Callback::from(move |_| on_dismiss.emit(id))
                };
                html! {
                    <div class={toast_classes(&toast.variant)}>
                        <div class="flex items-start justify-between gap-4">
                            <div class="flex items-start gap-3">
                                <span class={toast_dot_class(&toast.variant)} />
                                <div>
                                    <p class="text-sm font-semibold">
                                        { toast_title(&toast.variant) }
                                    </p>
                                    <p class="mt-1 text-sm text-secondary/80">
                                        { toast.message.clone() }
                                    </p>
                                </div>
                            </div>
                            <button
                                class="text-secondary/60 transition hover:text-secondary"
                                type="button"
                                onclick={on_dismiss}
                            >
                                { "×" }
                            </button>
                        </div>
                    </div>
                }
            }) }
        </div>
    }
}

fn toast_classes(variant: &ToastVariant) -> Classes {
    classes!(
        "toast",
        "pointer-events-auto",
        "rounded-xl",
        "border",
        "bg-inkLight",
        "p-4",
        "shadow-xl",
        "text-secondary",
        variant_border_class(variant)
    )
}

fn toast_dot_class(variant: &ToastVariant) -> Classes {
    classes!(
        "mt-1",
        "h-2.5",
        "w-2.5",
        "shrink-0",
        "rounded-full",
        variant_dot_class(variant)
    )
}

fn toast_title(variant: &ToastVariant) -> &'static str {
    match variant {
        ToastVariant::Info => "Info",
        ToastVariant::Success => "Success",
        ToastVariant::Warning => "Warning",
        ToastVariant::Error => "Error",
    }
}

fn variant_border_class(variant: &ToastVariant) -> &'static str {
    match variant {
        ToastVariant::Info => "border-accent/60",
        ToastVariant::Success => "border-secondary/70",
        ToastVariant::Warning => "border-primary/70",
        ToastVariant::Error => "border-rose-400/70",
    }
}

fn variant_dot_class(variant: &ToastVariant) -> &'static str {
    match variant {
        ToastVariant::Info => "bg-accent",
        ToastVariant::Success => "bg-secondary",
        ToastVariant::Warning => "bg-primary",
        ToastVariant::Error => "bg-rose-400",
    }
}

fn remove_toast(id: u32, toasts: UseStateHandle<Vec<Toast>>) {
    let mut next = (*toasts).clone();
    next.retain(|toast| toast.id != id);
    toasts.set(next);
}

#[hook]
pub fn use_toast() -> ToastContext {
    use_context::<ToastContext>()
        .expect("ToastContext not found. Ensure you are within <ToastProvider>.")
}
