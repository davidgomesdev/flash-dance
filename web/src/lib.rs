//! A website for the `effects` binary: a page of cards, one per effect, that
//! starts and stops it on the machine the server is running on.
//!
//! The server renders and hydrates with Leptos, and owns exactly one child
//! process, because the Launchpad has exactly one MIDI port. See [`runner`] for
//! why stopping goes through a closed pipe rather than a signal.

pub mod app;

#[cfg(feature = "ssr")]
pub mod runner;

#[cfg(feature = "hydrate")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn hydrate() {
    console_error_panic_hook::set_once();
    leptos::mount::hydrate_body(app::App);
}
