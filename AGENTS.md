# AGENTS.md

Guide for agents working in this repo. Read `README.md` for the user-facing API;
this file covers the parts you cannot infer from the code.

## What this is

A Rust library (`launchpad_mini`) that drives a **Novation Launchpad Mini MK3**
over USB MIDI: set the LEDs under its 8x8 grid, top row and right column, and
receive button presses. One dependency, `midir`. Two source files:

- `src/lib.rs` — the whole library, ~200 lines: `Pad`, `Light`, `Event`, `Launchpad`.
- `src/main.rs` — a demo binary (`cargo run`): tap grid pads to toggle them, hold
  a side button to light it, top-right button quits.

There is deliberately no module tree, no error enum, no trait abstraction. The
device has one model and one protocol; adding layers for a second one that does
not exist is the wrong move. If you are asked to support a Launchpad Pro or MK2,
that is the point to generalise — not before.

## How the device actually works

Everything below is the Launchpad Mini MK3 *programmer's reference guide*, and it
is the reason the code looks the way it does.

### Two USB ports, not one

The device enumerates **two** MIDI in/out port pairs:

```
hw:2,0,0  Launchpad Mini MK3 LPMiniMK3 DA   <- "DAW" port, for Ableton-style control
hw:2,0,1  Launchpad Mini MK3 LPMiniMK3 MI   <- "MIDI" port, the one we want
```

`find_port` in `src/lib.rs` filters port names for `LPMiniMK3` and then prefers the
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
21 green, 37 cyan, 45 blue.

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

## Working on this

```bash
cargo test      # 3 unit tests: pad<->xy mapping, SysEx encoding, MIDI parsing
cargo clippy --all-targets
cargo fmt
cargo run       # the demo, needs the device plugged in
```

Build needs ALSA headers on Linux (`libasound2-dev`).

**The tests do not touch hardware, and they cannot.** They cover the pure
functions — index arithmetic, byte encoding, message parsing — which is where the
bugs actually live. Anything past that (does the right LED light, does a press
arrive) needs the physical device and a human looking at it. Do not claim LED or
button behaviour is verified because `cargo test` passed; run `cargo run`, and say
plainly that a person has to confirm what they saw.

### Gotcha: killing the process leaves the device lit

`Drop for Launchpad` clears the LEDs and returns the device to standalone mode.
Ctrl+C, `timeout`, or a panic skips it, and the Launchpad stays in programmer mode
with whatever was on it still on. Reset it by hand:

```bash
amidi -l                                            # find the ",1" port
amidi -p hw:2,0,1 -S 'F0 00 20 29 02 0D 0E 00 F7'   # back to standalone mode
```

Prefer a quit button (as the demo does) over Ctrl+C when running interactively.

## Repo conventions

- The user's global `CLAUDE.md` applies: work in a git worktree on a
  `<type>/<short-description>` branch, never commit to `main`, never `--no-verify`
  or skip GPG signing, conventional-commit messages, **no agent attribution of any
  kind** in commits, branches, PRs or issues.
- There is no git remote configured, so there is nothing to push and no PR to open
  unless one gets added.
- Keep it small. The bar for a new dependency, file, or abstraction here is that
  the existing ~200 lines genuinely cannot do the job.
