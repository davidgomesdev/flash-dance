//! Eighteen LED effects with a picker, a speed control and a brightness control,
//! as a library: [`run`] is the whole session, and both the `effects` binary and
//! the web server call it. Four of the effects listen to an audio source.
//!
//! - **User** (top row, rightmost) opens the picker; tap a lit pad to choose.
//! - **Session** (top row, fifth) shows the [`Mood`]s someone else named; tap one
//!   and the effect stops, the mood's pad pulsing for a few seconds. Session again
//!   goes back to the effect, if one is still showing.
//! - **Up / down** change brightness. No on-screen indicator: the whole grid gets
//!   brighter or dimmer, which is the indicator.
//! - **Left / right** change speed. The right column shows it, blue below 1x,
//!   green at 1x, red above.
//! - **Stop/Solo/Mute** (bottom of the right column) quits.
//!
//! Both settings are global — they survive switching effects, because they live
//! out here rather than in the per-effect state that [`App::reset`] wipes.
//! Brightness is global in a stronger sense too: it is a setting on the device
//! itself, so whatever it is left at persists after the app exits.
//!
//! Effects run on a virtual clock, so changing speed scales everything they do —
//! motion, decay trails, spawn rates — rather than only the ones written against a
//! wall clock. `pressure map` is the one effect with no time in it at all.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use flash_dance::{Clock, Event, Grid, Launchpad, Light, Pad, Result, Rng, hsv};
use flash_dance_audio::{Audio, Sound};

const MENU: Pad = Pad(98); // top row, rightmost, "User"
const SESSION: Pad = Pad(95); // top row, fifth, "Session"
const FPS: f64 = 30.0;
/// Seconds a pressed mood pulses: long enough to see it took, short enough not to nag.
const GLOW: f32 = 5.0;

fn quit_button() -> Pad {
    Pad::right(7) // bottom of the right column, "Stop/Solo/Mute"
}

/// Discrete speeds, one per LED of the speed bar. Index 3 is normal.
const SPEEDS: [f32; 7] = [0.125, 0.25, 0.5, 1.0, 2.0, 4.0, 8.0];
const NORMAL_SPEED: usize = 3;

/// Device brightness levels, matching the eight the Setup menu offers. Starts at
/// the top: the device is often found near the bottom of this range, which makes
/// every effect look broken for reasons the code cannot explain.
const BRIGHTNESS: [u8; 8] = [16, 32, 48, 64, 80, 96, 112, 127];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    // Ambient: a function of time, no input.
    Rainbow,
    Plasma,
    Fire,
    Rain,
    Rings,
    Radar,
    Starfield,
    Wipe,
    // Reactive: driven by what you press.
    PressRipple,
    Comet,
    VelocityHeat,
    Lasers,
    Gravity,
    PressureMap,
    // Sound: driven by what the machine hears.
    Spectrum,
    PitchMeter,
    BeatRings,
    SoundFire,
}

use Effect::*;

/// Every effect, in picker order: grid position `i` on the device is `EFFECTS[i]`.
pub const EFFECTS: [Effect; 18] = [
    Rainbow,
    Plasma,
    Fire,
    Rain,
    Rings,
    Radar,
    Starfield,
    Wipe,
    PressRipple,
    Comet,
    VelocityHeat,
    Lasers,
    Gravity,
    PressureMap,
    Spectrum,
    PitchMeter,
    BeatRings,
    SoundFire,
];

impl Effect {
    pub fn name(self) -> &'static str {
        match self {
            Rainbow => "rainbow diagonal",
            Plasma => "plasma",
            Fire => "fire",
            Rain => "rain",
            Rings => "ripple rings",
            Radar => "radar sweep",
            Starfield => "starfield",
            Wipe => "colour wipe",
            PressRipple => "ripple on press",
            Comet => "comet trail",
            VelocityHeat => "velocity heat",
            Lasers => "lasers",
            Gravity => "gravity",
            PressureMap => "pressure map",
            Spectrum => "spectrum",
            PitchMeter => "pitch meter",
            BeatRings => "beat rings",
            SoundFire => "sound fire",
        }
    }

    /// What it looks like and what it responds to, in a sentence or two.
    pub fn about(self) -> &'static str {
        match self {
            Rainbow => "A rainbow laid diagonally across the grid, drifting slowly.",
            Plasma => "Overlapping waves of colour that swirl and melt into each other.",
            Fire => "Flames rising from the bottom row, white-hot at the base and red at the tips.",
            Rain => "Blue-green drops falling down random columns, leaving short trails.",
            Rings => "Rings of colour pulsing out from the centre, one after another.",
            Radar => "A beam turning around the centre, leaving a fading afterglow.",
            Starfield => "Stars blinking on at random and slowly fading out.",
            Wipe => "A solid colour sweeping across from left to right, a new one each pass.",
            PressRipple => {
                "Press a pad and a ring spreads out from it. \
                 Soft presses are red, harder ones shift through yellow to green."
            }
            Comet => {
                "Pads light up when pressed and fade slowly, so a swipe leaves a trail. \
                 The colour follows the column."
            }
            VelocityHeat => "Pads light by how hard you hit them: soft is blue, hard is red.",
            Lasers => "Each press fires beams along its row and column, crossing in a white flash.",
            Gravity => "Pressed pads fall to the bottom and stack up. A full column empties.",
            PressureMap => {
                "A heat map of where you have played: each press warms a pad from blue \
                 towards red, and nothing fades."
            }
            Spectrum => "A bar per frequency band, bass on the left and treble on the right.",
            PitchMeter => {
                "Fills the grid from the bottom: low for bass-heavy sound, high for bright, \
                 trebly sound. Dark in silence."
            }
            BeatRings => {
                "A ring bursts from the centre on every beat, a new colour each time, \
                 over a glow that follows the bass."
            }
            SoundFire => {
                "The fire, fed by the bass: embers when it is thin, a flare-up on every kick."
            }
        }
    }

    /// Signature colour, used for this effect's pad in the picker.
    pub fn hue(self) -> f32 {
        EFFECTS.iter().position(|e| *e == self).unwrap_or(0) as f32 / EFFECTS.len() as f32
    }

    /// Which of the three kinds this is: a function of time, of your hands, or of
    /// what the machine is playing. Matched exhaustively so that adding an effect
    /// without placing it is a compile error rather than a silent "ambient".
    pub fn group(self) -> &'static str {
        match self {
            Rainbow | Plasma | Fire | Rain | Rings | Radar | Starfield | Wipe => "ambient",
            PressRipple | Comet | VelocityHeat | Lasers | Gravity | PressureMap => "reactive",
            Spectrum | PitchMeter | BeatRings | SoundFire => "sound",
        }
    }
}

/// Per-second decay as a per-frame factor. Trails then look the same whatever the
/// frame rate, and speed reaches them like it reaches everything else.
fn decay(per_second: f32, dt: f32) -> f32 {
    per_second.powf(dt)
}

struct Drop {
    x: i32,
    y: f32,
    speed: f32,
    hue: f32,
}

struct Ripple {
    x: f32,
    y: f32,
    start: f32,
    hue: f32,
}

struct Fall {
    x: i32,
    y: f32,
    vy: f32,
    hue: f32,
}

/// All effect state, flat. Everything is reset on a switch, so effects can share
/// `scratch` (fire's heat, then the pressure map's accumulator) without confusion.
struct App {
    grid: Grid,
    rng: Rng,
    scratch: [f32; 64],
    embers: f32, // fire's propagation-step accumulator
    drops: Vec<Drop>,
    ripples: Vec<Ripple>,
    falling: Vec<Fall>,
    stack: [i32; 8],
    hue: f32, // walks on every beat
    /// Opened the first time an effect listens, and then left open: nothing else
    /// needs a recording stream, and reopening it on every switch back is worse.
    audio: Option<Audio>,
    listening: bool,
    source: Option<String>,
}

impl App {
    fn new(source: Option<String>) -> App {
        App {
            grid: Grid::new(),
            rng: Rng::new(),
            scratch: [0.0; 64],
            embers: 0.0,
            drops: Vec::new(),
            ripples: Vec::new(),
            falling: Vec::new(),
            stack: [0; 8],
            hue: 0.0,
            audio: None,
            listening: false,
            source,
        }
    }

    /// The last window of sound, silence if there is nothing to listen to. The
    /// failure is worth a line on stderr and nothing more: the effect still runs,
    /// it just sits dark, which is what silence looks like anyway.
    fn sound(&mut self) -> Sound {
        if !self.listening {
            self.listening = true;
            match Audio::open(self.source.as_deref()) {
                Ok(audio) => {
                    println!("listening to {}", audio.source());
                    self.audio = Some(audio);
                }
                Err(e) => eprintln!("no sound: {e}"),
            }
        }
        self.audio.as_mut().map_or(Sound::default(), Audio::listen)
    }

    /// Wipes the previous effect's state and blanks the device.
    fn reset(&mut self, pad: &mut Launchpad) -> Result<()> {
        self.grid.clear();
        self.scratch = [0.0; 64];
        self.embers = 0.0;
        self.drops.clear();
        self.ripples.clear();
        self.falling.clear();
        self.stack = [0; 8];
        self.hue = 0.0;
        pad.clear()
    }

    /// `t` and `dt` are virtual: already scaled by the speed setting.
    fn render(
        &mut self,
        pad: &mut Launchpad,
        effect: Effect,
        t: f32,
        dt: f32,
        events: &[Event],
    ) -> Result<()> {
        match effect {
            Rainbow => self.rainbow(t),
            Plasma => self.plasma(t),
            Fire => self.fire(dt, 1.0),
            Rain => self.rain(dt),
            Rings => self.rings(t),
            Radar => self.radar(t, dt),
            Starfield => self.starfield(dt),
            Wipe => self.wipe(t),
            PressRipple => self.press_ripple(t, events),
            Comet => self.comet(dt, events),
            VelocityHeat => self.velocity_heat(dt, events),
            Lasers => self.lasers(dt, events),
            Gravity => self.gravity(dt, events),
            PressureMap => self.pressure_map(events),
            Spectrum => {
                let heard = self.sound();
                self.spectrum(heard)
            }
            PitchMeter => {
                let heard = self.sound();
                self.pitch_meter(heard)
            }
            BeatRings => {
                let heard = self.sound();
                self.beat_rings(t, heard)
            }
            SoundFire => {
                let heard = self.sound();
                // Quiet is embers, a kick drum is a flare-up.
                self.fire(dt, 0.2 + heard.bass() * 1.3)
            }
        }
        pad.set_all(self.grid.lights())
    }

    fn rainbow(&mut self, t: f32) {
        for (x, y) in cells() {
            self.grid
                .set(x, y, hsv((x + y) as f32 / 14.0 + t * 0.2, 1.0, 1.0));
        }
    }

    fn plasma(&mut self, t: f32) {
        for (x, y) in cells() {
            let (fx, fy) = (x as f32, y as f32);
            let v = (fx * 0.7 + t).sin() + (fy * 0.6 - t * 1.3).sin() + ((fx + fy) * 0.4 + t).sin();
            self.grid.set(x, y, hsv(v / 6.0 + t * 0.05, 1.0, 1.0));
        }
    }

    /// `fuel` scales the heat fed in at the bottom row: 1.0 is a steady fire, 0 is
    /// out. It is what `sound fire` modulates.
    fn fire(&mut self, dt: f32, fuel: f32) {
        // Propagate on its own clock rather than once per frame, so the flames rise
        // at the same rate whatever the frame rate — and respond to speed.
        self.embers = (self.embers + dt * 20.0).min(4.0); // never spiral catching up
        while self.embers >= 1.0 {
            self.embers -= 1.0;
            for x in 0..8 {
                self.scratch[(7 * 8 + x) as usize] = (0.6 + self.rng.f32() * 0.4) * fuel;
            }
            for y in 0..7 {
                for x in 0..8 {
                    let drift = self.rng.below(3) as i32 - 1;
                    let below = ((x + drift).clamp(0, 7) + (y + 1) * 8) as usize;
                    let cooled = self.scratch[below] - self.rng.f32() * 0.28;
                    self.scratch[(y * 8 + x) as usize] = cooled.max(0.0);
                }
            }
        }
        for (x, y) in cells() {
            let heat = self.scratch[(y * 8 + x) as usize];
            // Hue runs red to yellow, and the hottest cells wash out towards white.
            let white = ((heat - 0.85) * 4.0).clamp(0.0, 1.0);
            self.grid.set(x, y, hsv(heat * 0.09, 1.0 - white, heat));
        }
    }

    fn rain(&mut self, dt: f32) {
        self.grid.fade(decay(1e-6, dt));
        if self.rng.chance((10.0 * dt).min(1.0)) {
            self.drops.push(Drop {
                x: self.rng.below(8) as i32,
                y: -1.0,
                speed: 4.0 + self.rng.f32() * 6.0,
                hue: 0.45 + self.rng.f32() * 0.15,
            });
        }
        for drop in &mut self.drops {
            drop.y += drop.speed * dt;
            self.grid
                .add(drop.x, drop.y as i32, hsv(drop.hue, 0.7, 1.0));
        }
        self.drops.retain(|d| d.y < 9.0);
    }

    fn rings(&mut self, t: f32) {
        for (x, y) in cells() {
            let d = ((x as f32 - 3.5).powi(2) + (y as f32 - 3.5).powi(2)).sqrt();
            let wave = (d * 1.3 - t * 3.0).sin().max(0.0).powi(3);
            self.grid.set(x, y, hsv(d / 8.0 + t * 0.1, 1.0, wave));
        }
    }

    fn radar(&mut self, t: f32, dt: f32) {
        self.grid.fade(decay(0.003, dt));
        let angle = t * 2.0;
        let hue = (t * 0.1).rem_euclid(1.0);
        // Walk out along the beam rather than testing every cell's angle: near the
        // centre a fixed angular width covers almost no pads at all.
        let mut r = 0.0;
        while r < 5.0 {
            let (x, y) = (3.5 + angle.cos() * r, 3.5 + angle.sin() * r);
            self.grid
                .add(x.round() as i32, y.round() as i32, hsv(hue, 0.8, 0.5));
            r += 0.25;
        }
    }

    fn starfield(&mut self, dt: f32) {
        self.grid.fade(decay(0.04, dt));
        if self.rng.chance((12.0 * dt).min(1.0)) {
            let (x, y) = (self.rng.below(8) as i32, self.rng.below(8) as i32);
            self.grid.set(x, y, hsv(self.rng.f32(), 0.4, 1.0));
        }
    }

    fn wipe(&mut self, t: f32) {
        const SPAN: f32 = 10.0; // 8 columns plus a beat of solid colour at each end
        let sweep = t * 3.0;
        let pass = (sweep / SPAN) as u32;
        let front = sweep.rem_euclid(SPAN) - 1.0;
        let (behind, ahead) = (hsv(pass as f32 * 0.17, 1.0, 1.0), {
            hsv(pass.wrapping_sub(1) as f32 * 0.17, 1.0, 1.0)
        });
        for (x, y) in cells() {
            let colour = if (x as f32) < front { behind } else { ahead };
            self.grid.set(x, y, colour);
        }
    }

    fn press_ripple(&mut self, t: f32, events: &[Event]) {
        for (x, y, velocity) in presses(events) {
            self.ripples.push(Ripple {
                x: x as f32,
                y: y as f32,
                start: t,
                hue: velocity as f32 / 127.0 * 0.4,
            });
        }
        self.grid.clear();
        self.draw_ripples(t);
    }

    /// Expanding rings, one per entry in `ripples`, fading as they grow. The caller
    /// paints the background first — this only adds light.
    fn draw_ripples(&mut self, t: f32) {
        for ripple in &self.ripples {
            let age = t - ripple.start;
            let radius = age * 7.0;
            let strength = (1.0 - age / 1.5).max(0.0);
            for (x, y) in cells() {
                let d = ((x as f32 - ripple.x).powi(2) + (y as f32 - ripple.y).powi(2)).sqrt();
                let ring = (-(d - radius).powi(2) * 2.0).exp();
                self.grid.add(x, y, hsv(ripple.hue, 1.0, ring * strength));
            }
        }
        self.ripples.retain(|r| t - r.start < 1.5);
    }

    fn comet(&mut self, dt: f32, events: &[Event]) {
        self.grid.fade(decay(0.02, dt));
        for (x, y, _) in presses(events) {
            self.grid.set(x, y, hsv(x as f32 / 8.0, 1.0, 1.0));
        }
    }

    fn velocity_heat(&mut self, dt: f32, events: &[Event]) {
        self.grid.fade(decay(0.4, dt));
        for (x, y, velocity) in presses(events) {
            // Soft is blue, hard is red — the byte `paint` throws away.
            let hue = 0.66 * (1.0 - velocity as f32 / 127.0);
            self.grid.set(x, y, hsv(hue, 1.0, 1.0));
        }
    }

    fn lasers(&mut self, dt: f32, events: &[Event]) {
        self.grid.fade(decay(0.0012, dt));
        for (x, y, _) in presses(events) {
            let hue = (x + y) as f32 / 14.0;
            for i in 0..8 {
                self.grid.add(i, y, hsv(hue, 1.0, 0.6));
                self.grid.add(x, i, hsv(hue, 1.0, 0.6));
            }
            self.grid.set(x, y, [1.0; 3]);
        }
    }

    fn gravity(&mut self, dt: f32, events: &[Event]) {
        for (x, y, _) in presses(events) {
            self.falling.push(Fall {
                x,
                y: y as f32,
                vy: 0.0,
                hue: self.rng.f32(),
            });
        }
        for fall in &mut self.falling {
            fall.vy += 24.0 * dt;
            fall.y += fall.vy * dt;
        }
        // Landed pads join the stack in their column; a full column collapses.
        let stack = &mut self.stack;
        self.falling.retain(|fall| {
            let floor = 7 - stack[fall.x as usize];
            if fall.y < floor as f32 {
                return true;
            }
            stack[fall.x as usize] += 1;
            false
        });
        for height in stack.iter_mut() {
            if *height >= 8 {
                *height = 0;
            }
        }

        self.grid.clear();
        for (x, height) in self.stack.iter().enumerate() {
            for i in 0..*height {
                self.grid.set(x as i32, 7 - i, hsv(0.08, 0.8, 0.5));
            }
        }
        for fall in &self.falling {
            self.grid
                .add(fall.x, fall.y.round() as i32, hsv(fall.hue, 1.0, 1.0));
        }
    }

    fn pressure_map(&mut self, events: &[Event]) {
        for (x, y, _) in presses(events) {
            let cell = &mut self.scratch[(y * 8 + x) as usize];
            *cell = (*cell + 0.2).min(1.0);
        }
        for (x, y) in cells() {
            let v = self.scratch[(y * 8 + x) as usize];
            // Cold blue through to hot red, dark where you have never played.
            self.grid.set(x, y, hsv(0.66 * (1.0 - v), 1.0, v));
        }
    }

    /// A bar per octave band, bass on the left, rising from the bottom row.
    fn spectrum(&mut self, sound: Sound) {
        for (x, y) in cells() {
            let height = sound.bands[x as usize] * 8.0;
            // The topmost cell of a bar dims rather than snapping on, which is the
            // whole reason the grid holds linear colour.
            let fill = (height - (7 - y) as f32).clamp(0.0, 1.0);
            self.grid.set(x, y, hsv(x as f32 / 12.0, 1.0, fill));
        }
    }

    /// The whole grid as one 64-LED meter of where the sound sits between bass and
    /// treble, filling row by row from the bottom left. Dark in silence.
    fn pitch_meter(&mut self, sound: Sound) {
        let lit = sound.pitch * 64.0;
        for (x, y) in cells() {
            let led = ((7 - y) * 8 + x) as f32;
            // Red at the bass end of the scale, blue at the treble end.
            self.grid.set(
                x,
                y,
                hsv(0.66 * led / 63.0, 1.0, (lit - led).clamp(0.0, 1.0)),
            );
        }
    }

    /// A ring out of the centre on every beat, over a glow that follows the bass.
    fn beat_rings(&mut self, t: f32, sound: Sound) {
        if sound.beat {
            self.hue = (self.hue + 0.13).rem_euclid(1.0);
            self.ripples.push(Ripple {
                x: 3.5,
                y: 3.5,
                start: t,
                hue: self.hue,
            });
        }
        self.grid.fill(hsv(self.hue, 1.0, sound.bass() * 0.12));
        self.draw_ripples(t);
    }
}

/// Every grid coordinate, x from the left and y from the top.
fn cells() -> impl Iterator<Item = (i32, i32)> {
    (0..8).flat_map(|y| (0..8).map(move |x| (x, y)))
}

/// Grid presses only, as `(x, y, velocity)` — the edge buttons are the app's own.
fn presses(events: &[Event]) -> impl Iterator<Item = (i32, i32, u8)> + '_ {
    events.iter().filter_map(|event| match event {
        Event::Press(pad, velocity) => {
            let (x, y) = pad.to_xy()?;
            Some((x as i32, y as i32, *velocity))
        }
        _ => None,
    })
}

/// What an arrow does. The Mini MK3's top row starts with up, down, left, right:
/// the vertical pair is brightness, the horizontal pair is speed.
enum Nudge {
    Brightness(i32),
    Speed(i32),
}

fn nudge(button: Pad) -> Option<Nudge> {
    match button {
        b if b == Pad::top(0) => Some(Nudge::Brightness(1)),
        b if b == Pad::top(1) => Some(Nudge::Brightness(-1)),
        b if b == Pad::top(2) => Some(Nudge::Speed(-1)),
        b if b == Pad::top(3) => Some(Nudge::Speed(1)),
        _ => None,
    }
}

/// Steps an index within `len`, returning `None` when it is already at the end so
/// the caller can stay quiet rather than reprinting a setting that did not move.
fn step(index: usize, by: i32, len: usize) -> Option<usize> {
    let moved = (index as i32 + by).clamp(0, len as i32 - 1) as usize;
    (moved != index).then_some(moved)
}

/// The buttons that do something, in a dim white: noticeable, but not competing
/// with the effect. The four arrows, moods and the picker; quit has its own colour.
fn controls() -> impl Iterator<Item = (Pad, Light)> {
    (0..4)
        .map(Pad::top)
        .chain([SESSION, MENU])
        .map(|b| (b, Light::Rgb(30, 30, 30)))
}

/// Speed as a bar up the right column, leaving the bottom button as quit.
fn draw_speed(pad: &mut Launchpad, level: usize) -> Result<()> {
    let colour = match level {
        l if l < NORMAL_SPEED => 45, // blue, slower than normal
        l if l > NORMAL_SPEED => 5,  // red, faster
        _ => 21,                     // green at 1x
    };
    let bar = (0..7).map(|i| {
        let light = if i <= level {
            Light::Solid(colour)
        } else {
            Light::Off
        };
        (Pad::right(6 - i as u8), light)
    });
    pad.set_all(
        bar.chain([(quit_button(), Light::Solid(7))])
            .chain(controls()),
    )
}

/// One pad per effect in its signature colour, the current one pulsing white.
fn draw_picker(pad: &mut Launchpad, current: Option<usize>) -> Result<()> {
    pad.clear()?;
    pad.set_all(
        controls().chain(EFFECTS.iter().enumerate().map(|(i, effect)| {
            let target = Pad::xy((i % 8) as u8, (i / 8) as u8);
            if Some(i) == current {
                (target, Light::Pulse(3))
            } else {
                let [r, g, b] = hsv(effect.hue(), 1.0, 1.0).map(|c| (c * 127.0) as u8);
                (target, Light::Rgb(r, g, b))
            }
        })),
    )
}

/// What a session tells whoever started it, as it happens.
#[derive(Clone, Debug, PartialEq)]
pub enum Report {
    /// The effect now showing: the one it started on, then each one picked.
    Showing(Effect),
    /// A mood was pressed, by name. Nothing is showing from then until an effect
    /// is picked.
    Mood(String),
    /// Every button event, controls included, as the device sent it, except the
    /// picker's and the moods': choosing one is not pressing a pad.
    Event(Event),
}

/// A pad someone else names and colours, Home Assistant most likely, for
/// Session to show. `x` from the left, `y` from the top, `rgb` ordinary 0..=255.
#[derive(Clone, Debug, PartialEq)]
pub struct Mood {
    pub x: u8,
    pub y: u8,
    pub name: String,
    pub rgb: [u8; 3],
}

/// What Session shows, shared with whoever sets it and read every frame, so a new
/// list shows at once.
#[derive(Debug, Default)]
pub struct Moods {
    pub list: Vec<Mood>,
    /// A mood pressed somewhere else, the website most likely, by name. The
    /// session takes it next frame and treats it as a press on its pad.
    pub pressed: Option<String>,
}

impl Moods {
    pub const fn new() -> Self {
        Moods {
            list: Vec::new(),
            pressed: None,
        }
    }
}

/// Session's mode, which [`run`] and [`panel`] both have: the moods on the grid.
struct MoodMode<'a> {
    moods: &'a Mutex<Moods>,
    on: bool,
    /// The mood pressed last, and when; it pulses for [`GLOW`] seconds.
    glow: Option<(Pad, f32)>,
    /// What is on the grid, so it is sent again only when the list or glow change.
    drawn: Option<(Vec<Mood>, Option<Pad>)>,
}

impl<'a> MoodMode<'a> {
    fn new(moods: &'a Mutex<Moods>) -> Self {
        MoodMode {
            moods,
            on: false,
            glow: None,
            drawn: None,
        }
    }

    fn show(&mut self) {
        self.on = true;
        self.drawn = None;
    }

    fn lock(&self) -> std::sync::MutexGuard<'a, Moods> {
        self.moods.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The mood under `button`, if any, which starts it glowing.
    fn press(&mut self, button: Pad, now: f32) -> Option<String> {
        let (x, y) = button.to_xy()?;
        let name = self
            .lock()
            .list
            .iter()
            .find(|s| (s.x, s.y) == (x, y))?
            .name
            .clone();
        self.glow = Some((button, now));
        Some(name)
    }

    /// A mood pressed somewhere else, if it is one: it glows like a press on its
    /// pad, and the moods come up to show it.
    fn remote(&mut self, now: f32) -> Option<String> {
        let mut moods = self.lock();
        let name = moods.pressed.take()?;
        let mood = moods.list.iter().find(|s| s.name == name)?;
        let button = Pad::xy(mood.x, mood.y);
        drop(moods);
        self.glow = Some((button, now));
        self.show();
        Some(name)
    }

    fn draw(&mut self, pad: &mut Launchpad, now: f32) -> Result<()> {
        if self.glow.is_some_and(|(_, at)| now - at > GLOW) {
            self.glow = None;
        }
        let glowing = self.glow.map(|(p, _)| p);
        let list = self.lock().list.clone();
        if self
            .drawn
            .as_ref()
            .is_some_and(|(l, g)| *l == list && *g == glowing)
        {
            return Ok(());
        }
        let mut picture: Picture = [[[0; 3]; 8]; 8];
        for mood in &list {
            if let Some(rgb) = picture
                .get_mut(mood.y as usize)
                .and_then(|row| row.get_mut(mood.x as usize))
            {
                *rgb = mood.rgb;
            }
        }
        pad.set_all(grid(&picture).lights().map(|(p, light)| {
            if Some(p) == glowing {
                (p, Light::Pulse(3))
            } else {
                (p, light)
            }
        }))?;
        self.drawn = Some((list, glowing));
        Ok(())
    }
}

/// Runs a session on the device: `start` first, then whatever the picker chooses,
/// with the arrows on speed and brightness and Session on `moods`, until
/// Stop/Solo/Mute is pressed or `stop` is set. `source` is the audio source the
/// sound effects listen to, by any part of its name; `None` is whatever the machine
/// is playing. `report` hears what is showing, each mood pressed and every button
/// event, on the session's own thread.
///
/// Returns normally either way, so the `Launchpad`'s `Drop` gets to clear the
/// LEDs, put the brightness back and leave programmer mode. That is why `stop` is
/// a flag checked once a frame rather than anything more forceful: whatever ends a
/// session must let it end like this, or the device is left lit.
pub fn run(
    start: Effect,
    source: Option<&str>,
    moods: &Mutex<Moods>,
    stop: &AtomicBool,
    mut report: impl FnMut(Report),
) -> Result<()> {
    let (mut pad, events) = Launchpad::connect()?;
    let mut app = App::new(source.map(String::from));
    let mut clock = Clock::new(FPS);
    let mut queue = Vec::new();
    // `None` once a mood is pressed: the mood stops the effect.
    let mut current = Some(EFFECTS.iter().position(|e| *e == start).unwrap_or(0));
    let mut picking = false;
    let mut moods = MoodMode::new(moods);
    // Grid pads pressed in the picker or among the moods, so their releases stay
    // unreported too.
    let mut picked: Vec<Pad> = Vec::new();
    let (mut wall, mut virtual_time) = (0.0f32, 0.0f32);
    // Global: both outlive a change of effect.
    let mut level = NORMAL_SPEED;
    let mut brightness = BRIGHTNESS.len() - 1;

    app.reset(&mut pad)?;
    pad.set_brightness(BRIGHTNESS[brightness])?;
    draw_speed(&mut pad, level)?;
    println!("{}", start.name());
    report(Report::Showing(start));

    'running: loop {
        let now = clock.tick(&events, &mut queue);
        if stop.load(Ordering::Relaxed) {
            break 'running;
        }
        // Effects see a virtual clock, so speed scales motion, decay and spawn
        // rates alike rather than only whatever was written against wall time.
        let dt = (now - wall).max(0.0) * SPEEDS[level];
        wall = now;
        virtual_time += dt;

        if let Some(name) = moods.remote(now) {
            current = None;
            picking = false;
            draw_speed(&mut pad, level)?;
            println!("mood {name}");
            report(Report::Mood(name));
        }
        for event in &queue {
            match *event {
                Event::Press(button, _) if (picking || moods.on) && button.to_xy().is_some() => {
                    picked.push(button)
                }
                Event::Release(button) if picked.contains(&button) => {
                    picked.retain(|&p| p != button)
                }
                _ => report(Report::Event(*event)),
            }
            let Event::Press(button, _) = *event else {
                continue;
            };
            if button == quit_button() {
                break 'running;
            }
            match nudge(button) {
                Some(Nudge::Brightness(by)) => {
                    if let Some(moved) = step(brightness, by, BRIGHTNESS.len()) {
                        brightness = moved;
                        pad.set_brightness(BRIGHTNESS[brightness])?;
                        println!("brightness {}", BRIGHTNESS[brightness]);
                    }
                    continue;
                }
                Some(Nudge::Speed(by)) => {
                    if let Some(moved) = step(level, by, SPEEDS.len()) {
                        level = moved;
                        println!("speed {}x", SPEEDS[level]);
                        if !picking {
                            draw_speed(&mut pad, level)?;
                        }
                    }
                    continue;
                }
                None => {}
            }
            if button == MENU {
                picking = !picking;
                // Going to the effects lets go of the mood.
                moods.glow = None;
                if picking {
                    moods.on = false;
                    draw_picker(&mut pad, current)?;
                } else if current.is_some() {
                    app.reset(&mut pad)?;
                    draw_speed(&mut pad, level)?;
                } else {
                    // Nothing to go back to: the moods are all there is.
                    moods.show();
                    draw_speed(&mut pad, level)?;
                }
                continue;
            }
            if button == SESSION {
                if !moods.on {
                    picking = false;
                    moods.show();
                    draw_speed(&mut pad, level)?;
                } else if current.is_some() {
                    moods.on = false;
                    app.reset(&mut pad)?;
                    draw_speed(&mut pad, level)?;
                }
                continue;
            }
            if moods.on
                && let Some(name) = moods.press(button, now)
            {
                current = None;
                println!("mood {name}");
                report(Report::Mood(name));
                continue;
            }
            if picking && let Some((x, y)) = button.to_xy() {
                let i = (y * 8 + x) as usize;
                if i < EFFECTS.len() {
                    current = Some(i);
                    picking = false;
                    app.reset(&mut pad)?;
                    draw_speed(&mut pad, level)?;
                    println!("{}", EFFECTS[i].name());
                    report(Report::Showing(EFFECTS[i]));
                }
            }
        }

        if moods.on {
            moods.draw(&mut pad, now)?;
        } else if !picking && let Some(i) = current {
            app.render(&mut pad, EFFECTS[i], virtual_time, dt, &queue)?;
        }
    }

    println!("{:.1} fps", clock.fps());
    Ok(())
}

/// A picture for the grid in ordinary 0..=255 RGB, `[y][x]` with the top row first,
/// the way [`Pad::xy`] counts.
pub type Picture = [[[u8; 3]; 8]; 8];

/// Runs a session that shows `picture` and nothing else: the grid as a panel that
/// someone else paints, Home Assistant most likely. Session shows `moods` instead,
/// as in [`run`], and pressing one leaves the picture for good. It redraws when the
/// picture changes, reports every event like [`run`], and quits on Stop/Solo/Mute or
/// `stop`, returning normally for the same reason [`run`] does.
pub fn panel(
    picture: &Mutex<Picture>,
    moods: &Mutex<Moods>,
    stop: &AtomicBool,
    mut report: impl FnMut(Report),
) -> Result<()> {
    let (mut pad, events) = Launchpad::connect()?;
    let mut clock = Clock::new(FPS);
    let mut queue = Vec::new();
    let mut shown = None;
    let mut moods = MoodMode::new(moods);
    // False once a mood is pressed, which stops the panel like it stops an effect.
    let mut painting = true;
    let mut picked: Vec<Pad> = Vec::new();
    pad.set_all([
        (quit_button(), Light::Solid(7)),
        (SESSION, Light::Rgb(30, 30, 30)),
    ])?;

    'running: loop {
        let now = clock.tick(&events, &mut queue);
        if stop.load(Ordering::Relaxed) {
            break 'running;
        }
        if let Some(name) = moods.remote(now) {
            painting = false;
            report(Report::Mood(name));
        }
        for event in &queue {
            match *event {
                Event::Press(button, _) if moods.on && button.to_xy().is_some() => {
                    picked.push(button)
                }
                Event::Release(button) if picked.contains(&button) => {
                    picked.retain(|&p| p != button)
                }
                _ => report(Report::Event(*event)),
            }
            let Event::Press(button, _) = *event else {
                continue;
            };
            if button == quit_button() {
                break 'running;
            }
            if button == SESSION {
                if !moods.on {
                    moods.show();
                } else if painting {
                    moods.on = false;
                    shown = None;
                }
            } else if moods.on
                && let Some(name) = moods.press(button, now)
            {
                painting = false;
                report(Report::Mood(name));
            }
        }
        if moods.on {
            moods.draw(&mut pad, now)?;
            continue;
        }
        let now = *picture
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if shown != Some(now) {
            pad.set_all(grid(&now).lights())?;
            shown = Some(now);
        }
    }
    Ok(())
}

/// 0..=255 RGB as the grid stores it. Ordinary RGB is already gamma-encoded, and
/// `Grid::lights` raises by `GAMMA` on the way out, so a straight scale is the
/// whole conversion: a mid grey from Home Assistant lands as a mid grey.
fn grid(picture: &Picture) -> Grid {
    let mut grid = Grid::new();
    for (y, row) in picture.iter().enumerate() {
        for (x, rgb) in row.iter().enumerate() {
            grid.set(x as i32, y as i32, rgb.map(|c| c as f32 / 255.0));
        }
    }
    grid
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_moods_blueprint_offers_every_effect() {
        // Its effect dropdown is written out by hand: Home Assistant cannot ask.
        let blueprint = include_str!("../../home-assistant/launchpad_moods.yaml");
        for effect in EFFECTS {
            let option = format!("- {}\n", effect.name());
            assert!(blueprint.contains(&option), "missing {:?}", effect.name());
        }
    }

    #[test]
    fn names_are_unique_because_callers_look_effects_up_by_them() {
        for (i, a) in EFFECTS.iter().enumerate() {
            for b in &EFFECTS[i + 1..] {
                assert_ne!(a.name(), b.name());
            }
        }
    }

    #[test]
    fn hues_follow_picker_order() {
        assert_eq!(EFFECTS[0].hue(), 0.0);
        assert!(EFFECTS.windows(2).all(|w| w[0].hue() < w[1].hue()));
    }
}
