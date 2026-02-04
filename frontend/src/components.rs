use yew::{classes, function_component, html, Callback, Children, Html, MouseEvent, Properties};

#[derive(Properties, PartialEq)]
pub struct CardProps {
    #[prop_or(false)]
    pub highlight: bool,
    #[prop_or_default]
    pub children: Children,
}

#[function_component(Card)]
pub fn card(props: &CardProps) -> Html {
    let class = classes!(
        "w-full",
        "max-w-md",
        "rounded-2xl",
        "border",
        "border-ink/50",
        "bg-inkLight",
        "p-8",
        "shadow-xl",
        "backdrop-blur",
        props.highlight.then_some("ring-2 ring-primary/40")
    );
    html! {
        <div class={class}>
            { for props.children.iter() }
        </div>
    }
}

#[derive(Properties, PartialEq)]
pub struct ButtonProps {
    pub onclick: Callback<MouseEvent>,
    #[prop_or_default]
    pub children: Children,
    #[prop_or_default]
    pub class: Option<String>,
    #[prop_or(false)]
    pub disabled: bool,
}

#[function_component(Button)]
pub fn button(props: &ButtonProps) -> Html {
    let class = classes!(
        "inline-flex",
        "items-center",
        "gap-2",
        "rounded",
        "bg-primary",
        "px-4",
        "py-2",
        "text-white",
        "cursor-pointer",
        "select-none",
        "transition",
        "duration-150",
        "ease-out",
        "active:scale-95",
        "hover:brightness-110",
        "focus-visible:outline-none",
        "focus-visible:ring-2",
        "focus-visible:ring-primary/50",
        "disabled:cursor-not-allowed",
        "disabled:opacity-60",
        props.class.clone()
    );
    html! {
        <button
            class={class}
            type="button"
            onclick={props.onclick.clone()}
            disabled={props.disabled}
        >
            { for props.children.iter() }
        </button>
    }
}
