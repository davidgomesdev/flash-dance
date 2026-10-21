# flash-dance

A small Rust API for the Novation Launchpad Mini MK3 over USB MIDI: set the button
LEDs, receive the button presses. The lights flash, the pads get danced on.

A cargo workspace: `api/` is the Launchpad library, `audio/` listens to whatever
the machine is playing or hearing, `apps/*` are the things built on them, and
`web/` is a website that runs them ([below](#the-website)).
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

**Sound:** spectrum, pitch meter, beat rings, sound fire — see [Listening](#listening).

Controls: **User** (top row, rightmost) opens the picker, **up/down** change
brightness, **left/right** change speed, and **Stop/Solo/Mute** quits. The right
column shows the speed — blue below 1x, green at 1x, red above. Brightness needs no
indicator, since the whole grid is one.

Both are global: they carry across a change of effect. Brightness is a setting on
the device rather than in the app, but quitting restores whatever level the device
was on before the app started.

Effects run on a virtual clock, so speed scales everything they do: motion, decay
trails and spawn rates alike. Speed does nothing to the ones with no time in them
— `pressure map`, `spectrum` and `pitch meter`, which are only ever a picture of
their input.

`cargo run -p effects -- fire` starts on a named effect instead of the first.

The effects themselves are a library, `flash-dance-effects`: `run(effect, source,
stop)` is the whole session — picker, arrows and quit button included — and the
`effects` binary is a few lines of argument parsing around it.

## The website

```bash
cargo install cargo-leptos          # once
rustup target add wasm32-unknown-unknown
cargo leptos serve                  # http://127.0.0.1:3000
```

A page of cards, one per effect, in the same colour that effect's pad has in the
device's own picker. Click one and it runs; click another and it switches; the bar
at the top says what is going and stops it. The `?` on a card says what that
effect looks like and what it responds to.

The server runs effects in-process through `flash-dance-effects` — the list, the
grouping, the colours and the descriptions come from the same library, and an effect started here
is the very session the `effects` binary runs, on a thread of its own. Only one
runs at a time, because the Launchpad has one MIDI port.

The one setting on the page is which audio source the sound effects listen to.
Speed and brightness are deliberately missing: they belong on the device's arrow
buttons, where your hands already are.

Stopping — the button, switching effects, or Ctrl+C on the server — sets the
session's stop flag and waits for it to return, so its `Drop` clears the LEDs and
leaves programmer mode. A `kill -9` of the server skips that and leaves the device
lit, the same as for any app.

## Home Assistant

Point the website's server at an MQTT broker and the Launchpad shows up in Home
Assistant by itself, through MQTT discovery — no YAML for the device:

```bash
FLASH_DANCE_MQTT=homeassistant.local FLASH_DANCE_MQTT_USER=launchpad \
FLASH_DANCE_MQTT_PASSWORD=... cargo leptos serve
```

`FLASH_DANCE_MQTT` is `host` or `host:port` (1883 by default); unset, the bridge is
off. Home Assistant gets one device, **Launchpad**, with:

- a **light**: on starts an effect from its effect list, off stops the session.
  One extra effect, `panel`, turns the grid into a panel Home Assistant paints;
- **Audio source**, a select for what the sound effects listen to;
- **Pad**, an event entity (`event.launchpad_pad`) firing `press` and `release`
  with the pad's `x`, `y` (from the top left) and the press velocity.
- **Mood**, an event entity (`event.launchpad_mood`) whose event type is the
  name of the mood pressed — there once Home Assistant has listed moods.

To paint the panel, publish `{"pad": [x, y], "rgb": [r, g, b]}`, or a list of
them, to `flash-dance/grid/set` — from an automation, with `mqtt.publish`. The
picture is kept while an effect runs, so switching to `panel` shows it at once.
The bridge publishes `online` on `flash-dance/availability` each time it
connects; trigger on that to repaint everything after a restart.

### Moods

A mood is a pad that tells Home Assistant to do something: play a playlist, set
the lights, start an effect. **Session** (top row, fifth button) swaps the grid for
the mood pads; **User** still opens the effect picker. The website has the same
two, as **Moods** and **Effects**, and pressing a mood there is pressing it on the
device (starting a session first if none is running). Pressing a mood stops the
effect — the light stays on with no effect — pulses its pad for 5 s and fires
`event.launchpad_mood` with the mood's name as the event type.

**The easy way:** the [Launchpad moods](home-assistant/launchpad_moods.yaml)
blueprint. Copy it to `config/blueprints/automation/flash-dance/`, reload
automations, and make an automation from it. It takes:

- **Scenes and scripts**, Home Assistant's own, picked from a list: each gets a pad, from the top left,
  named after the entity. Pressed, it runs.
- **Playlists**, one per pad, each running one **mood script** with its playlist
  and effect. `row` (1 is the top) puts a playlist on that row, after what is on it
  already; without one it goes after the pad before it. `rgb` (0–255 each) picks
  its colour:

  ```yaml
  - {name: Chill, playlist: "spotify:playlist:37i9dQZF1DX4WYpdgoIcn6", effect: plasma, row: 3, rgb: [0, 120, 255]}
  - {name: Jazz, playlist: "spotify:playlist:37i9dQZF1DXbITWG1ZJKYt"}
  - {name: Rock, playlist: "spotify:playlist:37i9dQZF1DWXRqgorJj26U", effect: spectrum, row: 4}
  ```

A playlist without an `rgb` is white; scenes and scripts are coloured in a rainbow, in order. A scene sets how things look; a script
runs steps, which is what playing music needs. The mood script, with the
[Spotify](https://www.home-assistant.io/integrations/spotify/) integration
(Premium only, and the speaker must already be one Spotify knows):

```yaml
alias: Launchpad mood
fields:
  playlist:
    required: true
    selector: {text: {}}
  effect:
    selector: {text: {}}
sequence:
  - action: media_player.select_source
    target: {entity_id: media_player.spotify}
    data: {source: Living room}
  - action: media_player.play_media
    target: {entity_id: media_player.spotify}
    data:
      media_content_id: "{{ playlist }}"
      media_content_type: playlist
  - if: "{{ effect | default('') != '' }}"
    then:
      - action: light.turn_on
        target: {entity_id: light.launchpad}
        data: {effect: "{{ effect }}"}
```

**By hand:** publish the list, **retained**, to `flash-dance/moods/set` (an empty
list removes them), and trigger on the event entity. Trigger on its state, not on
its `event_type` attribute: pressing the same mood twice changes only the state.

```json
[{"pad": [0, 0], "name": "Chill", "rgb": [255, 0, 128]}]
```

```yaml
triggers:
  - trigger: state
    entity_id: event.launchpad_mood
conditions:
  - condition: state
    entity_id: event.launchpad_mood
    attribute: event_type
    state: Chill
actions:
  - action: script.launchpad_mood
    data: {playlist: "spotify:playlist:37i9dQZF1DX4WYpdgoIcn6"}
```

## Listening

The four sound effects analyse whatever the machine is playing, and need nothing
set up: `flash-dance-audio` records a PulseAudio/PipeWire source and hands back
eight octave bands, a pitch and a beat flag. None of them depends on the volume:
the same song gives the same picture whether it is playing loud or quiet.

```rust
let mut audio = Audio::open(None)?;   // the first monitor source: system sound
let sound = audio.listen();           // call once a frame
sound.bands;   // 8 of them, 40 Hz to 10 kHz, an octave each, relative to the loudest
sound.pitch;   // 0..1, where the sound sits between bass and treble
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

On macOS it is an `ffmpeg` (AVFoundation) child instead — `brew install ffmpeg`.
macOS has no monitor sources, so system sound needs a loopback device such as
BlackHole; the terminal also needs microphone permission.

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
