//! Output pipeline: turn an ideal colour into the integers a lamp can show without visible banding.
//!
//! A lamp takes 8-bit channels and a coarse brightness step, at a limited update rate. Three small stages recover
//! quality that naive rounding throws away:
//!
//! - [`Calibration`]: a response curve, so equal steps in the source look like equal steps on the lamp.
//! - [`Smoother`]: an asymmetric low-pass filter (fast attack, slow release) that tames noisy live signals such as
//!   screen colour or audio energy without adding much latency on rises.
//! - [`Dither`]: first-order error diffusion across frames. A sunrise at 1% brightness has only a handful of device
//!   steps; alternating neighbouring steps over time gives smooth in-between levels. Only meaningful when frames
//!   are sent faster than the eye can resolve flicker (about 20 per second or more).

use crate::color::{hsv_to_rgb, rgb_to_hsv};

/// Four values flow through the pipeline per frame: red, green, blue and level.
pub type Quad = [f64; 4];

/// Response curve applied to a `0..=1` value: `v.powf(gamma)`. `gamma = 1.0` is the identity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Calibration {
    /// Exponent. Values above 1 darken the mid-tones (what most LED lamps need to look linear to the eye).
    pub gamma: f64,
}

impl Default for Calibration {
    fn default() -> Self {
        Calibration { gamma: 1.0 }
    }
}

impl Calibration {
    /// Apply the curve to `v` in `0..=1`.
    pub fn apply(&self, v: f64) -> f64 {
        let v = v.clamp(0.0, 1.0);
        if (self.gamma - 1.0).abs() < 1e-12 {
            v
        } else {
            v.powf(self.gamma)
        }
    }
}

/// CIE lightness `0..=1` (perceptual, equal steps look equal) to relative luminance `0..=1` (what a linear-duty
/// lamp actually emits).
pub fn lightness_to_luminance(p: f64) -> f64 {
    let l = p.clamp(0.0, 1.0) * 100.0;
    if l > 8.0 {
        ((l + 16.0) / 116.0).powi(3)
    } else {
        l / 903.3
    }
}

/// Asymmetric one-pole low-pass filter over four channels.
#[derive(Debug, Clone)]
pub struct Smoother {
    attack_ms: f64,
    release_ms: f64,
    state: Option<Quad>,
}

impl Smoother {
    /// `attack_ms`: time constant when a channel rises; `release_ms`: when it falls. Zero disables smoothing in
    /// that direction.
    pub fn new(attack_ms: f64, release_ms: f64) -> Self {
        Smoother {
            attack_ms: attack_ms.max(0.0),
            release_ms: release_ms.max(0.0),
            state: None,
        }
    }

    /// Advance by `dt_ms` toward `target` and return the smoothed value. The first call returns `target`.
    pub fn step(&mut self, target: Quad, dt_ms: f64) -> Quad {
        let Some(prev) = self.state else {
            self.state = Some(target);
            return target;
        };
        let mut out = prev;
        for ((o, p), t) in out.iter_mut().zip(prev).zip(target) {
            let tau = if t > p {
                self.attack_ms
            } else {
                self.release_ms
            };
            let alpha = if tau <= 0.0 {
                1.0
            } else {
                1.0 - (-dt_ms.max(0.0) / tau).exp()
            };
            *o = p + (t - p) * alpha;
        }
        self.state = Some(out);
        out
    }

    /// Forget history (for example when the source changes).
    pub fn reset(&mut self) {
        self.state = None;
    }
}

/// Smooths a live colour the way a painter mixes it: the hue moves the short way round the colour wheel and keeps its
/// saturation, instead of letting the red, green and blue channels cross-fade. Cross-fading channels passes through
/// grey or white between two different colours (blue to yellow has all three channels lit on the way), which on a
/// lamp reads as a white flash between every colour. Brightness (value) and the fourth channel keep the asymmetric
/// fast-attack, slow-release behaviour of [`Smoother`].
#[derive(Debug, Clone)]
pub struct ColourSmoother {
    attack_ms: f64,
    release_ms: f64,
    /// Hue, saturation, value (each `0..=1`) and the fourth channel.
    state: Option<[f64; 4]>,
}

impl ColourSmoother {
    /// `attack_ms` and `release_ms` as for [`Smoother`], applied to brightness. Hue and saturation move at half the
    /// attack rate, in both directions.
    pub fn new(attack_ms: f64, release_ms: f64) -> Self {
        ColourSmoother {
            attack_ms: attack_ms.max(0.0),
            release_ms: release_ms.max(0.0),
            state: None,
        }
    }

    /// Advance by `dt_ms` toward `target` (`[r, g, b]` in `0..=255`, then the fourth channel) and return the smoothed
    /// value. The first call returns `target`.
    pub fn step(&mut self, target: Quad, dt_ms: f64) -> Quad {
        let [r, g, b, l] = target;
        let [ht, st, vt] = rgb_to_hsv(r / 255.0, g / 255.0, b / 255.0);
        let Some([h, s, v, lv]) = self.state else {
            self.state = Some([ht, st, vt, l]);
            return target;
        };
        let dt = dt_ms.max(0.0);
        let ease = |tau: f64| {
            if tau <= 0.0 {
                1.0
            } else {
                1.0 - (-dt / tau).exp()
            }
        };
        let kc = ease(self.attack_ms * 2.0);
        let brightness = |p: f64, t: f64| {
            p + (t - p)
                * ease(if t > p {
                    self.attack_ms
                } else {
                    self.release_ms
                })
        };
        // a target with no saturation (grey, white, black) has no hue to move toward, and a colour that has faded to
        // grey can take the new hue at once: neither can be seen
        let h2 = if st < 0.03 {
            h
        } else if s < 0.03 {
            ht
        } else {
            let d = (ht - h).rem_euclid(1.0);
            (h + if d > 0.5 { d - 1.0 } else { d } * kc).rem_euclid(1.0)
        };
        let next = [h2, s + (st - s) * kc, brightness(v, vt), brightness(lv, l)];
        self.state = Some(next);
        let rgb = hsv_to_rgb(next[0], next[1].clamp(0.0, 1.0), next[2].clamp(0.0, 1.0));
        [rgb[0] * 255.0, rgb[1] * 255.0, rgb[2] * 255.0, next[3]]
    }

    /// Forget history (for example when the source changes).
    pub fn reset(&mut self) {
        self.state = None;
    }
}

/// First-order error-diffusion quantiser over four channels.
#[derive(Debug, Clone, Default)]
pub struct Dither {
    err: Quad,
}

impl Dither {
    /// A quantiser with no accumulated error.
    pub fn new() -> Self {
        Dither::default()
    }

    /// Quantise `v` to integers within `0..=max` per channel, carrying the rounding error into the next call.
    /// Over many frames the mean of the outputs equals the mean of the inputs.
    pub fn quantize(&mut self, v: Quad, max: [u8; 4]) -> [u8; 4] {
        let mut out = [0u8; 4];
        for (((o, e), want), m) in out.iter_mut().zip(self.err.iter_mut()).zip(v).zip(max) {
            let wanted = want + *e;
            let q = wanted.round().clamp(0.0, f64::from(m));
            *e = (wanted - q).clamp(-1.0, 1.0); // bounded so a clipped channel cannot wind up
            *o = q as u8;
        }
        out
    }

    /// Drop accumulated error.
    pub fn reset(&mut self) {
        self.err = [0.0; 4];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lowest saturation reached along a transition between two colours, smoothed at 50 updates a second.
    fn lowest_saturation(mut step: impl FnMut(Quad) -> Quad, from: [f64; 3], to: [f64; 3]) -> f64 {
        step([from[0], from[1], from[2], 100.0]);
        let mut lowest: f64 = 1.0;
        for _ in 0..60 {
            let q = step([to[0], to[1], to[2], 100.0]);
            let [_, s, v] = rgb_to_hsv(q[0] / 255.0, q[1] / 255.0, q[2] / 255.0);
            if v > 0.05 {
                lowest = lowest.min(s);
            }
        }
        lowest
    }

    #[test]
    fn channel_smoothing_washes_out_between_colours_and_colour_smoothing_does_not() {
        let pairs = [
            ([0.0, 0.0, 255.0], [255.0, 224.0, 0.0]), // blue to yellow
            ([255.0, 0.0, 0.0], [0.0, 255.0, 255.0]), // red to cyan
            ([255.0, 0.0, 0.0], [0.0, 255.0, 0.0]),
            ([0.0, 0.0, 255.0], [255.0, 0.0, 0.0]),
        ];
        for (from, to) in pairs {
            let mut old = Smoother::new(30.0, 150.0);
            let mut new = ColourSmoother::new(30.0, 150.0);
            let washed = lowest_saturation(|q| old.step(q, 20.0), from, to);
            let kept = lowest_saturation(|q| new.step(q, 20.0), from, to);
            assert!(
                kept > 0.9,
                "{from:?} to {to:?}: the colour smoother stays saturated ({kept:.2})"
            );
            assert!(
                washed <= kept + 1e-9,
                "{from:?} to {to:?}: channel smoothing is never better ({washed:.2} against {kept:.2})"
            );
        }
        // and for colours far apart on the wheel it washes out badly
        let mut cyan = Smoother::new(30.0, 150.0);
        assert!(
            lowest_saturation(
                |q| cyan.step(q, 20.0),
                [255.0, 0.0, 0.0],
                [0.0, 255.0, 255.0]
            ) < 0.6
        );
        // the clearest case of all: blue to yellow washes out badly with independent channels
        let mut old = Smoother::new(30.0, 150.0);
        assert!(
            lowest_saturation(
                |q| old.step(q, 20.0),
                [0.0, 0.0, 255.0],
                [255.0, 224.0, 0.0]
            ) < 0.6
        );
    }

    #[test]
    fn the_colour_smoother_arrives_follows_brightness_and_ignores_greys() {
        let mut s = ColourSmoother::new(30.0, 150.0);
        assert_eq!(
            s.step([10.0, 20.0, 30.0, 40.0], 20.0),
            [10.0, 20.0, 30.0, 40.0],
            "the first call is the target"
        );
        let mut q = [0.0; 4];
        for _ in 0..100 {
            q = s.step([200.0, 50.0, 100.0, 80.0], 20.0);
        }
        assert!(
            q.iter()
                .zip([200.0, 50.0, 100.0, 80.0])
                .all(|(a, b)| (a - b).abs() < 1.5),
            "arrives: {q:?}"
        );
        // a grey target does not swing the hue: it fades the colour toward grey in place
        let mut g = ColourSmoother::new(30.0, 150.0);
        g.step([255.0, 0.0, 0.0, 100.0], 20.0);
        let mid = g.step([255.0, 255.0, 255.0, 100.0], 20.0);
        assert!(
            mid[0] >= mid[1] && mid[0] >= mid[2],
            "still reddish on the way to white: {mid:?}"
        );
        // brightness rises faster than it falls
        let mut b = ColourSmoother::new(30.0, 150.0);
        b.step([255.0, 255.0, 255.0, 0.0], 20.0);
        let up = b.step([255.0, 255.0, 255.0, 100.0], 20.0)[3];
        let mut c = ColourSmoother::new(30.0, 150.0);
        c.step([255.0, 255.0, 255.0, 100.0], 20.0);
        let down = 100.0 - c.step([255.0, 255.0, 255.0, 0.0], 20.0)[3];
        assert!(up > down * 2.0, "attack {up} against release {down}");
    }

    #[test]
    fn calibration_identity_and_curve() {
        let id = Calibration::default();
        assert!((0..=100)
            .all(|i| (id.apply(f64::from(i) / 100.0) - f64::from(i) / 100.0).abs() < 1e-12));
        let g = Calibration { gamma: 2.2 };
        assert_eq!(g.apply(0.0), 0.0);
        assert_eq!(g.apply(1.0), 1.0);
        assert!(g.apply(0.5) < 0.25);
        assert_eq!(g.apply(7.0), 1.0, "clamped");
    }

    #[test]
    fn lightness_curve_is_monotonic_with_fixed_ends() {
        assert_eq!(lightness_to_luminance(0.0), 0.0);
        assert!((lightness_to_luminance(1.0) - 1.0).abs() < 1e-9);
        let mut prev = -1.0;
        for i in 0..=1000 {
            let y = lightness_to_luminance(f64::from(i) / 1000.0);
            assert!(y > prev - 1e-12, "not monotonic at {i}");
            prev = y;
        }
        assert!(
            (lightness_to_luminance(0.5) - 0.184).abs() < 0.01,
            "L*=50 is about 18% luminance"
        );
    }

    #[test]
    fn smoother_converges_and_is_asymmetric() {
        let mut s = Smoother::new(10.0, 200.0);
        assert_eq!(
            s.step([0.0; 4], 20.0),
            [0.0; 4],
            "first value passes straight through"
        );
        let up = s.step([100.0; 4], 20.0)[0];
        let mut slow = Smoother::new(200.0, 10.0);
        slow.step([0.0; 4], 20.0);
        let up_slow = slow.step([100.0; 4], 20.0)[0];
        assert!(
            up > up_slow * 3.0,
            "fast attack rises quicker: {up} vs {up_slow}"
        );
        let mut s = Smoother::new(50.0, 50.0);
        s.step([0.0; 4], 10.0);
        let mut v = 0.0;
        for _ in 0..200 {
            v = s.step([80.0; 4], 10.0)[0];
        }
        assert!((v - 80.0).abs() < 0.01);
        assert_eq!(Smoother::new(0.0, 0.0).step([5.0; 4], 10.0), [5.0; 4]);
    }

    #[test]
    fn dither_preserves_the_mean_and_stays_in_range() {
        let mut d = Dither::new();
        let (mut sum, n) = (0.0, 2000);
        for _ in 0..n {
            let q = d.quantize([100.3, 0.4, 254.9, 12.5], [255, 255, 255, 100]);
            sum += f64::from(q[0]);
            assert!(q[3] <= 100);
        }
        assert!(
            (sum / f64::from(n) - 100.3).abs() < 0.01,
            "mean {}",
            sum / f64::from(n)
        );
    }

    #[test]
    fn dither_never_leaves_the_range_even_when_clipped() {
        let mut d = Dither::new();
        for _ in 0..100 {
            let q = d.quantize(
                [-50.0, 900.0, f64::from(u8::MAX), 0.0],
                [255, 255, 255, 100],
            );
            assert_eq!([q[0], q[1], q[2]], [0, 255, 255]);
        }
        // after being clipped for a long time the error must not have wound up
        let q = d.quantize([10.0, 10.0, 10.0, 10.0], [255, 255, 255, 100]);
        assert!(q.iter().all(|v| (9..=11).contains(v)), "{q:?}");
    }

    #[test]
    fn low_levels_are_resolved_over_time() {
        // 0.25 of a device step cannot be shown in one frame, but one frame in four should light up
        let mut d = Dither::new();
        let lit = (0..400)
            .filter(|_| d.quantize([0.0, 0.0, 0.0, 0.25], [255, 255, 255, 100])[3] == 1)
            .count();
        assert!((95..=105).contains(&lit), "{lit}");
    }
}
