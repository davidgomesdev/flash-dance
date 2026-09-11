//! The site: pick an effect, see what is running, stop it.
//!
//! Everything the page knows comes from `flash-dance-effects` — the list, the
//! grouping, even the colour of each card, which is the hue that effect's pad has
//! in the device's own picker. Speed and brightness are deliberately absent: they
//! live on the Launchpad's arrow buttons, where your hands already are.

use leptos::either::Either;
use leptos::prelude::*;
use leptos_meta::{MetaTags, Stylesheet, Title, provide_meta_context};
use leptos_router::components::{Route, Router, Routes};
use leptos_router::path;
use serde::{Deserialize, Serialize};

/// One effect, as the effects library describes it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EffectInfo {
    pub group: String,
    pub name: String,
    /// What it looks like and what it responds to.
    pub about: String,
    /// `0.0..1.0`, the effect's signature hue on the device.
    pub hue: f32,
}

/// A capture source the sound effects can listen to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceInfo {
    pub name: String,
    /// An output's monitor: what the machine is playing, rather than a microphone.
    pub monitor: bool,
}

impl SourceInfo {
    /// Source names are long and mostly card identifiers; say what it is instead.
    pub fn label(&self) -> String {
        if self.monitor {
            format!("system sound — {}", self.name.trim_end_matches(".monitor"))
        } else {
            self.name.clone()
        }
    }
}

/// What the server's effect session is doing.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum Status {
    #[default]
    Idle,
    Running(String),
    /// The session ended with an error — no device plugged in, most often — and
    /// this is what it was.
    Failed {
        effect: String,
        error: String,
    },
}

#[server]
pub async fn list_effects() -> Result<Vec<EffectInfo>, ServerFnError> {
    Ok(crate::runner::effects())
}

#[server]
pub async fn list_sources() -> Result<Vec<SourceInfo>, ServerFnError> {
    Ok(crate::runner::sources())
}

#[server]
pub async fn start_effect(effect: String, source: String) -> Result<Status, ServerFnError> {
    crate::runner::start(&effect, Some(source.as_str())).map_err(ServerFnError::new)
}

#[server]
pub async fn stop_effect() -> Result<Status, ServerFnError> {
    Ok(crate::runner::stop())
}

#[server]
pub async fn current_status() -> Result<Status, ServerFnError> {
    Ok(crate::runner::status())
}

pub fn shell(options: LeptosOptions) -> impl IntoView {
    view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8" />
                <meta name="viewport" content="width=device-width, initial-scale=1" />
                <AutoReload options=options.clone() />
                <HydrationScripts options />
                <MetaTags />
            </head>
            <body>
                <App />
            </body>
        </html>
    }
}

#[component]
pub fn App() -> impl IntoView {
    provide_meta_context();

    view! {
        <Stylesheet id="leptos" href="/pkg/flash-dance-web.css" />
        <Title text="flash-dance" />
        <Router>
            <Routes fallback=|| view! { <p class="empty">"Nothing here."</p> }>
                <Route path=path!("") view=Console />
            </Routes>
        </Router>
    }
}

#[component]
fn Console() -> impl IntoView {
    let effects = OnceResource::new(list_effects());
    let sources = OnceResource::new(list_sources());
    let (source, set_source) = signal(String::new());

    let status = Resource::new(
        || (),
        |_| async { current_status().await.unwrap_or_default() },
    );
    let start = Action::new(move |effect: &String| {
        let (effect, source) = (effect.clone(), source.get_untracked());
        async move { start_effect(effect, source).await }
    });
    let stop = Action::new(|(): &()| async { stop_effect().await });

    // The effect can also stop itself — Stop/Solo/Mute on the device, or a crash
    // because nothing is plugged in — so the page asks rather than assumes.
    Effect::new(move |_| {
        let (started, stopped) = (start.version().get(), stop.version().get());
        let _ = (started, stopped);
        status.refetch();
    });
    Effect::new(move |_| {
        set_interval(
            move || status.refetch(),
            std::time::Duration::from_millis(1500),
        );
    });

    let running = move || match status.get() {
        Some(Status::Running(effect)) => Some(effect),
        _ => None,
    };

    view! {
        <main>
            <header>
                <h1>"flash" <em>"dance"</em></h1>
                <p class="tagline">"Launchpad Mini MK3 — pick an effect."</p>
            </header>

            <StatusBar status=status stop=stop />

            <section class="settings">
                <label for="source">"Sound effects listen to"</label>
                <Suspense fallback=|| {
                    view! { <select id="source" disabled><option>"…"</option></select> }
                }>
                    {move || Suspend::new(async move {
                        let listed = sources.await.unwrap_or_default();
                        let empty = listed.is_empty();
                        view! {
                            <select
                                id="source"
                                disabled=empty
                                on:change:target=move |ev| set_source.set(ev.target().value())
                            >
                                {listed
                                    .into_iter()
                                    .map(|source| {
                                        view! {
                                            <option value=source.name.clone()>{source.label()}</option>
                                        }
                                    })
                                    .collect_view()}
                                <Show when=move || empty>
                                    <option>"no sound server"</option>
                                </Show>
                            </select>
                        }
                    })}
                </Suspense>
                <p class="hint">
                    "Speed and brightness stay on the device: the arrows change them, "
                    <strong>"Stop/Solo/Mute"</strong>
                    " quits."
                </p>
            </section>

            // A Transition, not a Suspense: every card reads the status the page
            // polls, and a Suspense falls back to this placeholder on *every* reload
            // of what it reads — which tore down and rebuilt the whole grid once a
            // poll, and looked like the page refreshing itself.
            <Transition fallback=|| view! { <p class="empty">"Loading effects…"</p> }>
                {move || Suspend::new(async move {
                    match effects.await {
                        Err(e) => {
                            Either::Left(
                                view! { <p class="empty">"Cannot list the effects: " {e.to_string()}</p> },
                            )
                        }
                        Ok(listed) => {
                            Either::Right(
                                GROUPS
                                    .iter()
                                    .map(|(group, blurb)| {
                                        let mine: Vec<EffectInfo> = listed
                                            .iter()
                                            .filter(|effect| effect.group == *group)
                                            .cloned()
                                            .collect();
                                        view! {
                                            <section class="group">
                                                <h2>{*group}</h2>
                                                <p class="blurb">{*blurb}</p>
                                                <ul class="cards">
                                                    {mine
                                                        .into_iter()
                                                        .map(|effect| {
                                                            view! { <Card effect=effect running=running start=start /> }
                                                        })
                                                        .collect_view()}
                                                </ul>
                                            </section>
                                        }
                                    })
                                    .collect_view(),
                            )
                        }
                    }
                })}
            </Transition>
        </main>
    }
}

/// The three kinds the library reports, in the order they are worth reading.
const GROUPS: [(&str, &str); 3] = [
    ("ambient", "A function of time. Nothing to do but watch."),
    ("reactive", "Driven by the pads: press them."),
    ("sound", "Driven by what the machine is playing."),
];

#[component]
fn Card(
    effect: EffectInfo,
    running: impl Fn() -> Option<String> + Copy + Send + Sync + 'static,
    start: Action<String, Result<Status, ServerFnError>>,
) -> impl IntoView {
    let name = effect.name.clone();
    let (mine, colour) = (name.clone(), hue(effect.hue));
    let is_running = move || running().as_deref() == Some(mine.as_str());
    let label = name.clone();
    let about_id = format!("about-{}", name.replace(' ', "-"));

    // The `?` is a sibling of the card, not inside it — a button in a button is
    // invalid, and a click on it must not start the effect. It opens a native
    // popover, which needs no signals and works before the page has hydrated.
    view! {
        <li style=format!("--hue: {colour}")>
            <button
                class="card"
                class:running=is_running
                on:click=move |_| {
                    start.dispatch(name.clone());
                }
            >
                <span class="swatch"></span>
                <span class="name">{label.clone()}</span>
            </button>
            <button
                class="help"
                popovertarget=about_id.clone()
                aria-label=format!("What is {label}?")
            >
                "?"
            </button>
            <div class="about" id=about_id popover="auto">
                <h3>
                    <span class="swatch"></span>
                    {label}
                </h3>
                <p>{effect.about}</p>
            </div>
        </li>
    }
}

/// The effect's hue in degrees, matching the pad colour in the device's picker.
fn hue(hue: f32) -> i32 {
    (hue * 360.0).round() as i32
}

#[component]
fn StatusBar(
    status: Resource<Status>,
    stop: Action<(), Result<Status, ServerFnError>>,
) -> impl IntoView {
    view! {
        <div class="status">
            <Transition fallback=|| view! { <span class="dot idle"></span> }>
                {move || {
                    let status = status.get().unwrap_or_default();
                    let running = matches!(status, Status::Running(_));
                    let (class, text) = match &status {
                        Status::Idle => ("idle", "Nothing running.".to_string()),
                        Status::Running(effect) => ("live", format!("Running {effect}.")),
                        Status::Failed { effect, error } => {
                            ("failed", format!("{effect} stopped: {error}"))
                        }
                    };
                    view! {
                        <span class=format!("dot {class}")></span>
                        <span class="what">{text}</span>
                        <Show when=move || running>
                            <button
                                class="stop"
                                on:click=move |_| {
                                    stop.dispatch(());
                                }
                            >
                                "Stop"
                            </button>
                        </Show>
                    }
                }}
            </Transition>
        </div>
    }
}
