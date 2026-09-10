//! Seventeen LED effects with a picker.
//!
//! Top-right button opens the picker (tap a lit pad to choose), top-left quits.
//! The first three effects animate on the device's own clock and cost one message;
//! the rest render frames here at [`FPS`].

use flash_dance::{Clock, Event, Grid, Launchpad, Light, Pad, Result, Rng, hsv};

const QUIT: Pad = Pad(91); // top row, leftmost
const MENU: Pad = Pad(98); // top row, rightmost
const FPS: f64 = 30.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Effect {
    // Device-side: the Launchpad animates these itself.
    PulseWash,
    Checkerboard,
    BreathingBorder,
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
}

use Effect::*;

const EFFECTS: [Effect; 17] = [
    PulseWash,
    Checkerboard,
    BreathingBorder,
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
];

impl Effect {
    fn name(self) -> &'static str {
        match self {
            PulseWash => "pulse wash (device)",
            Checkerboard => "checkerboard flash (device)",
            BreathingBorder => "breathing border (device)",
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
        }
    }

    /// Signature colour, used for this effect's pad in the picker.
    fn hue(self) -> f32 {
        EFFECTS.iter().position(|e| *e == self).unwrap_or(0) as f32 / EFFECTS.len() as f32
    }
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
    drops: Vec<Drop>,
    ripples: Vec<Ripple>,
    falling: Vec<Fall>,
    stack: [i32; 8],
}

impl App {
    fn new() -> App {
        App {
            grid: Grid::new(),
            rng: Rng::new(),
            scratch: [0.0; 64],
            drops: Vec::new(),
            ripples: Vec::new(),
            falling: Vec::new(),
            stack: [0; 8],
        }
    }

    /// Selects an effect: wipes the previous one's state and, for a device-side
    /// effect, sends the one message that is all it needs.
    fn enter(&mut self, pad: &mut Launchpad, effect: Effect) -> Result<()> {
        self.grid.clear();
        self.scratch = [0.0; 64];
        self.drops.clear();
        self.ripples.clear();
        self.falling.clear();
        self.stack = [0; 8];
        pad.clear()?;

        match effect {
            PulseWash => pad.set_all(all_pads().map(|p| (p, Light::Pulse(21)))),
            Checkerboard => pad.set_all(grid_pads().map(|(p, x, y)| {
                let (a, b) = if (x + y) % 2 == 0 { (5, 45) } else { (45, 5) };
                (p, Light::Flash { on: a, off: b })
            })),
            BreathingBorder => pad.set_all(
                (0..8)
                    .flat_map(|i| [Pad::top(i), Pad::right(i)])
                    .map(|p| (p, Light::Pulse(3))),
            ),
            _ => Ok(()),
        }
    }

    fn render(
        &mut self,
        pad: &mut Launchpad,
        effect: Effect,
        t: f32,
        dt: f32,
        events: &[Event],
    ) -> Result<()> {
        match effect {
            PulseWash | Checkerboard | BreathingBorder => return Ok(()), // the device is doing it
            Rainbow => self.rainbow(t),
            Plasma => self.plasma(t),
            Fire => self.fire(),
            Rain => self.rain(dt),
            Rings => self.rings(t),
            Radar => self.radar(t),
            Starfield => self.starfield(),
            Wipe => self.wipe(t),
            PressRipple => self.press_ripple(t, events),
            Comet => self.comet(events),
            VelocityHeat => self.velocity_heat(events),
            Lasers => self.lasers(events),
            Gravity => self.gravity(dt, events),
            PressureMap => self.pressure_map(events),
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

    fn fire(&mut self) {
        // Seed the bottom row, then let each row above take from the one below and
        // cool. Ascending order reads the previous frame's row, so flames rise one
        // row per frame; cooling decides how far up they get before dying.
        for x in 0..8 {
            self.scratch[(7 * 8 + x) as usize] = 0.6 + self.rng.f32() * 0.4;
        }
        for y in 0..7 {
            for x in 0..8 {
                let drift = self.rng.below(3) as i32 - 1;
                let below = ((x + drift).clamp(0, 7) + (y + 1) * 8) as usize;
                let cooled = self.scratch[below] - self.rng.f32() * 0.28;
                self.scratch[(y * 8 + x) as usize] = cooled.max(0.0);
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
        self.grid.fade(0.62);
        if self.rng.chance(0.35) {
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

    fn radar(&mut self, t: f32) {
        self.grid.fade(0.82);
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

    fn starfield(&mut self) {
        self.grid.fade(0.9);
        if self.rng.chance(0.4) {
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

    fn comet(&mut self, events: &[Event]) {
        self.grid.fade(0.88);
        for (x, y, _) in presses(events) {
            self.grid.set(x, y, hsv(x as f32 / 8.0, 1.0, 1.0));
        }
    }

    fn velocity_heat(&mut self, events: &[Event]) {
        self.grid.fade(0.97);
        for (x, y, velocity) in presses(events) {
            // Soft is blue, hard is red — the byte `paint` throws away.
            let hue = 0.66 * (1.0 - velocity as f32 / 127.0);
            self.grid.set(x, y, hsv(hue, 1.0, 1.0));
        }
    }

    fn lasers(&mut self, events: &[Event]) {
        self.grid.fade(0.8);
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
}

/// Every grid coordinate, x from the left and y from the top.
fn cells() -> impl Iterator<Item = (i32, i32)> {
    (0..8).flat_map(|y| (0..8).map(move |x| (x, y)))
}

fn grid_pads() -> impl Iterator<Item = (Pad, i32, i32)> {
    cells().map(|(x, y)| (Pad::xy(x as u8, y as u8), x, y))
}

fn all_pads() -> impl Iterator<Item = Pad> {
    grid_pads()
        .map(|(p, _, _)| p)
        .chain((0..8).flat_map(|i| [Pad::top(i), Pad::right(i)]))
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

/// One pad per effect in its signature colour, the current one pulsing white.
fn draw_picker(pad: &mut Launchpad, current: usize) -> Result<()> {
    pad.clear()?;
    pad.set_all(EFFECTS.iter().enumerate().map(|(i, effect)| {
        let target = Pad::xy((i % 8) as u8, (i / 8) as u8);
        if i == current {
            (target, Light::Pulse(3))
        } else {
            let [r, g, b] = hsv(effect.hue(), 1.0, 1.0).map(|c| (c * 127.0) as u8);
            (target, Light::Rgb(r, g, b))
        }
    }))
}

/// `cargo run -p effects -- fire` starts on the first effect whose name matches,
/// which is also how you exercise one without a hand on the device.
fn starting_effect() -> usize {
    let Some(query) = std::env::args().nth(1) else {
        return 0;
    };
    match EFFECTS.iter().position(|e| e.name().contains(&query)) {
        Some(i) => i,
        None => {
            eprintln!("no effect matches {query:?}. try one of:");
            for effect in EFFECTS {
                eprintln!("  {}", effect.name());
            }
            std::process::exit(2);
        }
    }
}

fn main() -> Result<()> {
    let (mut pad, events) = Launchpad::connect()?;
    let mut app = App::new();
    let mut clock = Clock::new(FPS);
    let mut queue = Vec::new();
    let (mut current, mut picking, mut last) = (starting_effect(), false, 0.0f32);

    app.enter(&mut pad, EFFECTS[current])?;
    println!("{}", EFFECTS[current].name());
    println!("(top-right button picks an effect, top-left quits)");

    'running: loop {
        let t = clock.tick(&events, &mut queue);
        let dt = (t - last).max(0.0);
        last = t;

        for event in &queue {
            match *event {
                Event::Press(QUIT, _) => break 'running,
                Event::Press(MENU, _) => {
                    picking = !picking;
                    if picking {
                        draw_picker(&mut pad, current)?;
                    } else {
                        app.enter(&mut pad, EFFECTS[current])?;
                    }
                }
                Event::Press(chosen, _) if picking => {
                    if let Some((x, y)) = chosen.to_xy() {
                        let i = (y * 8 + x) as usize;
                        if i < EFFECTS.len() {
                            current = i;
                            picking = false;
                            app.enter(&mut pad, EFFECTS[current])?;
                            println!("{}", EFFECTS[current].name());
                        }
                    }
                }
                _ => {}
            }
        }

        if !picking {
            app.render(&mut pad, EFFECTS[current], t, dt, &queue)?;
        }
    }

    println!("{:.1} fps", clock.fps());
    Ok(())
}
