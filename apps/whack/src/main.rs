//! Whack-a-mole. One pad lights; hit it before it expires.
//!
//! The mole fades green to red as its time runs out and gets faster with every hit.
//! Score is the bar on the right column, lives are the red pads on the top row,
//! and a miss is either hitting the wrong pad or letting one expire.

use flash_dance::{Clock, Event, Grid, Launchpad, Light, Pad, Result, Rng, hsv};

const QUIT: Pad = Pad(91);
const FPS: f64 = 30.0;
const LIVES: u32 = 3;
const FIRST_MOLE: f32 = 1.2;
const FASTEST_MOLE: f32 = 0.35;

struct Game {
    rng: Rng,
    mole: (i32, i32),
    spawned: f32,
    lifetime: f32,
    score: u32,
    lives: u32,
}

impl Game {
    fn new(t: f32) -> Game {
        let mut rng = Rng::new();
        Game {
            mole: (rng.below(8) as i32, rng.below(8) as i32),
            rng,
            spawned: t,
            lifetime: FIRST_MOLE,
            score: 0,
            lives: LIVES,
        }
    }

    /// Moves the mole somewhere else — never back onto the pad just hit, or a fast
    /// double-tap scores twice for one reaction.
    fn respawn(&mut self, t: f32) {
        let previous = self.mole;
        while self.mole == previous {
            self.mole = (self.rng.below(8) as i32, self.rng.below(8) as i32);
        }
        self.spawned = t;
    }

    fn hit(&mut self, t: f32) {
        self.score += 1;
        self.lifetime = (self.lifetime * 0.97).max(FASTEST_MOLE);
        self.respawn(t);
    }

    fn miss(&mut self, t: f32) {
        self.lives = self.lives.saturating_sub(1);
        self.respawn(t);
    }
}

fn main() -> Result<()> {
    let (mut pad, events) = Launchpad::connect()?;
    let mut clock = Clock::new(FPS);
    let mut queue = Vec::new();
    let mut grid = Grid::new();
    let mut game = Game::new(0.0);
    println!("Whack the lit pad. Top-left quits.");

    loop {
        let t = clock.tick(&events, &mut queue);
        let mut quit = false;
        for event in &queue {
            match *event {
                Event::Press(QUIT, _) => quit = true,
                Event::Press(button, _) => match button.to_xy() {
                    Some((x, y)) if (x as i32, y as i32) == game.mole => game.hit(t),
                    Some(_) => game.miss(t),
                    None => {}
                },
                _ => {}
            }
        }
        if quit || game.lives == 0 {
            break;
        }
        if t - game.spawned > game.lifetime {
            game.miss(t);
        }

        // Green while there is time, red as it runs out.
        let left = 1.0 - ((t - game.spawned) / game.lifetime).clamp(0.0, 1.0);
        grid.clear();
        grid.set(game.mole.0, game.mole.1, hsv(left * 0.33, 1.0, 1.0));

        // The bar fills, changes colour, and starts again every 8 points.
        let filled = match game.score % 8 {
            0 if game.score > 0 => 8,
            remainder => remainder as u8,
        };
        let colour = Light::Solid(21 + (game.score / 8 % 4) as u8 * 8);
        let score_bar = (0..8).map(move |i| {
            let light = if i < filled { colour } else { Light::Off };
            (Pad::right(7 - i), light)
        });
        let lives = (0..LIVES).map(|i| {
            let light = if i < game.lives {
                Light::Solid(5)
            } else {
                Light::Off
            };
            (Pad::top(7 - i as u8), light)
        });
        pad.set_all(grid.lights().chain(score_bar).chain(lives))?;
    }

    println!("score {} ({:.1} fps)", game.score, clock.fps());
    Ok(())
}
