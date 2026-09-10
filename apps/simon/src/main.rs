//! Simon. Watch the sequence, play it back, watch it get one longer.
//!
//! The grid is four 4x4 quadrants rather than 64 separate pads — a sequence of
//! single pads on an 8x8 is unplayable past about three steps. Top-left quits.

use flash_dance::{Clock, Event, Grid, Launchpad, Pad, Result, Rgb, Rng, hsv};

const QUIT: Pad = Pad(91);
const FPS: f64 = 30.0;
const LIT: f32 = 0.45; // seconds a step stays lit while showing
const DARK: f32 = 0.15; // gap between steps, so a repeat reads as two flashes

const HUES: [f32; 4] = [0.0, 0.33, 0.58, 0.13];

fn quadrant_of(x: u8, y: u8) -> usize {
    (y as usize / 4) * 2 + x as usize / 4
}

fn light_quadrant(grid: &mut Grid, quadrant: usize, brightness: f32) {
    let (ox, oy) = ((quadrant % 2) as i32 * 4, (quadrant / 2) as i32 * 4);
    for y in 0..4 {
        for x in 0..4 {
            grid.set(ox + x, oy + y, hsv(HUES[quadrant], 1.0, brightness));
        }
    }
}

enum Phase {
    /// Playing the sequence back to the player.
    Show {
        step: usize,
        since: f32,
    },
    /// Waiting for the player, `at` steps in.
    Input {
        at: usize,
        flash: Option<usize>,
    },
    Over {
        since: f32,
    },
}

fn main() -> Result<()> {
    let (mut pad, events) = Launchpad::connect()?;
    let mut clock = Clock::new(FPS);
    let mut queue = Vec::new();
    let mut grid = Grid::new();
    let mut rng = Rng::new();

    let mut sequence = vec![rng.below(4) as usize];
    let mut phase = Phase::Show {
        step: 0,
        since: 0.0,
    };
    println!("Watch, then repeat. Top-left quits.");

    'running: loop {
        let t = clock.tick(&events, &mut queue);

        for event in &queue {
            let (Event::Press(button, _) | Event::Release(button)) = *event;
            if matches!(event, Event::Press(_, _)) && button == QUIT {
                break 'running;
            }
            let Phase::Input { at, flash } = &mut phase else {
                continue; // presses during playback are ignored, not buffered
            };
            match (event, button.to_xy()) {
                (Event::Press(_, _), Some((x, y))) => {
                    let played = quadrant_of(x, y);
                    *flash = Some(played);
                    if played != sequence[*at] {
                        phase = Phase::Over { since: t };
                        break;
                    }
                    *at += 1;
                    if *at == sequence.len() {
                        sequence.push(rng.below(4) as usize);
                        println!("round {}", sequence.len());
                        phase = Phase::Show {
                            step: 0,
                            since: t + 0.35, // a beat before the next round starts
                        };
                        break;
                    }
                }
                (Event::Release(_), _) => *flash = None,
                _ => {}
            }
        }

        grid.clear();
        match &mut phase {
            Phase::Show { step, since } => {
                let age = t - *since;
                if age >= 0.0 {
                    let within = age % (LIT + DARK);
                    let index = (age / (LIT + DARK)) as usize;
                    if index >= sequence.len() {
                        phase = Phase::Input { at: 0, flash: None };
                    } else {
                        *step = index;
                        if within < LIT {
                            light_quadrant(&mut grid, sequence[index], 1.0);
                        }
                    }
                }
            }
            Phase::Input { flash, .. } => {
                for q in 0..4 {
                    let brightness = if *flash == Some(q) { 1.0 } else { 0.06 };
                    light_quadrant(&mut grid, q, brightness);
                }
            }
            Phase::Over { since } => {
                if t - *since > 1.2 {
                    break 'running;
                }
                let flash: Rgb = if (((t - *since) * 8.0) as u32).is_multiple_of(2) {
                    [1.0, 0.0, 0.0]
                } else {
                    [0.0; 3]
                };
                grid.fill(flash);
            }
        }
        pad.set_all(grid.lights())?;
    }

    println!("reached round {} ({:.1} fps)", sequence.len(), clock.fps());
    Ok(())
}
