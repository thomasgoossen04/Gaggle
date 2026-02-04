use yew::{classes, function_component, html, Children, Html, Properties};

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
