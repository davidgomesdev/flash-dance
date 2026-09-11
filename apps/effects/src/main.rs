//! The effects from a terminal. Everything lives in `flash-dance-effects`; this is
//! argument parsing around [`run`].
//!
//! `cargo run -p effects -- fire` starts on the first effect whose name matches.
//! `FLASH_DANCE_SOURCE` picks what the sound effects listen to, by any part of a
//! PulseAudio source name; the default is whatever the machine is playing.

use std::sync::atomic::AtomicBool;

use flash_dance_effects::{EFFECTS, Effect, run};

/// The effect named on the command line, or the first. Decided before [`run`]
/// opens the device, because a name that matches nothing exits the process — and
/// an exit skips `Drop`, which would leave the Launchpad lit and in programmer mode.
fn starting_effect() -> Effect {
    let Some(query) = std::env::args().nth(1) else {
        return EFFECTS[0];
    };
    match EFFECTS.into_iter().find(|e| e.name().contains(&query)) {
        Some(effect) => effect,
        None => {
            eprintln!("no effect matches {query:?}. try one of:");
            for effect in EFFECTS {
                eprintln!("  {}", effect.name());
            }
            std::process::exit(2);
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let start = starting_effect();
    let source = std::env::var("FLASH_DANCE_SOURCE").ok();
    println!("(User picks an effect, up/down brightness, left/right speed, Stop/Solo/Mute quits)");
    // Nothing here ever sets it: from a terminal, the device's quit button ends it.
    run(start, source.as_deref(), &AtomicBool::new(false))
}
