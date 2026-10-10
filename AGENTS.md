# AGENTS.md

Guide for agents working in this repo. Read `README.md` for the user-facing API;
this file covers the parts you cannot infer from the code.

## What this is

A cargo workspace around a Rust library that drives a **Novation Launchpad Mini
MK3** over USB MIDI: set the LEDs under its 8x8 grid, top row and right column,
and receive button presses. One external dependency, `midir`.

```
api/           the `flash-dance` crate: the MIDI driver, plus the drawing
               primitives every app shares (Grid, hsv, Clock, Rng)
audio/         the `flash-dance-audio` crate: sound in, a level, eight bands
               and a beat out. Nothing here knows about Launchpads
web/           a Leptos site that runs the `effects` binary for you
effects/       the `flash-dance-effects` crate: 18 LED effects and `run`, the whole
               session — picker, speed, brightness, quit
apps/effects/  `run` from a terminal; `-- <name>` starts on one
apps/paint/    tap grid pads to toggle them
apps/palette/  browse the device's 128 palette colours and print indices
apps/whack/    whack-a-mole
apps/simon/    Simon, on four 4x4 quadrants
apps/snake/    snake, steered with the device's arrow buttons
apps/life/     Conway's life, drawable and self-reseeding
```

The split exists so the API stays publishable and the apps stay throwaway. `api/`
is the only crate that talks MIDI or knows the SysEx protocol; **an app must never
send raw bytes to the device.** `audio/` depends on neither and knows nothing about
the device — an effect is where a sound becomes a colour, and that is the only
place the two meet. If an app needs something the API cannot express,
add it to the API — that is the signal the API is missing something, not a licence
to reach around it.

Each app is its own crate under `apps/`, picked up by the `apps/*` glob in the
workspace manifest, `publish = false`, depending on `flash-dance = { workspace = true }`.
Add one with `cargo new --bin apps/<name>`. Apps do not depend on each other, and
nothing depends on an app: code two things want lives in a library crate —
`api/`, `audio/` or `effects/` — never in `apps/`.

Inside the API there is deliberately no module tree, no error enum, no trait
abstraction, and **no `Effect` trait** — effects are plain methods on one flat
struct in `effects/`, and a trait with eighteen implementations would buy
nothing. The device has one model and one protocol; if you are asked to support a
Launchpad Pro or MK2, that is the point to generalise — not before.

Effects are driven by a **virtual clock**: `main` scales wall-clock `dt` by the
speed setting and hands effects the result, so one speed control reaches motion,
decay and spawn rates alike. That only works because nothing renders per frame —
a trail is `decay(per_second, dt)`, not a fixed factor, and fire propagates off an
accumulator. Write new effects the same way, or speed will silently skip them.

### Drawing primitives

`Grid` is a frame buffer of **linear** colour that effects write and
`Launchpad::set_all` consumes via `Grid::lights()`. Two things about it matter:

- Coordinates are `i32` and off-grid reads and writes are silently ignored. Effects
  that ripple, sweep and scroll compute out-of-bounds constantly; bounds-checking
  each one at the call site was pure noise.
- Colour is linear `0.0..=1.0` internally and gamma-encoded once on the way out
  (`GAMMA`, 2.2). Fades, `add` and `fade` are arithmetic that only behaves in
  linear light, and a linear ramp sent raw to the LEDs looks like it snaps off
  early. **Do not send `Light::Rgb` computed by hand for anything that fades** —
  go through `Grid`, or reproduce the gamma step yourself.

`Clock::tick` is the frame loop: it spends the gap between frames blocking on the
event channel, so an effect renders on schedule and still sees presses immediately.
No threads, no async. `Clock::fps()` reports what was actually achieved.

`Rng` is a six-line xorshift, in the API rather than in each app because apps share
code only through this crate and `rand` would be a dependency tree in exchange.

## How the device actually works

Everything below is the Launchpad Mini MK3 *programmer's reference guide*, and it
is the reason the code looks the way it does.

### Two USB ports, not one

The device enumerates **two** MIDI in/out port pairs:

```
hw:2,0,0  Launchpad Mini MK3 LPMiniMK3 DA   <- "DAW" port, for Ableton-style control
hw:2,0,1  Launchpad Mini MK3 LPMiniMK3 MI   <- "MIDI" port, the one we want
```

`find_port` in `api/src/lib.rs` filters port names for `LPMiniMK3` and then prefers the
one containing `MI`. Talking to the DAW port instead produces no error and no
lights — a silent failure that is easy to misdiagnose. Check `amidi -l` first.

### Programmer mode and the index scheme

`Launchpad::connect` sends `F0 00 20 29 02 0D 0E 01 F7` to enter programmer mode,
and `... 0E 00 F7` on drop to leave it. Programmer mode is what makes every button
addressable by a single index:

```
index = row * 10 + column       row 1..8 bottom-to-top, column 1..8 left-to-right
91..98   top row (arrows / session buttons)
19,29..89  right column (scene launch), i.e. column 9
99       the logo LED
```

`Pad::xy(x, y)` converts from screen coordinates (x from the left, y from the
**top**) to that index, and `Pad::to_xy` back — returning `None` for the top row,
right column and logo, which is also how the demo distinguishes grid from edge
buttons. In the device's own scheme rows count from the bottom, hence the `8 - y`.

### Output: one SysEx path for everything

The device accepts Note On / CC messages for lighting, but the grid is notes and
the edges are CCs, which would mean two code paths and a lookup to decide which.
Instead everything goes through the SysEx "colour spec" command:

```
F0 00 20 29 02 0D 03 <spec> <spec> ... F7

spec = 00 <index> <colour>              solid, palette index 0..127
       01 <index> <colourB> <colourA>   flashing (device clock)
       02 <index> <colour>              pulsing
       03 <index> <r> <g> <b>           RGB, 7-bit channels
```

That is `Light::spec`. Any number of specs pack into one message, which is what
`set_all` exploits — `set` is just `set_all` with one entry, and `clear` is one
message covering all 81 indices. Sending 81 separate messages visibly tears when
animating; batching does not.

Colours for `Solid`/`Flash`/`Pulse` are **palette indices**, not RGB. The palette
chart is in Novation's programmer reference; useful ones: 0 off, 3 white, 5 red,
21 green, 37 cyan, 45 blue. `cargo run -p palette` browses the rest on the device.

### Brightness is a device setting, and it was found at zero

The device has a global brightness level, `F0 00 20 29 02 0D 08 <0-127> F7`, which
scales every LED. **If effects look dim, check this before touching any code.**
The first time it was read on this hardware it came back `00` — the minimum — which
made everything look washed out for reasons nothing in the source could explain.

It has a readback variant, so you can measure it rather than guess:

```bash
amidi -p hw:2,0,1 -S 'F0 00 20 29 02 0D 08 F7' -d -t 1
#   -> F0002029020D08 <level> F7
```

`connect` reads the level, raises it to 127, and drop puts the old level back;
`effects/` also puts it on the up/down arrows. Restoring matters because the
level **persists on the device** across unplugging — it is the Setup menu's LED
level — so without it an app that dims the device would leave it dim for
everything else, Ableton included.

**The manual documents no default brightness**, only the 0–127 range and the
Setup menu's eight levels, so there is no constant to restore to and the value the
device was on is the only honest answer. If the readback does not arrive within
250 ms, `brightness_was` stays `None` and drop leaves brightness alone rather than
inventing a value.

This is also the one place the input callback looks at something other than button
events: `parse_brightness` intercepts the reply before `Event::parse` sees it. The
device sends confirmations for other commands too (`0Eh` mode changes show up on
the wire), so match on the command byte, never on "is a SysEx".

Brightness and `GAMMA` are different knobs and worth not confusing: brightness
scales everything, gamma changes only how the midtones sit between off and full.

### Throughput: fast enough, and with no backpressure

Measured, not guessed:

| | |
|---|---|
| Device drain rate | **~85 KB/s** (22 KB and 110 KB bursts through rawmidi agreed) |
| Full 64-pad RGB frame | 330 bytes, ~3.8 ms |
| Ceiling for full frames | **~265 fps** — 30 fps uses about 12% of the link |

So the link is not the constraint, but **`set_all` never blocks**: 20 000 frames
(6.6 MB) went out through midir in 0.74 s, which is 9 MB/s and far beyond what the
device can consume. There is no flow control anywhere in the path, so sending
faster than the device drains does not push back — it silently builds latency or
loses frames. Pace with `Clock`; do not treat a returning `set_all` as delivery.

### Input: notes and CCs, velocity 0 means release

Grid pads send Note On (`0x90`), the top row and right column send CC (`0xB0`);
both carry the same programmer-mode index in the first data byte. A **release is
a velocity-0 Note On**, not a Note Off — though `Event::parse` handles `0x80` too,
because some firmware and any MIDI-Through routing will send it. Grid pads are
velocity sensitive; the edge buttons always report 127.

`connect` registers a `midir` input callback that parses and forwards onto an
`mpsc::channel`, and returns the `Receiver`. The `MidiInputConnection` is kept
alive in the `Launchpad` struct as `_input` — dropping it closes the port and the
events stop, so it must not be discarded.

## Sound

`audio/` captures by spawning **`parec`**, not by binding an audio library. That
is a deliberate trade and worth understanding before replacing it: PulseAudio and
PipeWire expose an output's `.monitor` as an ordinary source, so *system sound
costs exactly as much code as a microphone* — one name in one argument. A portable
capture crate (`cpal` and friends) gives you input devices and stops, and would
have been the project's second dependency. `parec` makes it zero: `audio/` has no
dependencies at all, FFT included.

Four things in there were found the hard way:

- **`--latency-msec=25` is load-bearing.** Without it `parec` hands over about a
  second of audio in one go, and since the ring only keeps the newest 2048 samples
  the analysis *freezes* between deliveries — a spectrum that holds one shape for a
  second, then jumps. It looks like a hung thread and is not one.
- **The reader thread must read whole samples.** It uses `read_exact` on an
  even-sized buffer; a short read that split one 16-bit sample would shift every
  byte after it and turn the rest of the stream into noise.
- **`XDG_RUNTIME_DIR` may not be set.** It is how libpulse finds its socket, and
  a process started outside a login session — a systemd unit, a cron job, an
  agent's shell — has none. The failure is `pa_context_connect() failed:
  Connection refused`, which reads like a dead sound server and is not one. The
  crate fills in the spec's own default, `/run/user/<uid>`, and never overrides an
  existing value.
- **`WINDOW` is 2048, not 1024.** The lowest band is 40–80 Hz, and at 1024 samples
  that octave gets two bins, so a 60 Hz note lands in the wrong band. The window
  always ends on the newest sample, so the extra length costs no latency on an
  onset — a kick is just averaged with a little more of what came before it.

**Nothing an effect sees depends on the volume** — only on which frequencies are
there. The bands are divided by the loudest band of the same window, the pitch is
an energy-weighted mean octave, and a beat is the bass against its own running
average: scaling the input scales both sides of each and cancels out. Do not
bring back a running gain or a loudness field; the old `level` (and the vu meter
drawn from it) was removed for exactly this. `volume_changes_nothing` guards it.

The one exception is `SILENCE`, an RMS gate at about -66 dBFS: below it a window
is `Sound::default()`, because otherwise hiss or dither would be scaled up to a
full spectrum like anything else. Raise it if a microphone in a quiet room still
lights the grid; lower it if a quiet source looks dead.

### Testing sound without playing anything aloud

Load a null sink, play into it, record its monitor — nothing reaches the speakers,
and the source name is known:

```bash
pactl load-module module-null-sink sink_name=flashtest
paplay --device=flashtest tone.wav &
cargo run -p flash-dance-audio --example meter -- flashtest.monitor
pactl unload-module module-null-sink        # put it back
```

The `meter` example prints the analysis as text, which is how the bands were
checked against known tones (480 Hz lights band 3, 7680 Hz band 7) and how the
freeze above was spotted. Expect a second or two of silence at the start: a
suspended sink takes that long to resume. Combine it with the MIDI injection below
and the whole sound path is testable with no hands and no speakers.

## The website

`web/` is a Leptos SSR app (axum, built by cargo-leptos) that runs effects
in-process through `flash-dance-effects`. **It renders no effects itself and must
not start to.** The list, the grouping, each card's colour and its `?` text
(`Effect::about`) all come from the library; a second copy here is how the site would start lying about what the
device can do. If the site needs to know something new, the library should say it.

The parts that are not obvious:

- **An effect is `run` on its own thread, stopped by a flag.** `runner::halt` sets
  the session's `AtomicBool` and *joins* — at most a frame. Joining is not
  politeness: returning is what runs the `Launchpad`'s `Drop`, which clears the
  LEDs, restores brightness, leaves programmer mode and releases the MIDI port the
  next session needs. Do not reach for anything that ends the thread less
  gracefully; there is no such thing that also runs `Drop`.
- **One session, ever.** The Launchpad has one MIDI port and a session holds it.
  Starting one stops the last.
- **The page polls, and the grid must sit under a `Transition`.** A session can end
  without the server asking — the device's own Stop/Solo/Mute, or no Launchpad
  plugged in — so the page asks every 1.5 s. Every card reads that status for its
  highlight, and a `Suspense` falls back on *every* reload of what it reads: under
  one, each poll tore down and rebuilt all eighteen cards, which looked like the
  page refreshing itself. A `Transition` only falls back on the first load.
- The session's error is a `String` by the time it reaches the page, because the
  library's `Box<dyn Error>` is not `Send` and cannot leave the thread.
- A `kill -9` of the server leaves the device lit, like any app. Ctrl+C is fine:
  the shutdown hook stops the session first.
- The web crate defaults to the `ssr` feature so an ordinary `cargo build
  --workspace` builds the server and nothing has to know about cargo-leptos, which
  passes the features itself for each of its two builds. The effects library is an
  `ssr`-only dependency — it pulls in MIDI and ALSA, which cannot build for wasm.

### Home Assistant, over MQTT

`web/src/mqtt.rs` is a bridge to Home Assistant, started from `main` when
`FLASH_DANCE_MQTT` is set. It is a client of `runner` like the page is, never of the
device, so the one-session rule and the joining stop hold for it too.

- **Sessions report back.** `effects::run` and `effects::panel` take a
  `report: impl FnMut(Report)` that hears every button event and every change of
  effect, the device's own picker included. That is how the light's state names
  the effect actually showing, and how presses reach HA. It runs on the session's
  thread, so `runner` never takes `RUNNING` inside it — `halt` holds that lock
  while it joins, and the two would deadlock.
- **The panel is a session, not an effect.** `runner::start("panel")` runs
  `effects::panel`, which shows `runner`'s `PICTURE`. HA paints that picture
  whether or not the panel is running, and the panel redraws only when it changes.
  The picture is ordinary 0..255 RGB, divided by 255 into `Grid`; `Grid::lights`
  then does the gamma step, so do not add another.
- **`try_publish`, never `publish`.** The bridge is the loop that drains rumqttc's
  queue; awaiting room in it from that loop would wait forever.
- **State is polled once a second**, not pushed, so a session ended on the device
  or started from the website still reaches HA. `status()` reports a failed session
  once, then idle — the bridge logs the error on that one tick.
- `rumqttc` is built without default features: no TLS, so no rustls tree on the
  Pi. Turn `use-rustls` on if a broker ever needs it.

Testing it needs a broker, not Home Assistant. A throwaway Mosquitto works:

```bash
docker run -d --rm --name fd-mosq -p 18830:1883 -v $PWD/mosq.conf:/mosquitto/config/mosquitto.conf eclipse-mosquitto:2
#   mosq.conf: "listener 1883" and "allow_anonymous true"
docker exec -d fd-mosq sh -c 'mosquitto_sub -v -t "#" > /tmp/all.log'
FLASH_DANCE_MQTT=127.0.0.1:18830 cargo leptos serve
docker exec fd-mosq mosquitto_pub -t flash-dance/light/set -m '{"state":"ON","effect":"fire"}'
docker exec fd-mosq cat /tmp/all.log
```

With no Launchpad plugged in the session fails at once, so the light reports `ON`
then `OFF` and the server logs why — which is still a full check of the path.

### Checking the page in a browser

Chrome drives headless over the DevTools protocol with nothing but Python's
`websockets`. That is how the flicker above was found and its fix proved: a
`MutationObserver` counting removed cards went 1, 2, 3, 4 across four polls before,
and stayed at 0 after — including while a session started, failed and stopped
underneath it. Count DOM teardowns rather than looking for the fallback text; most
last under the 200 ms a sampler would take to see them.

This is the one crate exempt from the "one dependency" rule below; a web framework
was the ask. Nothing it pulls in may leak into `api/`, `audio/`, `effects/` or
`apps/`.

## Working on this

```bash
cargo test --workspace   # 24 unit tests: pads, colour, grid, rng, clock, SysEx,
                         # parsing, FFT, bands, gain, effect names, MQTT payloads
cargo leptos serve       # the website, on :3000
cargo clippy --workspace --all-targets
cargo fmt
cargo run -p effects     # any app; needs the device plugged in
cargo run -p effects -- fire   # start on one effect, no hands needed
```

Build needs ALSA headers on Linux (`libasound2-dev`).

**The unit tests do not touch hardware.** They cover the pure functions — index
arithmetic, colour, byte encoding, message parsing — which is where most bugs live.
Do not claim LED behaviour is verified because `cargo test` passed.

### Testing input without hands on the device

You can deliver a real button press to a running app, which makes the whole input
path testable from a terminal. Start the app, find the ALSA client that `midir`
opened for it, and play a MIDI file at it:

```bash
./target/debug/palette &
PORT=$(aconnect -l | grep -i launchpad-in | sed -E 's/client ([0-9]+):.*/\1/')
aplaymidi -p "$PORT:0" tap.mid    # e.g. 90 51 64 = press grid pad (0,0)
```

Note the port is a *destination*, so it shows up in `aconnect -l` and `aconnect -o`
but **not** in `aconnect -i`. Pick an app that prints what it received — `palette`
prints the index of every pad tapped — and you get an assertable result. This is
how the grid-press and quit-button paths were verified, along with `effects`
reporting 29.9 fps against a requested 30.

What this still cannot check is **what the device looks like**: colour, brightness,
gamma, whether an animation reads as fire or as noise. That needs the hardware and
a person watching it. Say so plainly rather than implying the visuals are verified.

### Gotcha: killing the process leaves the device lit

`Drop for Launchpad` clears the LEDs and returns the device to standalone mode.
Ctrl+C, `timeout`, or a panic skips it, and the Launchpad stays in programmer mode
with whatever was on it still on. Reset it by hand:

```bash
amidi -l                                            # find the ",1" port
amidi -p hw:2,0,1 -S 'F0 00 20 29 02 0D 0E 00 F7'   # back to standalone mode
```

Every app has a quit button — top-left, except `effects` and `snake`, which use the
arrows for their own controls and so quit with Stop/Solo/Mute (`Pad::right(7)`).
Prefer it over Ctrl+C.

## Repo conventions

- The user's global `CLAUDE.md` applies: work in a git worktree on a
  `<type>/<short-description>` branch, never commit to `main`, never `--no-verify`
  or skip GPG signing, conventional-commit messages, **no agent attribution of any
  kind** in commits, branches, PRs or issues.
- `origin` is `davidgomesdev/flash-dance` (private). Push the branch and open a PR
  with `gh pr create`; leave the merge to a human.
- Keep it small. `midir` is still the only external dependency, and the bar for a
  second one is that the existing code genuinely cannot do the job.
