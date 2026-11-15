//! The analysis as text, for checking the sound half without a Launchpad:
//!
//! ```text
//! cargo run -p flash-dance-audio --example meter            # system sound
//! cargo run -p flash-dance-audio --example meter -- webcam  # a named source
//! ```

use flash_dance_audio::{Audio, Result};

fn main() -> Result<()> {
    // The library logs "listening to …" itself.
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();
    let want = std::env::args().nth(1);
    let mut audio = Audio::open(want.as_deref())?;

    loop {
        std::thread::sleep(std::time::Duration::from_millis(33));
        let sound = audio.listen();
        let bars: String = sound
            .bands
            .iter()
            .map(|b| {
                "_.-=#"
                    .chars()
                    .nth((b * 4.0).round() as usize)
                    .unwrap_or('#')
            })
            .collect();
        tracing::info!(
            "pitch {:.2}  bands {bars}  {}",
            sound.pitch,
            if sound.beat { "beat" } else { "" }
        );
    }
}
