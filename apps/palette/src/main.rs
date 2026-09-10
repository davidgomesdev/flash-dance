//! Browse the device's 128 palette colours, since Novation's chart is not in this
//! repo and `Light::Solid` takes an index rather than a colour.
//!
//! Two pages of 64. The right column's top two buttons switch page, tapping a pad
//! prints its index, and the top-left button quits and dumps everything you tapped.

use flash_dance::{Event, Launchpad, Light, Pad, Result};

const QUIT: Pad = Pad(91);
const PAGES: u8 = 2;

fn draw(pad: &mut Launchpad, page: u8) -> Result<()> {
    let swatches = (0..64u8).map(|i| {
        let (x, y) = (i % 8, i / 8);
        (Pad::xy(x, y), Light::Solid(page * 64 + i))
    });
    // White marks the pages, and the current one pulses.
    let tabs = (0..PAGES).map(|p| {
        let light = if p == page {
            Light::Pulse(3)
        } else {
            Light::Solid(1)
        };
        (Pad::right(p), light)
    });
    pad.set_all(swatches.chain(tabs))
}

fn main() -> Result<()> {
    let (mut pad, events) = Launchpad::connect()?;
    let mut page = 0u8;
    let mut tapped: Vec<u8> = Vec::new();
    draw(&mut pad, page)?;
    println!("Page {page}: indices {}-{}", page * 64, page * 64 + 63);
    println!("(right column switches page, tap a pad for its index, top-left quits)");

    for event in events {
        let Event::Press(button, _) = event else {
            continue;
        };
        if button == QUIT {
            break;
        }
        match button.to_xy() {
            Some((x, y)) => {
                let index = page * 64 + y * 8 + x;
                println!("  {index}");
                tapped.push(index);
            }
            None => {
                if let Some(p) = (0..PAGES).find(|p| Pad::right(*p) == button) {
                    page = p;
                    draw(&mut pad, page)?;
                    println!("Page {page}: indices {}-{}", page * 64, page * 64 + 63);
                }
            }
        }
    }

    if !tapped.is_empty() {
        let list: Vec<String> = tapped.iter().map(|i| i.to_string()).collect();
        println!("tapped: {}", list.join(", "));
    }
    Ok(())
}
