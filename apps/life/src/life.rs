//! The creature's mathematics: a pure function of time, so it can be tested without a lamp.
//!
//! The output is a colour and a level. Its parts: a **baseline** (the time of day, or a fixed colour); **breathing**, a
//! sine whose period wanders; **drift**, smooth noise on the colour; **stirring**, a random brightening that raises a
//! decaying *excitement*; a fast small **flicker** for fire-like moods; and **pokes**, coloured pulses whose strength
//! falls the more often they come (habituation).
//!
//! A **mood** is a set of [`Params`]. The creature never jumps to a new mood: its current parameters ease toward the
//! target, so a change takes about fifteen seconds and always looks continuous. Nothing modulates faster than 3 Hz.

use crate::automaton::Grid;
use std::f64::consts::TAU;

/// What to show.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    pub rgb: [u8; 3],
    /// Percent, 0 to 100.
    pub level: u8,
}

impl Frame {
    /// The same light with the level moved into the colour channels (level 100): 255 steps instead of 100 at the dim
    /// end, so slow changes look smoother. The hue shifts slightly where a channel is small.
    pub fn fine(self) -> Frame {
        let k = f64::from(self.level) / 100.0;
        Frame {
            rgb: self.rgb.map(|c| (f64::from(c) * k).round() as u8),
            level: 100,
        }
    }
}

/// Hour, colour, level in percent. The first and last key repeat so the day wraps.
const DAY: [(f64, [f64; 3], f64); 9] = [
    (0.0, [255.0, 40.0, 0.0], 5.0),
    (5.0, [255.0, 40.0, 0.0], 4.0),
    (7.0, [255.0, 110.0, 15.0], 18.0),
    (9.0, [255.0, 150.0, 40.0], 30.0),
    (13.0, [255.0, 175.0, 80.0], 34.0),
    (17.0, [255.0, 130.0, 30.0], 30.0),
    (20.0, [255.0, 80.0, 10.0], 18.0),
    (23.0, [255.0, 45.0, 0.0], 8.0),
    (24.0, [255.0, 40.0, 0.0], 5.0),
];

/// The baseline colour and level for an hour of the day (`0.0..24.0`).
pub fn baseline(hour: f64) -> ([f64; 3], f64) {
    let h = hour.rem_euclid(24.0);
    for pair in DAY.windows(2) {
        let [(h0, c0, l0), (h1, c1, l1)] = pair else {
            continue;
        };
        let (h0, h1, l0, l1) = (*h0, *h1, *l0, *l1);
        if h >= h0 && h <= h1 {
            let f = if h1 > h0 { (h - h0) / (h1 - h0) } else { 0.0 };
            let f = f * f * (3.0 - 2.0 * f); // smoothstep: no kinks at the keys
            let mix = |a: f64, b: f64| a + (b - a) * f;
            return (
                [mix(c0[0], c1[0]), mix(c0[1], c1[1]), mix(c0[2], c1[2])],
                mix(l0, l1),
            );
        }
    }
    (DAY[0].1, DAY[0].2)
}

/// A personality. Every field is a number the creature eases toward, so changing one is always gradual.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Params {
    /// 1 follows the clock, 0 uses `colour` and `level` below.
    pub follow_day: f64,
    pub colour: [f64; 3],
    /// Percent, used when not following the day.
    pub level: f64,
    /// Seconds per breath.
    pub breath_period: f64,
    /// Breathing depth as a fraction of the level (0 to 0.5).
    pub breath_depth: f64,
    /// Multiplier on the colour wander.
    pub drift: f64,
    /// Mean seconds between spontaneous stirs.
    pub stir_every: f64,
    /// Gain on excitement from stirs and pokes.
    pub excitability: f64,
    /// Fast small flicker, 0 to 1 (fire).
    pub flicker: f64,
    /// Degrees the hue slowly wanders either side of the colour (0 to 120): what makes an aurora move through colours.
    pub sway: f64,
    /// How much the hidden Game of Life bends the light (0 to 1).
    pub automaton: f64,
    /// Pace of the hidden Game of Life, as a multiple (0.3 to 3).
    pub tempo: f64,
}

impl Params {
    /// The default: follows the day, breathes slowly, stirs now and then.
    pub const EMBER: Params = Params {
        follow_day: 1.0,
        colour: [255.0, 60.0, 5.0],
        level: 20.0,
        breath_period: 7.0,
        breath_depth: 0.10,
        drift: 1.0,
        stir_every: 90.0,
        excitability: 1.0,
        flicker: 0.0,
        sway: 0.0,
        automaton: 0.0,
        tempo: 1.0,
    };

    /// A named mood.
    pub fn mood(name: &str) -> Option<Params> {
        let e = Params::EMBER;
        Some(match name {
            "ember" => e,
            "campfire" => Params {
                follow_day: 0.0,
                colour: [255.0, 48.0, 0.0],
                level: 45.0,
                breath_period: 5.0,
                breath_depth: 0.15,
                drift: 1.6,
                stir_every: 25.0,
                excitability: 1.3,
                flicker: 0.7,
                sway: 8.0,
                automaton: 0.0,
                tempo: 1.0,
            },
            "calm" => Params {
                breath_period: 10.0,
                breath_depth: 0.08,
                drift: 0.6,
                stir_every: 240.0,
                excitability: 0.5,
                ..e
            },
            "focus" => Params {
                follow_day: 0.0,
                colour: [255.0, 200.0, 140.0],
                level: 40.0,
                breath_depth: 0.02,
                drift: 0.3,
                stir_every: 900.0,
                excitability: 0.2,
                ..e
            },
            "ocean" => Params {
                follow_day: 0.0,
                colour: [0.0, 110.0, 170.0],
                level: 35.0,
                breath_period: 9.0,
                breath_depth: 0.14,
                drift: 2.0,
                stir_every: 60.0,
                ..e
            },
            "night" => Params {
                follow_day: 0.0,
                colour: [255.0, 30.0, 0.0],
                level: 3.0,
                breath_depth: 0.05,
                stir_every: 900.0,
                excitability: 0.3,
                ..e
            },
            "aurora" => Params {
                follow_day: 0.0,
                colour: [20.0, 220.0, 120.0],
                level: 34.0,
                breath_period: 11.0,
                breath_depth: 0.12,
                drift: 1.0,
                stir_every: 50.0,
                sway: 85.0,
                ..e
            },
            "forest" => Params {
                follow_day: 0.0,
                colour: [40.0, 190.0, 50.0],
                level: 30.0,
                breath_period: 8.0,
                breath_depth: 0.10,
                sway: 22.0,
                stir_every: 70.0,
                ..e
            },
            "dusk" => Params {
                follow_day: 0.0,
                colour: [170.0, 60.0, 230.0],
                level: 28.0,
                breath_period: 9.0,
                breath_depth: 0.12,
                sway: 38.0,
                stir_every: 80.0,
                ..e
            },
            "sunset" => Params {
                follow_day: 0.0,
                colour: [255.0, 95.0, 15.0],
                level: 34.0,
                breath_period: 8.0,
                breath_depth: 0.10,
                sway: 16.0,
                stir_every: 100.0,
                ..e
            },
            "bloom" => Params {
                follow_day: 0.0,
                colour: [255.0, 55.0, 150.0],
                level: 30.0,
                breath_period: 7.0,
                breath_depth: 0.13,
                sway: 24.0,
                stir_every: 60.0,
                ..e
            },
            // a hidden Game of Life drives density, hue and pace
            "conway" => Params {
                follow_day: 0.0,
                colour: [70.0, 170.0, 215.0],
                level: 32.0,
                breath_period: 8.0,
                breath_depth: 0.07,
                drift: 0.5,
                sway: 8.0,
                stir_every: 150.0,
                automaton: 1.0,
                tempo: 1.6,
                ..e
            },
            _ => return None,
        })
    }

    /// Set one named number (clamped to a sensible range). `false` if the name is unknown.
    pub fn set(&mut self, key: &str, v: f64) -> bool {
        match key {
            "level" => self.level = v.clamp(0.0, 100.0),
            "breath_period" => self.breath_period = v.clamp(2.0, 30.0),
            "breath_depth" => self.breath_depth = v.clamp(0.0, 0.5),
            "drift" => self.drift = v.clamp(0.0, 3.0),
            "stir_every" => self.stir_every = v.clamp(5.0, 3600.0),
            "excitability" => self.excitability = v.clamp(0.0, 2.0),
            "flicker" => self.flicker = v.clamp(0.0, 1.0),
            "sway" => self.sway = v.clamp(0.0, 120.0),
            "automaton" => self.automaton = v.clamp(0.0, 1.0),
            "tempo" => self.tempo = v.clamp(0.3, 3.0),
            "follow_day" => self.follow_day = v.clamp(0.0, 1.0),
            _ => return false,
        }
        true
    }

    /// Ease toward `to`: first-order, time constant `tau` seconds, so a change is always continuous.
    fn ease(&mut self, to: &Params, dt: f64, tau: f64) {
        let k = 1.0 - (-dt / tau).exp();
        let m = |a: &mut f64, b: f64| *a += (b - *a) * k;
        m(&mut self.follow_day, to.follow_day);
        for (a, b) in self.colour.iter_mut().zip(to.colour) {
            m(a, b);
        }
        m(&mut self.level, to.level);
        m(&mut self.breath_period, to.breath_period);
        m(&mut self.breath_depth, to.breath_depth);
        m(&mut self.drift, to.drift);
        m(&mut self.stir_every, to.stir_every);
        m(&mut self.excitability, to.excitability);
        m(&mut self.flicker, to.flicker);
        m(&mut self.sway, to.sway);
        m(&mut self.automaton, to.automaton);
        m(&mut self.tempo, to.tempo);
    }
}

/// Rotate the hue of an RGB colour (0 to 255) by `degrees`, keeping saturation and value.
fn rotate_hue(rgb: [f64; 3], degrees: f64) -> [f64; 3] {
    let [r, g, b] = rgb.map(|c| c / 255.0);
    let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
    let d = mx - mn;
    if d <= 1e-9 || degrees.abs() < 1e-9 {
        return rgb;
    }
    let h = if mx == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if mx == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    let h = (h * 60.0 + degrees).rem_euclid(360.0);
    let (s, v) = (d / mx, mx);
    let (c, hp) = (v * s, h / 60.0);
    let x = c * (1.0 - (hp.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [(r1 + m) * 255.0, (g1 + m) * 255.0, (b1 + m) * 255.0]
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn lattice(seed: u64, i: i64) -> f64 {
    let mut x =
        (i as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ seed.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 27;
    (x >> 11) as f64 / (1u64 << 52) as f64 - 1.0 // -1..1
}

/// Smooth one-dimensional noise in `-1..1`.
pub fn noise(seed: u64, x: f64) -> f64 {
    let i = x.floor();
    let f = x - i;
    let f = f * f * (3.0 - 2.0 * f);
    let (a, b) = (lattice(seed, i as i64), lattice(seed, i as i64 + 1));
    a + (b - a) * f
}

struct Poke {
    at: f64,
    colour: [f64; 3],
    strength: f64,
}

/// The creature.
pub struct Life {
    seed: u64,
    rng: Rng,
    last_t: f64,
    phase: f64,
    excitement: f64,
    next_stir: f64,
    pokes: Vec<Poke>,
    now: Params,
    target: Params,
    /// The hidden Game of Life, when its next generation is due, and its features smoothed so that no single
    /// generation can make the light jump.
    grid: Grid,
    grid_next: f64,
    density: f64,
    gathering: f64,
    activity: f64,
}

/// Seconds for a mood change to settle (the time constant).
const MOOD_TAU: f64 = 6.0;
/// Seconds between generations of the hidden Game of Life (about three a second: slow enough to read as drift).
const GENERATION: f64 = 0.35;
/// Time constant, in seconds, of the smoothing of the grid's features.
const FEATURE_TAU: f64 = 0.9;

impl Life {
    pub fn new(seed: u64) -> Life {
        let mut rng = Rng(seed | 1);
        let next_stir = 20.0 + 40.0 * rng.next();
        let grid = Grid::random(&mut || rng.next(), 0.3);
        let (density, gathering) = grid.features();
        Life {
            seed,
            rng,
            last_t: 0.0,
            phase: 0.0,
            excitement: 0.0,
            next_stir,
            pokes: Vec::new(),
            now: Params::EMBER,
            target: Params::EMBER,
            grid,
            grid_next: GENERATION,
            density,
            gathering,
            activity: 0.0,
        }
    }

    /// Where the personality is heading.
    pub fn target_mut(&mut self) -> &mut Params {
        &mut self.target
    }

    /// Something happened. Repeated pokes within a minute count for less: `0.6^n`.
    #[cfg(test)]
    pub fn poke(&mut self, t: f64, colour: [f64; 3]) {
        self.poke_at(t, colour, None);
    }

    /// As [`Life::poke`], and in a Game-of-Life mood the glider is dropped into the given grid column, so that
    /// different kinds of event push the hue in different, recognisable directions.
    pub fn poke_at(&mut self, t: f64, colour: [f64; 3], column: Option<i32>) {
        let recent = self.pokes.iter().filter(|p| t - p.at < 60.0).count();
        let strength = 0.6f64.powi(i32::try_from(recent).unwrap_or(8));
        self.excitement = (self.excitement + 0.6 * strength * self.now.excitability).min(1.0);
        if self.now.automaton > 0.05 {
            // the world is touched: a glider is dropped into the hidden grid
            let rng = &mut self.rng;
            let x = column.unwrap_or((rng.next() * 10.0) as i32);
            let y = (rng.next() * 10.0) as i32;
            self.grid.glider(x, y);
            self.grid.glider(x, y + 5); // two, so the effect is strong enough to read
        }
        self.pokes.push(Poke {
            at: t,
            colour,
            strength,
        });
        self.pokes.retain(|p| t - p.at < 60.0);
    }

    /// The frame for time `t` (seconds since start) at local `hour`, never brighter than `max_level` percent.
    pub fn step(&mut self, t: f64, hour: f64, max_level: f64) -> Frame {
        let dt = (t - self.last_t).clamp(0.0, 0.5);
        self.last_t = t;
        let target = self.target;
        self.now.ease(&target, dt, MOOD_TAU);
        let p = self.now;
        self.excitement *= (-dt / 6.0).exp();
        if t >= self.next_stir {
            self.excitement = (self.excitement + 0.5 * p.excitability).min(1.0);
            self.next_stir = t + p.stir_every * (0.4 + 1.2 * self.rng.next());
        }
        let e = self.excitement;
        // the hidden Game of Life: advance it on its own clock, read slow features from it
        if t - self.grid_next > 5.0 {
            self.grid_next = t; // do not catch up after a long gap
        }
        while t >= self.grid_next {
            let rng = &mut self.rng;
            self.grid.step_alive(&mut || rng.next());
            self.grid_next += GENERATION / p.tempo.max(0.3);
        }
        let k = 1.0 - (-dt / FEATURE_TAU).exp();
        let (density, gathering) = self.grid.features();
        self.density += (density - self.density) * k;
        self.gathering += (gathering - self.gathering) * k;
        self.activity += (self.grid.changed as f64 / 100.0 - self.activity) * k;
        let a = p.automaton;
        // breathing: a wandering period, never faster than 0.5 Hz; a busy grid breathes a little quicker
        let wander = 1.0 + 0.25 * noise(self.seed, t / 40.0);
        let period = ((p.breath_period * wander)
            .clamp(p.breath_period * 0.6, p.breath_period * 1.4)
            / (1.0 + 0.8 * e + 2.5 * a * self.activity))
            .max(2.0);
        self.phase += dt / period;
        let breath = (TAU * self.phase).sin();
        let (day, day_level) = baseline(hour);
        let f = p.follow_day.clamp(0.0, 1.0);
        let mix = |a: f64, b: f64| a + (b - a) * f;
        let base = rotate_hue(
            [
                mix(p.colour[0], day[0]),
                mix(p.colour[1], day[1]),
                mix(p.colour[2], day[2]),
            ],
            p.sway * noise(self.seed + 6, t / 35.0) + a * 110.0 * self.gathering,
        );
        // a fuller grid is a little brighter, a sparser one a little dimmer
        let base_level =
            mix(p.level, day_level) * (1.0 + a * 2.0 * (self.density - 0.25).clamp(-0.2, 0.3));
        // fire: a small fast component, band-limited to 2.5 Hz so it never becomes a flash
        let fast = noise(self.seed + 4, t * 2.5) * 0.7 + noise(self.seed + 5, t * 1.1) * 0.3;
        let mut level = base_level
            * (1.0 + (p.breath_depth + 0.15 * e) * breath)
            * (1.0 + 0.5 * e)
            * (1.0 + 0.22 * p.flicker * fast);
        let mut rgb = [
            base[0] + 10.0 * p.drift * noise(self.seed + 1, t / 53.0),
            base[1]
                + 18.0 * p.drift * noise(self.seed + 2, t / 41.0)
                + 25.0 * e
                + 14.0 * p.flicker * fast,
            base[2] + 6.0 * p.drift * noise(self.seed + 3, t / 67.0),
        ];
        for k in &self.pokes {
            let age = t - k.at;
            let env = if age < 0.25 {
                age / 0.25
            } else {
                (-(age - 0.25) / 2.5).exp()
            };
            let w = (k.strength * env).clamp(0.0, 1.0);
            for (c, to) in rgb.iter_mut().zip(k.colour) {
                *c += (to - *c) * w;
            }
            level += 40.0 * w;
        }
        Frame {
            rgb: rgb.map(|c| c.round().clamp(0.0, 255.0) as u8),
            level: level.clamp(0.0, max_level.clamp(0.0, 100.0)).round() as u8,
        }
    }
}

/// Where in the hidden grid a kind of event drops its glider: left and right sit on opposite sides of the hue
/// wheel's push, so different events read as different directions.
pub fn poke_column(name: &str) -> Option<i32> {
    match name {
        "good" => Some(7),
        "info" => Some(2),
        "warn" => Some(5),
        "bad" => Some(0),
        _ => None,
    }
}

/// Poke names to colours.
pub fn poke_colour(name: &str) -> [f64; 3] {
    match name {
        "good" => [60.0, 255.0, 120.0],
        "info" => [80.0, 160.0, 255.0],
        "warn" => [255.0, 170.0, 30.0],
        "bad" => [255.0, 40.0, 40.0],
        _ => [255.0, 220.0, 180.0],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(seed: u64, secs: f64, hour: f64) -> Vec<Frame> {
        let mut l = Life::new(seed);
        (0..(secs * 20.0) as usize)
            .map(|i| l.step(i as f64 / 20.0, hour, 100.0))
            .collect()
    }

    fn reversals_per_second(l: &mut Life, hour: f64, secs: usize) -> f64 {
        let frames: Vec<f64> = (0..secs * 20)
            .map(|i| f64::from(l.step(i as f64 / 20.0, hour, 100.0).level))
            .collect();
        let n = frames
            .windows(3)
            .filter(|w| (w[1] - w[0]) * (w[2] - w[1]) < 0.0 && (w[1] - w[0]).abs() >= 1.0)
            .count();
        n as f64 / secs as f64
    }

    #[test]
    fn it_is_deterministic_and_seeds_differ() {
        assert_eq!(run(1, 30.0, 12.0), run(1, 30.0, 12.0));
        assert_ne!(run(1, 30.0, 12.0), run(2, 30.0, 12.0));
    }

    #[test]
    fn the_day_wraps_without_a_jump_and_nights_are_dim() {
        let (_, a) = baseline(23.999);
        let (_, b) = baseline(0.0);
        assert!((a - b).abs() < 0.5, "midnight is continuous: {a} vs {b}");
        assert!(baseline(3.0).1 < 6.0, "deep night is dim");
        assert!(
            baseline(13.0).1 > baseline(21.0).1,
            "afternoon is brighter than the evening"
        );
        for h in 0..2400 {
            let (c, l) = baseline(f64::from(h) / 100.0);
            assert!(c.iter().all(|v| (0.0..=255.0).contains(v)) && (0.0..=100.0).contains(&l));
        }
    }

    #[test]
    fn it_never_exceeds_the_ceiling_and_never_goes_out_of_range() {
        for mood in [
            "ember", "campfire", "calm", "focus", "ocean", "night", "aurora", "forest", "dusk",
            "sunset", "bloom",
        ] {
            let mut l = Life::new(9);
            *l.target_mut() = Params::mood(mood).unwrap();
            for i in 0..4000 {
                let t = f64::from(i) / 20.0;
                if i % 300 == 0 {
                    l.poke(t, poke_colour("bad"));
                }
                let f = l.step(t, 14.0, 40.0);
                assert!(f.level <= 40, "{mood}: level {} at {t}", f.level);
            }
        }
    }

    #[test]
    fn no_mood_modulates_fast_enough_to_flicker() {
        // brightness reversals stay under 3 per second in every mood, even excited
        for mood in [
            "ember", "campfire", "calm", "focus", "ocean", "night", "aurora", "forest", "dusk",
            "sunset", "bloom",
        ] {
            let mut l = Life::new(5);
            *l.target_mut() = Params::mood(mood).unwrap();
            l.now = Params::mood(mood).unwrap();
            l.poke(0.0, poke_colour("info"));
            let r = reversals_per_second(&mut l, 13.0, 120);
            assert!(r < 3.0, "{mood}: {r} reversals per second");
        }
    }

    #[test]
    fn a_mood_change_glides_and_never_jumps() {
        let mut l = Life::new(2);
        for i in 0..400 {
            l.step(f64::from(i) / 20.0, 13.0, 100.0);
        }
        *l.target_mut() = Params::mood("campfire").unwrap();
        let mut prev = l.step(20.0, 13.0, 100.0);
        let mut worst = 0i32;
        for i in 1..600 {
            let f = l.step(20.0 + f64::from(i) / 20.0, 13.0, 100.0);
            worst = worst.max((i32::from(f.level) - i32::from(prev.level)).abs());
            prev = f;
        }
        assert!(worst <= 6, "largest single-frame step {worst} percent");
        // and it arrives: the campfire is redder and brighter than the afternoon
        assert!(
            prev.rgb[1] < 120 && prev.level >= 35,
            "settled on campfire: {prev:?}"
        );
    }

    #[test]
    fn it_stirs_on_its_own_and_calms_again() {
        let mut l = Life::new(3);
        let mut peak: f64 = 0.0;
        let mut at_end = 0.0;
        for i in 0..(400 * 20) {
            l.step(f64::from(i) / 20.0, 13.0, 100.0);
            peak = peak.max(l.excitement);
            at_end = l.excitement;
        }
        assert!(
            peak > 0.4,
            "it stirred at least once in ~7 minutes (peak {peak})"
        );
        assert!(at_end < 1.0);
    }

    #[test]
    fn repeated_pokes_count_for_less() {
        let mut l = Life::new(1);
        l.poke(0.0, poke_colour("good"));
        let first = l.pokes[0].strength;
        for k in 1..5 {
            l.poke(f64::from(k), poke_colour("good"));
        }
        let last = l.pokes.last().map_or(1.0, |p| p.strength);
        assert!(last < first * 0.3, "habituated: {last} vs {first}");
        let mut m = Life::new(1);
        m.poke(0.0, poke_colour("good"));
        m.poke(100.0, poke_colour("good"));
        assert!(
            (m.pokes.last().map_or(0.0, |p| p.strength) - 1.0).abs() < 1e-9,
            "a poke after a quiet minute is fresh"
        );
    }

    #[test]
    fn in_the_game_of_life_mood_different_pokes_push_the_hue_different_ways() {
        let hue_after = |name: &str| {
            let mut l = Life::new(11);
            *l.target_mut() = Params::mood("conway").unwrap();
            l.now = Params::mood("conway").unwrap();
            for i in 0..400 {
                l.step(f64::from(i) / 20.0, 13.0, 100.0);
            }
            let before = l.gathering;
            l.poke_at(20.0, poke_colour(name), poke_column(name));
            for i in 0..80 {
                l.step(20.0 + f64::from(i) / 20.0, 13.0, 100.0);
            }
            l.gathering - before
        };
        let (good, info) = (hue_after("good"), hue_after("info"));
        assert!(
            (good - info).abs() > 0.02,
            "good and info move the hue differently: {good} vs {info}"
        );
    }

    #[test]
    fn a_poke_visibly_shifts_the_colour_then_fades() {
        let mut l = Life::new(1);
        let before = l.step(10.0, 13.0, 100.0);
        l.poke(10.0, poke_colour("good"));
        let peak = l.step(10.4, 13.0, 100.0);
        assert!(
            peak.rgb[1] > before.rgb[1] && peak.rgb[2] > before.rgb[2],
            "greener: {before:?} -> {peak:?}"
        );
        let later = l.step(40.0, 13.0, 100.0);
        assert!(later.rgb[1] < peak.rgb[1], "it fades");
    }

    #[test]
    fn moods_and_settings_validate() {
        assert!(Params::mood("nope").is_none());
        let mut p = Params::EMBER;
        assert!(
            p.set("flicker", 9.0) && (p.flicker - 1.0).abs() < 1e-9,
            "clamped"
        );
        assert!(p.set("breath_period", 0.1) && p.breath_period >= 2.0);
        assert!(!p.set("nonsense", 1.0));
    }

    #[test]
    fn hue_rotation_keeps_brightness_and_turns_the_colour() {
        let red = [255.0, 0.0, 0.0];
        let g = rotate_hue(red, 120.0);
        assert!(
            g[1] > 250.0 && g[0] < 5.0 && g[2] < 5.0,
            "red turned 120 degrees is green: {g:?}"
        );
        assert_eq!(rotate_hue(red, 0.0), red);
        let grey = [90.0, 90.0, 90.0];
        assert_eq!(rotate_hue(grey, 77.0), grey, "grey has no hue to turn");
        let full = rotate_hue([200.0, 100.0, 50.0], 360.0);
        assert!(
            (full[0] - 200.0).abs() < 1e-6 && (full[1] - 100.0).abs() < 1e-6,
            "a full turn is the same colour"
        );
    }

    #[test]
    fn the_fine_form_is_the_same_light_with_finer_steps() {
        let f = Frame {
            rgb: [255, 100, 10],
            level: 20,
        }
        .fine();
        assert_eq!((f.rgb, f.level), ([51, 20, 2], 100));
        let full = Frame {
            rgb: [1, 2, 3],
            level: 100,
        };
        assert_eq!(full.fine(), full);
    }
}
