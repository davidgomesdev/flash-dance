//! Conway's Game of Life, both as an ambient effect and as something to draw in.
//!
//! Tap pads to toggle cells. The right column's top three buttons are run/pause,
//! reseed and clear; top-left quits.
//!
//! The grid is a torus and the board reseeds itself when it stops changing. On 8x8
//! both are necessary rather than decorative: with walls almost every seed dies in
//! a few generations, and what survives settles into still-lifes or 2-cycles fast
//! enough that an unattended board is a static picture within about ten seconds.

use flash_dance::{Clock, Event, Grid, Launchpad, Light, Pad, Result, Rng, hsv};

const QUIT: Pad = Pad(91);
const FPS: f64 = 30.0;
const STEP: f32 = 0.18; // seconds per generation
const STAGNANT_AFTER: u32 = 6; // generations of no change before reseeding

type Board = [bool; 64];

fn neighbours(board: &Board, x: i32, y: i32) -> usize {
    let mut alive = 0;
    for dy in -1..=1 {
        for dx in -1..=1 {
            if (dx, dy) == (0, 0) {
                continue;
            }
            let (nx, ny) = ((x + dx).rem_euclid(8), (y + dy).rem_euclid(8));
            alive += board[(ny * 8 + nx) as usize] as usize;
        }
    }
    alive
}

fn step(board: &Board) -> Board {
    let mut next = [false; 64];
    for y in 0..8 {
        for x in 0..8 {
            let i = (y * 8 + x) as usize;
            next[i] = matches!(
                (board[i], neighbours(board, x, y)),
                (true, 2 | 3) | (false, 3)
            );
        }
    }
    next
}

fn seed(rng: &mut Rng) -> Board {
    let mut board = [false; 64];
    for cell in &mut board {
        *cell = rng.chance(0.35);
    }
    board
}

fn main() -> Result<()> {
    let (mut pad, events) = Launchpad::connect()?;
    let mut clock = Clock::new(FPS);
    let mut queue = Vec::new();
    let mut grid = Grid::new();
    let mut rng = Rng::new();

    let mut board = seed(&mut rng);
    let mut age = [0u32; 64]; // generations each cell has been alive, for colour
    let mut history: Vec<Board> = Vec::new();
    let mut running = true;
    let mut stagnant = 0;
    let mut next_step = 0.0f32;
    let mut generation = 0u32;
    println!("Tap to draw. Right column: run/pause, reseed, clear. Top-left quits.");

    'running: loop {
        let t = clock.tick(&events, &mut queue);

        for event in &queue {
            let Event::Press(button, _) = *event else {
                continue;
            };
            if button == QUIT {
                break 'running;
            }
            match button.to_xy() {
                Some((x, y)) => {
                    let i = (y * 8 + x) as usize;
                    board[i] = !board[i];
                    age[i] = 0;
                    history.clear();
                }
                None if button == Pad::right(0) => running = !running,
                None if button == Pad::right(1) => {
                    board = seed(&mut rng);
                    age = [0; 64];
                    history.clear();
                }
                None if button == Pad::right(2) => {
                    board = [false; 64];
                    age = [0; 64];
                    history.clear();
                }
                None => {}
            }
        }

        if running && t >= next_step {
            next_step = t + STEP;
            let next = step(&board);
            for (i, alive) in next.iter().enumerate() {
                age[i] = if *alive { age[i] + 1 } else { 0 };
            }

            // A repeat of any recent board means a still-life or a short cycle, and
            // it will never leave it. Comparing against the last few generations
            // catches 2- and 3-cycles, which is most of what 8x8 settles into.
            if history.contains(&next) {
                stagnant += 1;
            } else {
                stagnant = 0;
            }
            history.push(next);
            if history.len() > 4 {
                history.remove(0);
            }
            if stagnant >= STAGNANT_AFTER {
                println!("stagnant after {generation} generations — reseeding");
                board = seed(&mut rng);
                age = [0; 64];
                history.clear();
                stagnant = 0;
            } else {
                board = next;
            }
            generation += 1;
        }

        grid.clear();
        for y in 0..8 {
            for x in 0..8 {
                let i = (y * 8 + x) as usize;
                if board[i] {
                    // Newborn cells are green, long-lived ones drift to blue.
                    let settled = (age[i] as f32 / 12.0).min(1.0);
                    grid.set(x, y, hsv(0.33 + settled * 0.3, 0.9, 1.0));
                }
            }
        }
        let controls = [
            (Pad::right(0), if running { 21 } else { 5 }),
            (Pad::right(1), 37),
            (Pad::right(2), 1),
        ];
        pad.set_all(
            grid.lights()
                .chain(controls.map(|(p, c)| (p, Light::Solid(c)))),
        )?;
    }

    println!("{generation} generations ({:.1} fps)", clock.fps());
    Ok(())
}
