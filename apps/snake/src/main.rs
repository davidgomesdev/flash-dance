//! Snake on the 8x8 grid, steered with the device's own arrow buttons.
//!
//! The grid wraps at the edges — with only 64 cells, walls end the game before it
//! starts — so the only way to die is running into yourself. Because the arrows
//! occupy the top-left buttons this app quits with the **bottom scene button**
//! (right column, bottom) rather than the usual top-left.

use flash_dance::{Clock, Event, Grid, Launchpad, Pad, Result, Rng, hsv};
use std::collections::VecDeque;

const FPS: f64 = 30.0;
const FIRST_STEP: f32 = 0.35; // seconds per move
const FASTEST_STEP: f32 = 0.12;

fn quit_button() -> Pad {
    Pad::right(7)
}

/// The Mini MK3's top row starts with up, down, left, right.
fn direction_of(button: Pad) -> Option<(i32, i32)> {
    match button {
        b if b == Pad::top(0) => Some((0, -1)),
        b if b == Pad::top(1) => Some((0, 1)),
        b if b == Pad::top(2) => Some((-1, 0)),
        b if b == Pad::top(3) => Some((1, 0)),
        _ => None,
    }
}

struct Snake {
    body: VecDeque<(i32, i32)>,
    dir: (i32, i32),
    food: (i32, i32),
    step: f32,
    rng: Rng,
}

impl Snake {
    fn new() -> Snake {
        let mut rng = Rng::new();
        let body = VecDeque::from(vec![(3, 4), (2, 4), (1, 4)]);
        let food = (rng.below(8) as i32, rng.below(8) as i32);
        Snake {
            body,
            dir: (1, 0),
            food,
            step: FIRST_STEP,
            rng,
        }
    }

    fn place_food(&mut self) {
        // 64 cells and a short snake: rejection sampling is fine and always ends.
        loop {
            let spot = (self.rng.below(8) as i32, self.rng.below(8) as i32);
            if !self.body.contains(&spot) {
                self.food = spot;
                return;
            }
        }
    }

    /// Returns false when the snake bites itself.
    fn advance(&mut self) -> bool {
        let (hx, hy) = self.body[0];
        let head = (
            (hx + self.dir.0).rem_euclid(8),
            (hy + self.dir.1).rem_euclid(8),
        );

        let eating = head == self.food;
        if !eating {
            self.body.pop_back(); // free the tail cell before testing, it moves away
        }
        if self.body.contains(&head) {
            return false;
        }
        self.body.push_front(head);
        if eating {
            self.step = (self.step * 0.94).max(FASTEST_STEP);
            self.place_food();
        }
        true
    }
}

fn main() -> Result<()> {
    let (mut pad, events) = Launchpad::connect()?;
    let mut clock = Clock::new(FPS);
    let mut queue = Vec::new();
    let mut grid = Grid::new();
    let mut snake = Snake::new();
    snake.place_food();

    let mut next_step = 0.0f32;
    let mut turn: Option<(i32, i32)> = None;
    println!("Arrows steer, bottom scene button quits.");

    let alive = 'running: loop {
        let t = clock.tick(&events, &mut queue);

        for event in &queue {
            let Event::Press(button, _) = *event else {
                continue;
            };
            if button == quit_button() {
                break 'running true;
            }
            if let Some(d) = direction_of(button) {
                // Buffer the turn until the next step, and refuse a 180: reversing
                // between two frames would drive the head straight into the neck.
                if (d.0 + snake.dir.0, d.1 + snake.dir.1) != (0, 0) {
                    turn = Some(d);
                }
            }
        }

        if t >= next_step {
            next_step = t + snake.step;
            if let Some(d) = turn.take() {
                snake.dir = d;
            }
            if !snake.advance() {
                break 'running false;
            }
        }

        grid.clear();
        let length = snake.body.len() as f32;
        for (i, (x, y)) in snake.body.iter().enumerate() {
            // Head bright, tail dim, so the direction of travel is readable.
            let along = i as f32 / length;
            grid.set(*x, *y, hsv(0.28 + along * 0.12, 0.9, 1.0 - along * 0.65));
        }
        let pulse = 0.6 + 0.4 * (t * 8.0).sin();
        grid.set(snake.food.0, snake.food.1, hsv(0.0, 0.85, pulse));
        pad.set_all(grid.lights())?;
    };

    if !alive {
        for _ in 0..3 {
            grid.fill([0.6, 0.0, 0.0]);
            pad.set_all(grid.lights())?;
            std::thread::sleep(std::time::Duration::from_millis(120));
            grid.clear();
            pad.set_all(grid.lights())?;
            std::thread::sleep(std::time::Duration::from_millis(120));
        }
    }
    println!("length {} ({:.1} fps)", snake.body.len(), clock.fps());
    Ok(())
}
