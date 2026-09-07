//! Demo: tap a grid pad to toggle it on, hold a side button to light it.
//! The top-right button quits (so the device gets reset on the way out).

use std::collections::HashSet;

use flashdance::{Event, Launchpad, Light, Pad, Result};

const QUIT: Pad = Pad(98); // top row, rightmost

fn main() -> Result<()> {
    let (mut pad, events) = Launchpad::connect()?;
    pad.set(QUIT, Light::Solid(5))?; // red: press to quit
    println!("Tap the pads. Top-right button quits.");

    let mut lit = HashSet::new();
    for event in events {
        match event {
            Event::Press(QUIT, _) => break,
            Event::Press(p, velocity) => match p.to_xy() {
                Some((x, _)) if lit.insert(p) => pad.set(p, Light::Rgb(velocity, x * 16, 40))?,
                Some(_) => {
                    lit.remove(&p);
                    pad.set(p, Light::Off)?;
                }
                None => pad.set(p, Light::Pulse(3))?, // side buttons: white while held
            },
            Event::Release(p) if p.to_xy().is_none() => pad.set(p, Light::Off)?,
            Event::Release(_) => {}
        }
    }

    pad.set(QUIT, Light::Off)?;
    Ok(())
}
