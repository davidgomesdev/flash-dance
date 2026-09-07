# flash-dance

A small Rust API for the Novation Launchpad Mini MK3 over USB MIDI: set the button
LEDs, receive the button presses. The lights flash, the pads get danced on.

A cargo workspace: `api/` is the library, `apps/*` are the things built on it.

```
api/          the flash-dance crate — LEDs and button events
apps/paint/   cargo run -p paint — tap pads to toggle them, top-right quits
```

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

Buttons are addressed the way the device does it in programmer mode — index
`row * 10 + column` — with `Pad::xy` / `Pad::top` / `Pad::right` as the sane way in
and `Pad::to_xy` as the way back out. `Light::Solid`, `Flash` and `Pulse` take
palette indices (`0..128`, see Novation's programmer reference for the chart);
`Light::Rgb` takes 7-bit channels. `set_all` batches any number of LEDs into a
single SysEx message, which matters if you are animating.

Requires ALSA on Linux (`libasound2-dev` to build).

## Adding an app

Drop a crate in `apps/`; the workspace picks it up from the `apps/*` glob.

```bash
cargo new --bin apps/strobe
```

Then depend on the API with `flash-dance = { workspace = true }`, and run it with
`cargo run -p strobe`.
