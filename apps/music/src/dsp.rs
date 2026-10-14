//! Audio to light, as pure functions (principle P1: reduce where the data lives).
//!
//! 48,000 samples a second become one colour and one intensity, 20 times a second. The mapping is deliberately simple
//! and explainable:
//!
//! - **hue** follows the spectral balance: bass pushes toward red, mids toward green, treble toward blue/violet,
//!   smoothed so it drifts instead of flickering;
//! - **intensity** follows loudness relative to an automatic gain control (a slowly decaying running peak), so quiet
//!   and loud rooms both use the lamp's full range;
//! - **beats** come from spectral flux (the positive change of the low-frequency spectrum) against an adaptive
//!   threshold, and kick the intensity, which then decays.

use std::f64::consts::PI;

/// Samples per analysis window (a power of two).
pub const WINDOW: usize = 2048;

/// What to send to the lamp.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    /// Colour at full value.
    pub rgb: [u8; 3],
    /// Intensity percent.
    pub level: u8,
    /// A beat was detected in this window.
    pub beat: bool,
}

/// In-place radix-2 FFT. `re.len()` must equal `im.len()` and be a power of two (anything else is ignored).
pub fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    if n < 2 || !n.is_power_of_two() || im.len() != n {
        return;
    }
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * PI / len as f64;
        let (wr, wi) = (ang.cos(), ang.sin());
        let mut start = 0;
        while start < n {
            let (mut cr, mut ci) = (1.0, 0.0);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let (xr, xi) = (
                    re.get(b).copied().unwrap_or(0.0),
                    im.get(b).copied().unwrap_or(0.0),
                );
                let (tr, ti) = (xr * cr - xi * ci, xr * ci + xi * cr);
                let (ur, ui) = (
                    re.get(a).copied().unwrap_or(0.0),
                    im.get(a).copied().unwrap_or(0.0),
                );
                set(re, a, ur + tr);
                set(im, a, ui + ti);
                set(re, b, ur - tr);
                set(im, b, ui - ti);
                let ncr = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = ncr;
            }
            start += len;
        }
        len <<= 1;
    }
}

fn set(v: &mut [f64], i: usize, x: f64) {
    if let Some(s) = v.get_mut(i) {
        *s = x;
    }
}

/// Stateful analyser: feed it the most recent [`WINDOW`] mono samples at a steady rate.
pub struct Analyzer {
    sample_rate: f64,
    window: Vec<f64>,
    prev_low: Vec<f64>,
    flux_mean: f64,
    flux_var: f64,
    peak_db: f64,
    hue: f64,
    beat_env: f64,
    level: f64,
}

impl Analyzer {
    pub fn new(sample_rate: f64) -> Analyzer {
        let window = (0..WINDOW)
            .map(|i| 0.5 - 0.5 * (2.0 * PI * i as f64 / (WINDOW - 1) as f64).cos())
            .collect();
        Analyzer {
            sample_rate,
            window,
            prev_low: Vec::new(),
            flux_mean: 0.0,
            flux_var: 0.0,
            peak_db: -60.0,
            hue: 0.0,
            beat_env: 0.0,
            level: 0.0,
        }
    }

    fn bin_range(&self, lo_hz: f64, hi_hz: f64) -> (usize, usize) {
        let hz = self.sample_rate / WINDOW as f64;
        (
            ((lo_hz / hz).ceil() as usize).max(1),
            ((hi_hz / hz).floor() as usize).min(WINDOW / 2 - 1),
        )
    }

    /// Analyse one window. `dt_ms` is the time since the previous call (for the smoothing filters).
    pub fn analyse(&mut self, samples: &[f32], dt_ms: f64) -> Frame {
        let mut re: Vec<f64> = (0..WINDOW)
            .map(|i| {
                f64::from(samples.get(i).copied().unwrap_or(0.0))
                    * self.window.get(i).copied().unwrap_or(0.0)
            })
            .collect();
        let mut im = vec![0.0; WINDOW];
        fft(&mut re, &mut im);
        let mag: Vec<f64> = re
            .iter()
            .zip(&im)
            .take(WINDOW / 2)
            .map(|(r, i)| (r * r + i * i).sqrt() / WINDOW as f64)
            .collect();

        let band = |lo: f64, hi: f64| {
            let (a, b) = self.bin_range(lo, hi);
            mag.get(a..=b.max(a))
                .map_or(0.0, |s| s.iter().map(|m| m * m).sum::<f64>())
        };
        let (bass, mid, high) = (
            band(30.0, 200.0),
            band(200.0, 2000.0),
            band(2000.0, 10000.0),
        );
        let total = bass + mid + high;
        let rms_db = 10.0 * (total.max(1e-12)).log10();

        // automatic gain control: the peak rises at once and decays about 6 dB per 10 s
        self.peak_db = self.peak_db.max(rms_db) - 6.0 * dt_ms / 10_000.0;
        let loud = ((rms_db - (self.peak_db - 40.0)) / 40.0).clamp(0.0, 1.0);

        // hue: red (bass) -> green (mid) -> blue/violet (treble), smoothed
        let target = if total > 1e-10 {
            (mid * 120.0 + high * 260.0) / total
        } else {
            self.hue
        };
        let alpha = 1.0 - (-dt_ms / 400.0).exp();
        self.hue += (target - self.hue) * alpha;

        // beat: spectral flux of the low end against an adaptive threshold (mean + 1.5 sigma)
        let (a, b) = self.bin_range(30.0, 600.0);
        let low: Vec<f64> = mag
            .get(a..=b.max(a))
            .map(<[f64]>::to_vec)
            .unwrap_or_default();
        let flux: f64 = if self.prev_low.len() == low.len() {
            low.iter()
                .zip(&self.prev_low)
                .map(|(x, p)| (x - p).max(0.0))
                .sum()
        } else {
            0.0
        };
        self.prev_low = low;
        let sigma = self.flux_var.sqrt();
        let beat = flux > self.flux_mean + 1.5 * sigma && flux > 1e-4 && loud > 0.15;
        let k = 1.0 - (-dt_ms / 1500.0).exp();
        self.flux_var += ((flux - self.flux_mean).powi(2) - self.flux_var) * k;
        self.flux_mean += (flux - self.flux_mean) * k;

        self.beat_env = if beat {
            1.0
        } else {
            self.beat_env * (-dt_ms / 180.0).exp()
        };
        let target_level = (0.30 + 0.70 * loud).max(self.beat_env) * loud.max(0.05);
        self.level += (target_level - self.level) * (1.0 - (-dt_ms / 60.0).exp());

        let [r, g, bl] = lampbus_render_hsv(self.hue, 1.0);
        Frame {
            rgb: [r, g, bl],
            level: (8.0 + 92.0 * self.level).round().clamp(0.0, 100.0) as u8,
            beat,
        }
    }
}

/// HSV (hue in degrees, full saturation) to RGB bytes, without depending on other crates.
fn lampbus_render_hsv(hue_deg: f64, v: f64) -> [u8; 3] {
    let h6 = (hue_deg.rem_euclid(360.0)) / 60.0;
    let (i, f) = (h6.floor(), h6 - h6.floor());
    let (p, q, t) = (0.0, v * (1.0 - f), v * f);
    let (r, g, b) = match i as u32 % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    [r, g, b].map(|c| (c * 255.0).round() as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f64, amp: f64, n: usize, rate: f64, offset: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (amp * (2.0 * PI * freq * (i + offset) as f64 / rate).sin()) as f32)
            .collect()
    }

    #[test]
    fn fft_finds_a_sine_in_the_right_bin_and_obeys_parseval() {
        let n = 256;
        let mut re: Vec<f64> = (0..n)
            .map(|i| (2.0 * PI * 16.0 * i as f64 / n as f64).sin())
            .collect();
        let mut im = vec![0.0; n];
        let energy_time: f64 = re.iter().map(|x| x * x).sum();
        fft(&mut re, &mut im);
        let mags: Vec<f64> = re
            .iter()
            .zip(&im)
            .map(|(r, i)| (r * r + i * i).sqrt())
            .collect();
        let peak = mags
            .iter()
            .take(n / 2)
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, _)| i);
        assert_eq!(peak, Some(16));
        let energy_freq: f64 = mags.iter().map(|m| m * m).sum::<f64>() / n as f64;
        assert!(
            (energy_time - energy_freq).abs() < 1e-6,
            "Parseval: {energy_time} vs {energy_freq}"
        );
        // an impulse has a flat spectrum
        let mut re = vec![0.0; 8];
        re[0] = 1.0;
        let mut im = vec![0.0; 8];
        fft(&mut re, &mut im);
        assert!(re.iter().all(|x| (x - 1.0).abs() < 1e-12) && im.iter().all(|x| x.abs() < 1e-12));
        // bad lengths are ignored, not panics
        let (mut a, mut b) = (vec![1.0; 6], vec![0.0; 6]);
        fft(&mut a, &mut b);
        assert_eq!(a, vec![1.0; 6]);
    }

    fn settle(a: &mut Analyzer, signal: impl Fn(usize) -> Vec<f32>, frames: usize) -> Frame {
        let mut last = Frame {
            rgb: [0; 3],
            level: 0,
            beat: false,
        };
        for k in 0..frames {
            last = a.analyse(&signal(k), 50.0);
        }
        last
    }

    #[test]
    fn bass_is_red_and_treble_is_blue() {
        let rate = 48_000.0;
        let bass = settle(
            &mut Analyzer::new(rate),
            |k| sine(80.0, 0.5, WINDOW, rate, k * 2400),
            60,
        );
        assert!(
            bass.rgb[0] > 200 && bass.rgb[2] < 60,
            "bass should be red: {:?}",
            bass.rgb
        );
        let treble = settle(
            &mut Analyzer::new(rate),
            |k| sine(5000.0, 0.5, WINDOW, rate, k * 2400),
            60,
        );
        assert!(
            treble.rgb[2] > treble.rgb[0],
            "treble should lean blue/violet: {:?}",
            treble.rgb
        );
        assert!(
            bass.level > 40 && treble.level > 40,
            "a steady tone is lit: {} {}",
            bass.level,
            treble.level
        );
    }

    #[test]
    fn silence_is_dim_and_loudness_is_relative() {
        let rate = 44_100.0;
        let quiet = settle(&mut Analyzer::new(rate), |_| vec![0.0; WINDOW], 40);
        assert!(quiet.level <= 12 && !quiet.beat, "silence: {quiet:?}");
        // the same tone at 1% amplitude is still lit after the gain control adapts (a quiet room uses the full range)
        let mut a = Analyzer::new(rate);
        let soft = settle(&mut a, |k| sine(440.0, 0.01, WINDOW, rate, k * 2205), 400);
        assert!(soft.level > 30, "AGC should adapt: {soft:?}");
    }

    #[test]
    fn a_kick_drum_pattern_triggers_beats() {
        let rate = 48_000.0;
        let mut a = Analyzer::new(rate);
        let mut beats = 0;
        let mut kicks = 0;
        // 20 frames per second; a kick (a 60 Hz burst) every 10th frame over 20 s on top of quiet mid noise
        for k in 0..400usize {
            let kick = k % 10 == 0;
            if kick && k > 60 {
                kicks += 1;
            }
            let s: Vec<f32> = (0..WINDOW)
                .map(|i| {
                    let t = (k * 2400 + i) as f64 / rate;
                    let hum = 0.02 * (2.0 * PI * 700.0 * t).sin();
                    let thump = if kick {
                        0.8 * (2.0 * PI * 60.0 * t).sin() * (-((i as f64) / 600.0)).exp()
                    } else {
                        0.0
                    };
                    (hum + thump) as f32
                })
                .collect();
            if a.analyse(&s, 50.0).beat && k > 60 {
                beats += 1;
            }
        }
        assert!(
            beats >= kicks * 6 / 10 && beats <= kicks * 2,
            "{beats} beats for {kicks} kicks"
        );
    }

    #[test]
    fn hue_conversion() {
        assert_eq!(lampbus_render_hsv(0.0, 1.0), [255, 0, 0]);
        assert_eq!(lampbus_render_hsv(120.0, 1.0), [0, 255, 0]);
        assert_eq!(lampbus_render_hsv(240.0, 1.0), [0, 0, 255]);
    }
}
