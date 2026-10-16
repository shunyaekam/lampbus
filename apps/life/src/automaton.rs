//! Conway's Game of Life on a small torus, used as the creature's nervous system.
//!
//! The rules are fixed and simple; what they produce is not: structure that moves, collides, dies back and returns,
//! without repeating. That is the "edge of chaos" behaviour that makes something look alive. The creature reads a few
//! slow features from the grid (how full it is, where the life is gathered, how much is changing) and lets them bend
//! its light. Pokes drop a glider in. A grid that dies out, fills up or falls into a short loop is given a spark, so it
//! never settles for good.

use std::f64::consts::TAU;

pub const W: usize = 10;
pub const H: usize = 10;

pub struct Grid {
    cells: Vec<bool>,
    history: [u64; 8],
    next: usize,
    /// How many cells changed in the last step.
    pub changed: usize,
}

impl Grid {
    pub fn from_cells(cells: Vec<bool>) -> Grid {
        let mut g = Grid {
            cells,
            history: [0; 8],
            next: 0,
            changed: 0,
        };
        g.cells.resize(W * H, false);
        g
    }

    /// Random cells alive with the given probability.
    pub fn random(rng: &mut dyn FnMut() -> f64, density: f64) -> Grid {
        Grid::from_cells((0..W * H).map(|_| rng() < density).collect())
    }

    pub fn alive(&self, x: i32, y: i32) -> bool {
        let (w, h) = (W as i32, H as i32);
        self.cells
            .get((y.rem_euclid(h) * w + x.rem_euclid(w)) as usize)
            .copied()
            .unwrap_or(false)
    }

    pub fn population(&self) -> usize {
        self.cells.iter().filter(|c| **c).count()
    }

    fn set(&mut self, x: i32, y: i32, v: bool) {
        let (w, h) = (W as i32, H as i32);
        if let Some(c) = self
            .cells
            .get_mut((y.rem_euclid(h) * w + x.rem_euclid(w)) as usize)
        {
            *c = v;
        }
    }

    fn hash(&self) -> u64 {
        self.cells.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, c| {
            (h ^ u64::from(*c)).wrapping_mul(0x0100_0000_01b3)
        })
    }

    /// One generation: a live cell with two or three live neighbours survives, a dead cell with exactly three is born.
    pub fn step(&mut self) {
        let mut next = vec![false; W * H];
        let mut changed = 0;
        for y in 0..H as i32 {
            for x in 0..W as i32 {
                let n = (-1..=1)
                    .flat_map(|dy| (-1..=1).map(move |dx| (dx, dy)))
                    .filter(|&(dx, dy)| (dx, dy) != (0, 0) && self.alive(x + dx, y + dy))
                    .count();
                let now = self.alive(x, y);
                let after = matches!((now, n), (true, 2 | 3) | (false, 3));
                changed += usize::from(after != now);
                if let Some(c) = next.get_mut((y as usize) * W + x as usize) {
                    *c = after;
                }
            }
        }
        self.cells = next;
        self.changed = changed;
    }

    /// Toggle a few cells alive near a random place.
    pub fn spark(&mut self, rng: &mut dyn FnMut() -> f64) {
        let (cx, cy) = ((rng() * W as f64) as i32, (rng() * H as f64) as i32);
        for _ in 0..6 {
            let (dx, dy) = ((rng() * 3.0) as i32 - 1, (rng() * 3.0) as i32 - 1);
            self.set(cx + dx, cy + dy, true);
        }
    }

    /// A glider heading down and to the right, with its top-left corner at `(x, y)`.
    pub fn glider(&mut self, x: i32, y: i32) {
        for (dx, dy) in [(1, 0), (2, 1), (0, 2), (1, 2), (2, 2)] {
            self.set(x + dx, y + dy, true);
        }
    }

    /// Step, then make sure it keeps going: reseed when it has died out or filled up, spark it when it has fallen into
    /// a loop of up to eight generations.
    pub fn step_alive(&mut self, rng: &mut dyn FnMut() -> f64) {
        self.step();
        let pop = self.population();
        if !(W * H / 14..=W * H * 7 / 10).contains(&pop) {
            *self = Grid::random(rng, 0.28);
            return;
        }
        let seen = self.history.contains(&self.hash());
        if seen {
            self.spark(rng);
        }
        let now = self.hash();
        let slot = self.next;
        if let Some(s) = self.history.get_mut(slot) {
            *s = now;
        }
        self.next = (slot + 1) % self.history.len();
    }

    /// `(density, gathering)`: the fraction of cells alive (0 to 1), and where the life is gathered round the torus
    /// as a position in `-1..1` (0 when it is spread evenly or empty).
    pub fn features(&self) -> (f64, f64) {
        let (mut sin, mut cos) = (0.0, 0.0);
        for y in 0..H as i32 {
            for x in 0..W as i32 {
                if self.alive(x, y) {
                    let a = TAU * f64::from(x) / W as f64;
                    sin += a.sin();
                    cos += a.cos();
                }
            }
        }
        let strength = (sin * sin + cos * cos).sqrt() / self.population().max(1) as f64;
        let angle = sin.atan2(cos) / std::f64::consts::PI;
        (self.population() as f64 / (W * H) as f64, angle * strength)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(cells: &[(i32, i32)]) -> Grid {
        let mut g = Grid::from_cells(vec![false; W * H]);
        for &(x, y) in cells {
            g.set(x, y, true);
        }
        g
    }

    fn lcg() -> impl FnMut() -> f64 {
        let mut s = 12345u64;
        move || {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (s >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    #[test]
    fn a_block_is_still_and_a_blinker_has_period_two() {
        let mut block = pattern(&[(2, 2), (3, 2), (2, 3), (3, 3)]);
        let h = block.hash();
        block.step();
        assert_eq!(block.hash(), h, "a block does not change");
        let mut blinker = pattern(&[(4, 3), (4, 4), (4, 5)]);
        let h0 = blinker.hash();
        blinker.step();
        assert_ne!(blinker.hash(), h0);
        assert!(
            blinker.alive(3, 4) && blinker.alive(4, 4) && blinker.alive(5, 4),
            "it turns on its side"
        );
        blinker.step();
        assert_eq!(blinker.hash(), h0, "and back");
    }

    #[test]
    fn a_glider_walks_one_cell_diagonally_every_four_generations() {
        let mut g = Grid::from_cells(vec![false; W * H]);
        g.glider(1, 1);
        let before = g.hash();
        for _ in 0..4 {
            g.step();
        }
        let mut moved = Grid::from_cells(vec![false; W * H]);
        moved.glider(2, 2);
        assert_eq!(
            g.hash(),
            moved.hash(),
            "the same five cells, one step down and right"
        );
        assert_ne!(g.hash(), before);
        assert_eq!(g.population(), 5);
    }

    #[test]
    fn the_world_is_a_torus() {
        // a glider walks off one edge and comes back on the other after 4 * 10 generations
        let mut g = Grid::from_cells(vec![false; W * H]);
        g.glider(1, 1);
        let start = g.hash();
        for _ in 0..4 * W {
            g.step();
        }
        assert_eq!(g.hash(), start, "once round the torus it is where it began");
    }

    #[test]
    fn it_never_dies_fills_up_or_freezes() {
        let mut rng = lcg();
        let mut g = Grid::random(&mut rng, 0.3);
        let mut stuck = 0;
        let mut last = 0;
        for i in 0..20_000 {
            g.step_alive(&mut rng);
            let p = g.population();
            assert!(
                (W * H / 14..=W * H * 7 / 10).contains(&p),
                "step {i}: population {p}"
            );
            stuck = if g.changed == 0 { stuck + 1 } else { 0 };
            assert!(stuck < 12, "frozen for {stuck} generations at step {i}");
            last = p;
        }
        assert!(last > 0);
    }

    #[test]
    fn features_are_bounded_and_see_where_the_life_gathers() {
        let (d, a) = pattern(&[]).features();
        assert_eq!((d, a), (0.0, 0.0));
        let left = pattern(&[(1, 2), (1, 3), (1, 4), (2, 3)]).features();
        let right = pattern(&[(7, 2), (7, 3), (7, 4), (6, 3)]).features();
        assert!(
            left.1 * right.1 < 0.0,
            "life on opposite sides gives opposite signs: {left:?} {right:?}"
        );
        let mut rng = lcg();
        for _ in 0..200 {
            let (d, a) = Grid::random(&mut rng, 0.3).features();
            assert!((0.0..=1.0).contains(&d) && (-1.0..=1.0).contains(&a));
        }
    }
}
