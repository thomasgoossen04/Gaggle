use wasm_bindgen::prelude::Closure;
use wasm_bindgen::JsCast;
use yew::{
    classes, function_component, html, use_effect, use_effect_with, use_mut_ref, use_state,
    Callback, Children, Html, MouseEvent, Properties,
};

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

#[allow(unused)]
#[derive(Properties, PartialEq)]
pub struct ProgressBarProps {
    pub value: f64,
}

#[function_component(ProgressBar)]
pub fn progress_bar(props: &ProgressBarProps) -> Html {
    let clamped = props.value.max(0.0).min(100.0);
    let width = format!("{:.1}%", clamped);
    html! {
        <div class="h-3 w-full rounded-full border border-ink/50 bg-ink/40">
            <div
                class="h-full rounded-full bg-primary/70 transition-[width] duration-200"
                style={format!("width: {width};")}
            />
        </div>
    }
}

#[function_component(IndeterminateBar)]
pub fn indeterminate_bar() -> Html {
    html! {
        <div class="h-3 w-full overflow-hidden rounded-full border border-ink/50 bg-ink/40">
            <div class="h-full w-full animate-[stripe_1.2s_linear_infinite] bg-[length:40px_40px] bg-[linear-gradient(135deg,rgba(255,255,255,0.18)_25%,rgba(255,255,255,0)_25%,rgba(255,255,255,0)_50%,rgba(255,255,255,0.18)_50%,rgba(255,255,255,0.18)_75%,rgba(255,255,255,0)_75%,rgba(255,255,255,0))]" />
        </div>
    }
}

#[derive(Properties, PartialEq)]
pub struct DownloadSpeedGraphProps {
    /// (timestamp_secs, speed bps)
    pub points: Vec<(f64, f64)>,
    #[prop_or(72)]
    pub height: u32,
    pub paused: bool,
}

#[function_component(DownloadSpeedGraph)]
pub fn download_speed_graph(props: &DownloadSpeedGraphProps) -> Html {
    let history = use_mut_ref(Vec::<(f64, f64)>::new);
    let interp_point = use_state(|| None::<(f64, f64)>);
    let last_real = use_mut_ref(|| None::<(f64, f64)>);

    if props.paused {
        // clear the history, we are paused
        history.borrow_mut().clear();
    }

    {
        let history = history.clone();
        let interp_point = interp_point.clone();
        let last_real = last_real.clone();
        let points = props.points.clone();

        use_effect_with(points, move |points| {
            let mut h = history.borrow_mut();

            if let Some(&(t, v)) = points.last() {
                let cutoff = t - 10.0;

                if h.last() != Some(&(t, v)) {
                    h.push((t, v));
                }

                h.retain(|(ts, _)| *ts >= cutoff);

                *last_real.borrow_mut() = Some((t, v));
                interp_point.set(Some((t, v)));
            }
        });
    }

    {
        let interp_point = interp_point.clone();
        let last_real = last_real.clone();

        use_effect(move || {
            use std::cell::RefCell;
            use std::rc::Rc;

            let window = web_sys::window().unwrap();

            let raf: Rc<RefCell<Option<Closure<dyn FnMut()>>>> = Rc::new(RefCell::new(None));
            let raf_in_closure = raf.clone();
            let window_in_closure = window.clone();

            *raf.borrow_mut() = Some(Closure::wrap(Box::new(move || {
                if let (Some((target_t, target_v)), Some((prev_t, prev_v))) =
                    (*last_real.borrow(), *interp_point)
                {
                    let dv = (target_v - prev_v).abs();
                    if dv < 0.01 {
                        interp_point.set(Some((target_t, target_v)));
                        return;
                    }

                    let now = js_sys::Date::now() / 1000.0;
                    let dt = (now - prev_t).clamp(0.0, 0.15);
                    let alpha = (dt / 0.15).clamp(0.0, 1.0);

                    let v = prev_v + alpha * (target_v - prev_v);
                    interp_point.set(Some((now, v)));
                }

                if let Some(cb) = raf_in_closure.borrow().as_ref() {
                    let _ = window_in_closure.request_animation_frame(cb.as_ref().unchecked_ref());
                }
            }) as Box<dyn FnMut()>));

            if let Some(cb) = raf.borrow().as_ref() {
                let _ = window.request_animation_frame(cb.as_ref().unchecked_ref());
            }

            move || {
                raf.borrow_mut().take();
            }
        });
    }

    let mut points = history.borrow().clone();
    if let Some((t, v)) = *interp_point {
        if let Some(&(_last_t, last_v)) = points.last() {
            if (v - last_v).abs() > 0.01 {
                points.push((t, v));
            }
        }
    }

    //console::log_1(&format!("graph history: {:?}", *points).into());

    if points.len() < 2 {
        return html! {
            <div class="h-18 rounded-lg border border-ink/40 bg-ink/20" />
        };
    }

    let height = props.height as f64;
    let raw_max = points.iter().map(|(_, s)| *s).fold(0.0, f64::max);
    let max_speed = nice_max(raw_max * 1.1); // 10% headroom

    let width = 100.0;
    let window = 10.0;

    let first_t = points.first().unwrap().0;
    let now = last_real
        .borrow()
        .map(|(t, _)| t)
        .unwrap_or_else(|| points.last().unwrap().0);

    // Decide the right edge
    let right_t = if now - first_t < window {
        first_t + window
    } else {
        now
    };

    let left_t = right_t - window;

    let mapped: Vec<(f64, f64)> = points
        .iter()
        .map(|(t, speed)| {
            let x = width * ((t - left_t) / window);
            let y = height - (speed / max_speed) * height;
            (x.clamp(0.0, width), y)
        })
        .collect();
    let path = catmull_rom_to_bezier(&mapped);

    let mut fill = String::new();
    fill.push_str(&format!("M {:.2} {:.2}", mapped.first().unwrap().0, height));
    fill.push_str(&format!(
        " L {:.2} {:.2}",
        mapped.first().unwrap().0,
        mapped.first().unwrap().1
    ));
    fill.push_str(&path[1..]);
    fill.push_str(&format!(" L {:.2} {:.2}", mapped.last().unwrap().0, height));
    fill.push_str(" Z");

    html! {
        <div class="w-full overflow-hidden rounded-xl border border-ink/40 bg-ink/20">
            <svg
                viewBox={format!("0 0 {} {}", width, height)}
                preserveAspectRatio="none"
                class="block h-[72px] w-full"
            >
                <line
                    x1="0"
                    y1={height.to_string()}
                    x2="100"
                    y2={height.to_string()}
                    stroke="currentColor"
                    stroke-width="1"
                    class="text-secondary/20"
                />

                // filled area
                <path
                    d={fill}
                    fill="currentColor"
                    fill-opacity="0.2"
                    stroke="none"
                    class="text-primary"
                />

                // line path
                <path
                    d={path}
                    fill="none"
                    stroke="currentColor"
                    stroke-width="1.5"
                    vector-effect="non-scaling-stroke"
                    stroke-linecap="round"
                    stroke-linejoin="round"
                    class="text-primary"
                />
            </svg>
        </div>
    }
}

fn catmull_rom_to_bezier(points: &[(f64, f64)]) -> String {
    if points.len() < 2 {
        return String::new();
    }

    let mut d = String::new();
    d.push_str(&format!("M {:.2} {:.2}", points[0].0, points[0].1));

    for i in 0..points.len() - 1 {
        let p0 = if i == 0 { points[i] } else { points[i - 1] };
        let p1 = points[i];
        let p2 = points[i + 1];
        let p3 = if i + 2 < points.len() {
            points[i + 2]
        } else {
            p2
        };

        let c1x = p1.0 + (p2.0 - p0.0) / 6.0;
        let c1y = p1.1 + (p2.1 - p0.1) / 6.0;
        let c2x = p2.0 - (p3.0 - p1.0) / 6.0;
        let c2y = p2.1 - (p3.1 - p1.1) / 6.0;

        d.push_str(&format!(
            " C {:.2} {:.2}, {:.2} {:.2}, {:.2} {:.2}",
            c1x, c1y, c2x, c2y, p2.0, p2.1
        ));
    }

    d
}

fn nice_max(value: f64) -> f64 {
    if value <= 0.0 {
        return 1.0;
    }

    let exp = value.log10().floor();
    let base = 10f64.powf(exp);

    let fraction = value / base;
    let nice_fraction = if fraction <= 1.0 {
        1.0
    } else if fraction <= 2.0 {
        2.0
    } else if fraction <= 5.0 {
        5.0
    } else {
        10.0
    };

    nice_fraction * base
}
