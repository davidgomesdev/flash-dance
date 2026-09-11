//! A website for the effects: a page of cards, one per effect, that starts and
//! stops them on the machine the server is running on.
//!
//! The server renders and hydrates with Leptos, and runs effects in-process
//! through `flash-dance-effects` — one session at a time, on a thread of its own,
//! because the Launchpad has exactly one MIDI port. See [`runner`].

pub mod app;

#[cfg(feature = "ssr")]
pub mod runner;

#[cfg(feature = "hydrate")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn hydrate() {
    console_error_panic_hook::set_once();
    leptos::mount::hydrate_body(app::App);
}
