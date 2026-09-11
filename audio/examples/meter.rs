//! The analysis as text, for checking the sound half without a Launchpad:
//!
//! ```text
//! cargo run -p flash-dance-audio --example meter            # system sound
//! cargo run -p flash-dance-audio --example meter -- webcam  # a named source
//! ```

use flash_dance_audio::{Audio, Result};

fn main() -> Result<()> {
    let want = std::env::args().nth(1);
    let mut audio = Audio::open(want.as_deref())?;
    eprintln!("listening to {}", audio.source());

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
        println!(
            "pitch {:.2}  bands {bars}  {}",
            sound.pitch,
            if sound.beat { "beat" } else { "" }
        );
    }
}
