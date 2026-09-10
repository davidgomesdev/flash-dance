# flash-dance

A small Rust API for the Novation Launchpad Mini MK3 over USB MIDI: set the button
LEDs, receive the button presses. The lights flash, the pads get danced on.

A cargo workspace: `api/` is the Launchpad library, `audio/` listens to whatever
the machine is playing or hearing, `apps/*` are the things built on them.
Most apps quit with the top-left button; `effects` and `snake` use the arrows for
their own controls, so those two quit with **Stop/Solo/Mute** (bottom of the right
column) instead.

| | |
|---|---|
| `cargo run -p effects` | 18 LED effects with a picker and a speed control |
| `cargo run -p paint` | tap pads to toggle them |
| `cargo run -p palette` | browse the device's 128 palette colours, print indices |
| `cargo run -p whack` | whack-a-mole |
| `cargo run -p simon` | Simon, played on four 4x4 quadrants |
| `cargo run -p snake` | snake on a torus, steered with the arrow buttons |
| `cargo run -p life` | Conway's life: tap to draw, right column runs, reseeds, clears |

### The effects

**Ambient:** rainbow diagonal, plasma, fire, rain, ripple rings, radar sweep,
starfield, colour wipe.

**Reactive:** ripple on press, comet trail, velocity heat (colour by how hard you
hit), lasers, gravity, pressure map.

**Sound:** spectrum, vu meter, beat rings, sound fire — see [Listening](#listening).

Controls: **User** (top row, rightmost) opens the picker, **up/down** change
brightness, **left/right** change speed, and **Stop/Solo/Mute** quits. The right
column shows the speed — blue below 1x, green at 1x, red above. Brightness needs no
indicator, since the whole grid is one.

Both are global: they carry across a change of effect. Brightness is a setting on
the device rather than in the app, but quitting restores whatever level the device
was on before the app started.

Effects run on a virtual clock, so speed scales everything they do: motion, decay
trails and spawn rates alike. Speed does nothing to the ones with no time in them
— `pressure map`, `spectrum` and `vu meter`, which are only ever a picture of
their input.

`cargo run -p effects -- fire` starts on a named effect instead of the first.

## Listening

The four sound effects analyse whatever the machine is playing, and need nothing
set up: `flash-dance-audio` records a PulseAudio/PipeWire source and hands back a
loudness, eight octave bands and a beat flag.

```rust
let mut audio = Audio::open(None)?;   // the first monitor source: system sound
let sound = audio.listen();           // call once a frame
sound.level;   // 0..1, normalised by a running gain, so any source fills the grid
sound.bands;   // 8 of them, 40 Hz to 10 kHz, an octave each
sound.beat;    // set on the one window a beat was found on
```

`Audio::open(Some("webcam"))` takes any part of a source name instead — a
microphone, a line input, another machine's monitor. `sources()` lists them, and
the effects app reads `FLASH_DANCE_SOURCE`:

```bash
FLASH_DANCE_SOURCE=webcam cargo run -p effects -- spectrum
cargo run -p flash-dance-audio --example meter    # the analysis as text
```

Capture is a `parec` child process rather than an audio-library binding, which is
what makes system sound as easy as a microphone — a `.monitor` source is just
another name. It needs `pulseaudio-utils` installed, and it is the reason the
sound half is a separate crate: an app that only lights pads never touches it.

## Usage

```rust
use flash_dance::{Event, Launchpad, Light, Pad};

let (mut pad, events) = Launchpad::connect()?;   // enters programmer mode

pad.set(Pad::xy(0, 0), Light::Rgb(127, 0, 40))?; // top-left of the grid
pad.set(Pad::top(2), Light::Pulse(21))?;         // third button of the top row
pad.set_all((0..8).map(|i| (Pad::right(i), Light::Flash { on: 5, off: 0 })))?;

for event in events {
    match event {
        Event::Press(p, velocity) => println!("{:?} pressed at {velocity}", p.to_xy()),
        Event::Release(p) => pad.set(p, Light::Off)?,
    }
}
```

`Launchpad::connect` returns the device plus a `Receiver<Event>` fed from the MIDI
input callback, so events can be polled (`try_recv`) or blocked on. Dropping the
`Launchpad` clears the LEDs and puts the device back in its standalone mode.

It also turns the device's global brightness up to full, having first asked the
device what its brightness was, so dropping the `Launchpad` can put that back. The
manual documents no default value to restore instead — only that the Setup menu
offers eight levels — so the pre-run level is the only honest thing to return to.
`set_brightness` is the knob while you are running.

Buttons are addressed the way the device does it in programmer mode — index
`row * 10 + column` — with `Pad::xy` / `Pad::top` / `Pad::right` as the sane way in
and `Pad::to_xy` as the way back out. `Light::Solid`, `Flash` and `Pulse` take
palette indices (`0..128`, see Novation's programmer reference for the chart);
`Light::Rgb` takes 7-bit channels. `set_all` batches any number of LEDs into a
single SysEx message, which matters if you are animating.

## Animating

`Grid` is a frame buffer of linear-light colour; `Clock` paces frames while still
delivering presses the instant they arrive. Together they are the whole animation
loop — no threads, no async:

```rust
let (mut pad, events) = Launchpad::connect()?;
let (mut grid, mut clock, mut queue) = (Grid::new(), Clock::new(30.0), Vec::new());

loop {
    let t = clock.tick(&events, &mut queue);   // blocks on events until the frame is due
    for (x, y) in (0..8).flat_map(|y| (0..8).map(move |x| (x, y))) {
        grid.set(x, y, hsv((x + y) as f32 / 14.0 + t * 0.2, 1.0, 1.0));
    }
    pad.set_all(grid.lights())?;               // one SysEx message
}
```

Grid colour is linear `0.0..=1.0` per channel and gets gamma-corrected (`GAMMA`)
on the way out, because the LEDs are perceptually non-linear — send a linear ramp
raw and every fade looks like it snaps off early. Writes outside the grid are
ignored, so effects that ripple and scroll off the edges need no bounds checks.

The device drains about 85 KB/s, so a full 64-pad RGB frame (330 bytes) costs
~3.8 ms and the ceiling is roughly 265 fps. Nothing in the path applies
backpressure, though — `set_all` returns long before the device has caught up — so
pace with `Clock` rather than sending as fast as the calls return.

Requires ALSA on Linux (`libasound2-dev` to build).

## Adding an app

Drop a crate in `apps/`; the workspace picks it up from the `apps/*` glob.

```bash
cargo new --bin apps/strobe
```

Then depend on the API with `flash-dance = { workspace = true }`, and run it with
`cargo run -p strobe`.
