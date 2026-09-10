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
    /// What the brightness was before we touched it, put back on drop. `None` if
    /// the device did not answer the readback in time, in which case drop leaves
    /// brightness alone rather than guessing at a value.
    brightness_was: Option<u8>,
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
        let (brightness_tx, brightness_rx) = channel();
        let _input = input.connect(
            &in_port,
            "launchpad",
            move |_time, msg, _| {
                // The receivers going away just means nobody is listening any more.
                if let Some(level) = parse_brightness(msg) {
                    let _ = brightness_tx.send(level);
                } else if let Some(event) = Event::parse(msg) {
                    let _ = tx.send(event);
                }
            },
            (),
        )?;

        let mut pad = Launchpad {
            out: output.connect(&out_port, "launchpad")?,
            _input,
            brightness_was: None,
        };
        pad.set_programmer_mode(true)?;

        // Ask what the brightness is before changing it, so drop can put it back.
        // The manual documents no default value to fall back on.
        pad.out.send(&command(&[0x08]))?;
        pad.brightness_was = brightness_rx
            .recv_timeout(std::time::Duration::from_millis(250))
            .ok();

        pad.set_brightness(127)?;
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

    /// Sets the global LED brightness, 0 (dimmest) to 127.
    ///
    /// This is the same setting as the Setup menu's LED page, and it **persists on
    /// the device** — it survives unplugging, and nothing here puts it back.
    /// [`Launchpad::connect`] turns it up to full, because a device left on a low
    /// level makes every effect look broken in a way the code cannot explain.
    pub fn set_brightness(&mut self, level: u8) -> Result<()> {
        Ok(self.out.send(&command(&[0x08, level & 0x7F]))?)
    }

    fn set_programmer_mode(&mut self, on: bool) -> Result<()> {
        Ok(self.out.send(&command(&[0x0E, on as u8]))?)
    }
}

/// Wraps a payload in the device's SysEx header and terminator.
fn command(payload: &[u8]) -> Vec<u8> {
    let mut msg = Vec::from(HEADER);
    msg.extend(payload);
    msg.push(0xF7);
    msg
}

impl Drop for Launchpad {
    fn drop(&mut self) {
        // Leave the device as we found it: dark, at the brightness it came with,
        // and back in its standalone mode.
        let _ = self.clear();
        if let Some(level) = self.brightness_was {
            let _ = self.set_brightness(level);
        }
        let _ = self.set_programmer_mode(false);
    }
}

/// The device's reply to a brightness readback: the header, `08h`, the level.
fn parse_brightness(msg: &[u8]) -> Option<u8> {
    match msg {
        [header @ .., 0x08, level, 0xF7] if header == HEADER => Some(*level),
        _ => None,
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

/// Exponent correcting the LEDs' non-linear response, applied when a [`Grid`] is
/// encoded for the device. A linear ramp sent raw spends almost all of its visible
/// change in the bottom quarter, so fades look like they snap off early; ~2.2 makes
/// a linear ramp *look* linear. It is a per-eye, per-device knob — tune it here.
pub const GAMMA: f32 = 2.2;

/// Linear-light colour, `0.0..=1.0` per channel. Linear so that adding, scaling and
/// fading behave the way the arithmetic in an effect expects; the gamma curve is
/// applied once, on the way out.
pub type Rgb = [f32; 3];

/// `h` wraps, `s` and `v` clamp to `0.0..=1.0`.
pub fn hsv(h: f32, s: f32, v: f32) -> Rgb {
    let (s, v) = (s.clamp(0.0, 1.0), v.clamp(0.0, 1.0));
    let h = (h.rem_euclid(1.0)) * 6.0;
    let (c, x) = (v * s, v * s * (1.0 - (h % 2.0 - 1.0).abs()));
    let m = v - c;
    let [r, g, b] = match h as u32 {
        0 => [c, x, 0.0],
        1 => [x, c, 0.0],
        2 => [0.0, c, x],
        3 => [0.0, x, c],
        4 => [x, 0.0, c],
        _ => [c, 0.0, x],
    };
    [r + m, g + m, b + m]
}

/// The 8x8 grid as a frame buffer of linear colour, written by an effect and handed
/// to [`Launchpad::set_all`] via [`Grid::lights`].
///
/// Coordinates are `i32` and match [`Pad::xy`] — x from the left, y from the top.
/// Reads and writes outside the grid are silently ignored rather than panicking,
/// because effects that ripple, sweep or scroll compute off-grid constantly and
/// bounds-checking every one of them at the call site is noise.
#[derive(Clone)]
pub struct Grid {
    px: [Rgb; 64],
}

impl Default for Grid {
    fn default() -> Grid {
        Grid::new()
    }
}

impl Grid {
    pub fn new() -> Grid {
        Grid { px: [[0.0; 3]; 64] }
    }

    fn index(x: i32, y: i32) -> Option<usize> {
        ((0..8).contains(&x) && (0..8).contains(&y)).then(|| (y * 8 + x) as usize)
    }

    /// Black outside the grid.
    pub fn get(&self, x: i32, y: i32) -> Rgb {
        Grid::index(x, y).map_or([0.0; 3], |i| self.px[i])
    }

    pub fn set(&mut self, x: i32, y: i32, c: Rgb) {
        if let Some(i) = Grid::index(x, y) {
            self.px[i] = c;
        }
    }

    /// Adds light rather than replacing it — overlapping sparks brighten.
    pub fn add(&mut self, x: i32, y: i32, c: Rgb) {
        if let Some(i) = Grid::index(x, y) {
            for (channel, added) in self.px[i].iter_mut().zip(c) {
                *channel += added;
            }
        }
    }

    /// Scales every pixel, for decay trails. `k` is per-frame, so it is frame-rate
    /// dependent by design — effects tune it against the clock they run at.
    pub fn fade(&mut self, k: f32) {
        for px in &mut self.px {
            for channel in px {
                *channel *= k;
            }
        }
    }

    pub fn fill(&mut self, c: Rgb) {
        self.px = [c; 64];
    }

    pub fn clear(&mut self) {
        self.fill([0.0; 3]);
    }

    /// The frame as device lights: clamped, gamma-encoded and paired with its pad.
    pub fn lights(&self) -> impl Iterator<Item = (Pad, Light)> + '_ {
        self.px.iter().enumerate().map(|(i, c)| {
            let [r, g, b] = c.map(|v| (v.clamp(0.0, 1.0).powf(GAMMA) * 127.0).round() as u8);
            (Pad::xy((i % 8) as u8, (i / 8) as u8), Light::Rgb(r, g, b))
        })
    }
}

/// Fixed-rate frame clock. [`Clock::tick`] spends the time between frames blocking
/// on the event channel instead of sleeping, so an effect renders on schedule and
/// still sees presses the moment they happen — no threads, no async.
pub struct Clock {
    period: std::time::Duration,
    start: std::time::Instant,
    next: std::time::Instant,
    frames: u32,
}

impl Clock {
    pub fn new(fps: f64) -> Clock {
        let now = std::time::Instant::now();
        Clock {
            period: std::time::Duration::from_secs_f64(1.0 / fps),
            start: now,
            next: now,
            frames: 0,
        }
    }

    /// Blocks until the next frame is due, collecting button events into `out`.
    /// Returns seconds elapsed since the clock started.
    pub fn tick(&mut self, events: &Receiver<Event>, out: &mut Vec<Event>) -> f32 {
        use std::sync::mpsc::RecvTimeoutError;

        out.clear();
        self.next += self.period;
        while let Some(left) = self.next.checked_duration_since(std::time::Instant::now()) {
            match events.recv_timeout(left) {
                Ok(event) => out.push(event),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    std::thread::sleep(left);
                    break;
                }
            }
        }
        // Rendering overran the budget: give up on catching the lost frames back,
        // or the clock spins with no wait at all until it does.
        self.next = self.next.max(std::time::Instant::now());
        self.frames += 1;
        self.start.elapsed().as_secs_f32()
    }

    /// Frames per second actually achieved — the honest number, not the requested
    /// one. Print it to find where the device or the host starts falling behind.
    pub fn fps(&self) -> f32 {
        self.frames as f32 / self.start.elapsed().as_secs_f32().max(f32::EPSILON)
    }
}

/// xorshift64*. Here rather than in each app because apps share code only through
/// this crate, and pulling `rand` in for what fire and starfield need would be a
/// dependency tree in exchange for six lines.
pub struct Rng(u64);

impl Default for Rng {
    fn default() -> Rng {
        Rng::new()
    }
}

impl Rng {
    /// Seeded from the clock.
    pub fn new() -> Rng {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x2545_F491_4F6C_DD1D, |d| d.as_nanos() as u64);
        Rng::seeded(nanos)
    }

    pub fn seeded(seed: u64) -> Rng {
        Rng(seed | 1) // xorshift is stuck forever at zero
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// `0.0..1.0`
    pub fn f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u32 << 24) as f32
    }

    /// `0..n`
    pub fn below(&mut self, n: u32) -> u32 {
        (self.next_u64() % n.max(1) as u64) as u32
    }

    pub fn chance(&mut self, probability: f32) -> bool {
        self.f32() < probability
    }
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
    fn commands_match_the_manual() {
        // Launchpad Mini [MK3] Programmer's Reference, verbatim.
        assert_eq!(
            command(&[0x08, 127]),
            [0xF0, 0x00, 0x20, 0x29, 0x02, 0x0D, 0x08, 0x7F, 0xF7]
        );
        assert_eq!(
            command(&[0x0E, 1]),
            [0xF0, 0x00, 0x20, 0x29, 0x02, 0x0D, 0x0E, 0x01, 0xF7]
        );
    }

    #[test]
    fn brightness_replies_parse_and_nothing_else_does() {
        assert_eq!(parse_brightness(&command(&[0x08, 96])), Some(96));
        assert_eq!(parse_brightness(&command(&[0x08, 0])), Some(0));
        // The device also confirms mode changes; those must not be mistaken for it.
        assert_eq!(parse_brightness(&command(&[0x0E, 0])), None);
        assert_eq!(parse_brightness(&[0x90, 0x08, 0x7F]), None);
        assert_eq!(parse_brightness(&command(&[0x08])), None); // the request itself
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

    fn close(a: Rgb, b: Rgb) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5)
    }

    #[test]
    fn hsv_hits_the_primaries_and_wraps() {
        assert!(close(hsv(0.0, 1.0, 1.0), [1.0, 0.0, 0.0]));
        assert!(close(hsv(1.0 / 3.0, 1.0, 1.0), [0.0, 1.0, 0.0]));
        assert!(close(hsv(2.0 / 3.0, 1.0, 1.0), [0.0, 0.0, 1.0]));
        assert!(close(hsv(1.0, 1.0, 1.0), [1.0, 0.0, 0.0]), "hue wraps");
        assert!(
            close(hsv(-0.5, 1.0, 1.0), hsv(0.5, 1.0, 1.0)),
            "negative hue wraps"
        );
        assert!(close(hsv(0.7, 0.0, 0.5), [0.5; 3]), "no saturation is grey");
    }

    #[test]
    fn grid_writes_ignore_off_grid_and_encode_with_gamma() {
        let mut grid = Grid::new();
        grid.set(3, 4, [1.0, 0.5, 0.0]);
        assert_eq!(grid.get(3, 4), [1.0, 0.5, 0.0]);

        grid.set(-1, 0, [1.0; 3]); // must not panic or wrap onto another row
        grid.set(8, 0, [1.0; 3]);
        assert_eq!(grid.get(-1, 0), [0.0; 3]);
        assert_eq!(grid.get(7, 0), [0.0; 3]);

        grid.add(3, 4, [0.0, 0.5, 0.25]);
        assert_eq!(grid.get(3, 4), [1.0, 1.0, 0.25]);
        grid.fade(0.5);
        assert_eq!(grid.get(3, 4), [0.5, 0.5, 0.125]);

        let lights: Vec<_> = grid.lights().collect();
        assert_eq!(lights.len(), 64);
        assert_eq!(lights[0].0, Pad::xy(0, 0));
        assert_eq!(lights[63].0, Pad::xy(7, 7));

        grid.fill([1.0; 3]);
        assert_eq!(grid.lights().next().unwrap().1, Light::Rgb(127, 127, 127));
        grid.clear();
        assert_eq!(grid.lights().next().unwrap().1, Light::Rgb(0, 0, 0));

        // Gamma pulls mid-grey down; without it fades look like they snap off early.
        grid.fill([0.5; 3]);
        let Light::Rgb(mid, _, _) = grid.lights().next().unwrap().1 else {
            panic!("expected Rgb")
        };
        assert!((1..64).contains(&mid), "mid-grey encoded as {mid}");
    }

    #[test]
    fn rng_stays_in_range_and_never_sticks() {
        let mut rng = Rng::seeded(0); // the one seed xorshift cannot survive raw
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            let f = rng.f32();
            assert!((0.0..1.0).contains(&f), "f32 out of range: {f}");
            assert!(rng.below(8) < 8);
            seen.insert(rng.next_u64());
        }
        assert!(
            seen.len() > 900,
            "rng stuck: only {} distinct values",
            seen.len()
        );
    }

    #[test]
    fn clock_advances_and_reports_achieved_rate() {
        let (tx, rx) = channel();
        drop(tx); // no device: tick must still pace itself, not spin
        let mut clock = Clock::new(500.0);
        let mut events = Vec::new();

        let first = clock.tick(&rx, &mut events);
        let mut last = first;
        for _ in 0..4 {
            let now = clock.tick(&rx, &mut events);
            assert!(now >= last, "time went backwards: {last} -> {now}");
            last = now;
        }
        assert!(last > first, "clock never advanced");
        assert!(clock.fps() > 0.0);
        assert!(events.is_empty());
    }
}
