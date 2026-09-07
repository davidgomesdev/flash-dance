//! Minimal API for the Novation Launchpad Mini MK3: light the pads, read the presses.
//!
//! The device is put into *programmer mode*, where every button — grid, top row and
//! right column — is addressed by a single index `row * 10 + column`.

use std::sync::mpsc::{Receiver, channel};

use midir::{MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Novation SysEx header for the Launchpad Mini MK3.
const HEADER: [u8; 6] = [0xF0, 0x00, 0x20, 0x29, 0x02, 0x0D];

/// A button, as its programmer-mode index (`row * 10 + column`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Pad(pub u8);

impl Pad {
    /// The logo LED above the right column.
    pub const LOGO: Pad = Pad(99);

    /// Grid button at `x` columns from the left, `y` rows from the top (both `0..8`).
    pub fn xy(x: u8, y: u8) -> Pad {
        assert!(x < 8 && y < 8, "grid is 8x8, got ({x}, {y})");
        Pad((8 - y) * 10 + x + 1)
    }

    /// Top row (the arrows / session buttons), `0..8` left to right.
    pub fn top(i: u8) -> Pad {
        assert!(i < 8, "top row has 8 buttons, got {i}");
        Pad(91 + i)
    }

    /// Right column (the scene launch buttons), `0..8` top to bottom.
    pub fn right(i: u8) -> Pad {
        assert!(i < 8, "right column has 8 buttons, got {i}");
        Pad((8 - i) * 10 + 9)
    }

    /// Column from the left, row from the top — the inverse of [`Pad::xy`].
    /// `None` for the top row, right column and logo, which are outside the grid.
    pub fn to_xy(self) -> Option<(u8, u8)> {
        let (row, col) = (self.0 / 10, self.0 % 10);
        if !(1..=8).contains(&row) || !(1..=8).contains(&col) {
            return None;
        }
        Some((col - 1, 8 - row))
    }
}

/// What an LED should do. Colours are palette indices (`0..128`, 0 = off); see the
/// Launchpad Mini MK3 programmer's reference for the palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Light {
    Off,
    /// Steady palette colour.
    Solid(u8),
    /// Alternates between the two colours in time with the device clock.
    Flash {
        on: u8,
        off: u8,
    },
    /// Fades in and out.
    Pulse(u8),
    /// Arbitrary colour, each channel `0..128`.
    Rgb(u8, u8, u8),
}

impl Light {
    /// The device's "colour spec": one entry of a SysEx lighting message.
    fn spec(self, pad: Pad) -> Vec<u8> {
        match self {
            Light::Off => vec![0, pad.0, 0],
            Light::Solid(c) => vec![0, pad.0, c & 0x7F],
            Light::Flash { on, off } => vec![1, pad.0, off & 0x7F, on & 0x7F],
            Light::Pulse(c) => vec![2, pad.0, c & 0x7F],
            Light::Rgb(r, g, b) => vec![3, pad.0, r & 0x7F, g & 0x7F, b & 0x7F],
        }
    }
}

/// A button event. Grid buttons report the press velocity; the top row and right
/// column are not velocity sensitive and always report 127.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Press(Pad, u8),
    Release(Pad),
}

impl Event {
    pub fn pad(self) -> Pad {
        match self {
            Event::Press(p, _) | Event::Release(p) => p,
        }
    }

    fn parse(msg: &[u8]) -> Option<Event> {
        // Grid pads are notes, the top row and right column are CCs; both carry the
        // programmer-mode index in the first data byte.
        match msg {
            [0x90 | 0xB0, pad, 0] | [0x80, pad, _] => Some(Event::Release(Pad(*pad))),
            [0x90 | 0xB0, pad, velocity] => Some(Event::Press(Pad(*pad), *velocity)),
            _ => None,
        }
    }
}

pub struct Launchpad {
    out: MidiOutputConnection,
    // Held so the input callback keeps running; dropping it closes the port.
    _input: MidiInputConnection<()>,
}

impl Launchpad {
    /// Opens the first connected Launchpad Mini MK3 and switches it to programmer mode.
    /// Returns the device plus the stream of button events.
    pub fn connect() -> Result<(Launchpad, Receiver<Event>)> {
        let input = MidiInput::new("launchpad-in")?;
        let output = MidiOutput::new("launchpad-out")?;

        let in_port = find_port(&input.ports(), |p| input.port_name(p).ok())?;
        let out_port = find_port(&output.ports(), |p| output.port_name(p).ok())?;

        let (tx, rx) = channel();
        let _input = input.connect(
            &in_port,
            "launchpad",
            move |_time, msg, _| {
                if let Some(event) = Event::parse(msg) {
                    // The receiver going away just means nobody is listening any more.
                    let _ = tx.send(event);
                }
            },
            (),
        )?;

        let mut pad = Launchpad {
            out: output.connect(&out_port, "launchpad")?,
            _input,
        };
        pad.set_programmer_mode(true)?;
        pad.clear()?;
        Ok((pad, rx))
    }

    /// Sets one LED.
    pub fn set(&mut self, pad: Pad, light: Light) -> Result<()> {
        self.set_all([(pad, light)])
    }

    /// Sets many LEDs in a single message — far faster than one call per pad.
    pub fn set_all(&mut self, lights: impl IntoIterator<Item = (Pad, Light)>) -> Result<()> {
        let mut msg = Vec::from(HEADER);
        msg.push(0x03); // "colour spec" lighting command
        for (pad, light) in lights {
            msg.extend(light.spec(pad));
        }
        if msg.len() == HEADER.len() + 1 {
            return Ok(()); // nothing to light
        }
        msg.push(0xF7);
        Ok(self.out.send(&msg)?)
    }

    /// Turns every LED off.
    pub fn clear(&mut self) -> Result<()> {
        let all = (1..=9).flat_map(|row| (1..=9).map(move |col| (Pad(row * 10 + col), Light::Off)));
        self.set_all(all)
    }

    fn set_programmer_mode(&mut self, on: bool) -> Result<()> {
        let mut msg = Vec::from(HEADER);
        msg.extend([0x0E, on as u8, 0xF7]);
        Ok(self.out.send(&msg)?)
    }
}

impl Drop for Launchpad {
    fn drop(&mut self) {
        // Leave the device as we found it: dark, and back in its standalone mode.
        let _ = self.clear();
        let _ = self.set_programmer_mode(false);
    }
}

/// Picks the "MIDI" port of the Launchpad — the one programmer mode talks over —
/// rather than the "DAW" port sitting next to it.
fn find_port<P: Clone>(ports: &[P], name_of: impl Fn(&P) -> Option<String>) -> Result<P> {
    let named: Vec<_> = ports
        .iter()
        .filter_map(|p| Some((p, name_of(p)?)))
        .filter(|(_, name)| name.contains("LPMiniMK3") || name.contains("Launchpad Mini MK3"))
        .collect();

    let (port, _) = named
        .iter()
        .find(|(_, name)| name.contains("MI"))
        .or_else(|| named.first())
        .ok_or("no Launchpad Mini MK3 found — is it plugged in?")?;
    Ok((*port).clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pad_addressing_matches_programmer_mode() {
        assert_eq!(Pad::xy(0, 0), Pad(81)); // top-left of the grid
        assert_eq!(Pad::xy(7, 7), Pad(18)); // bottom-right
        assert_eq!(Pad::top(0), Pad(91));
        assert_eq!(Pad::right(0), Pad(89));
        assert_eq!(Pad::right(7), Pad(19));

        for y in 0..8 {
            for x in 0..8 {
                assert_eq!(Pad::xy(x, y).to_xy(), Some((x, y)));
            }
        }
        assert_eq!(Pad::top(3).to_xy(), None);
        assert_eq!(Pad::right(3).to_xy(), None);
        assert_eq!(Pad::LOGO.to_xy(), None);
    }

    #[test]
    fn light_specs_match_the_sysex_reference() {
        assert_eq!(Light::Solid(5).spec(Pad(11)), [0, 11, 5]);
        assert_eq!(Light::Flash { on: 5, off: 1 }.spec(Pad(11)), [1, 11, 1, 5]);
        assert_eq!(Light::Pulse(5).spec(Pad(11)), [2, 11, 5]);
        assert_eq!(Light::Rgb(127, 0, 64).spec(Pad(11)), [3, 11, 127, 0, 64]);
    }

    #[test]
    fn events_parse_from_raw_midi() {
        assert_eq!(
            Event::parse(&[0x90, 81, 100]),
            Some(Event::Press(Pad(81), 100))
        );
        assert_eq!(Event::parse(&[0x90, 81, 0]), Some(Event::Release(Pad(81))));
        assert_eq!(Event::parse(&[0x80, 81, 64]), Some(Event::Release(Pad(81))));
        assert_eq!(
            Event::parse(&[0xB0, 91, 127]),
            Some(Event::Press(Pad(91), 127))
        );
        assert_eq!(Event::parse(&[0xB0, 91, 0]), Some(Event::Release(Pad(91))));
        assert_eq!(Event::parse(&[0xF8]), None);
    }
}
