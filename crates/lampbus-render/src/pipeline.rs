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
