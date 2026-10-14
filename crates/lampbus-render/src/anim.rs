//! The declarative animation format. Validated JSON in, `(colour, level)` per timestamp out.
//!
//! No code from an animation file is ever executed: an agent can author animations safely, and every animation is
//! validated against hard limits before it is accepted.
//!
//! ```text
//! keyframes: {"type":"keyframes","duration":s,"loop":bool,"space":"linear"|"srgb"|"hsv","then":"hold"|"off",
//!             "keys":[{"t":s,"color":..,"level":0-100,"ease":"linear"|"smooth"|"hold"}]}
//! generator: {"type":"gen","gen":"rainbow"|"breathe"|"candle"|"aurora","params":{..},"duration":s?}
//! common:    "fps": 0.1..=40
//! ```

use crate::color::{self, lerp, mix, Rgb, Space};
use crate::{err, Error};
use serde_json::Value;
use std::f64::consts::PI;

/// Most keyframes an animation may have.
pub const MAX_KEYS: usize = 500;
/// Longest an animation may run, in seconds.
pub const MAX_DURATION: f64 = 86_400.0;
/// Highest frame rate an animation may ask for.
pub const MAX_FPS: f64 = 40.0;

/// Names for library entries: lowercase letters, digits, `-` and `_`, starting with a letter or digit, max 40.
pub fn valid_name(n: &str) -> bool {
    n.len() <= 40
        && n.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && n.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// One sampled moment of an animation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    /// Colour, each channel `0.0..=255.0`.
    pub rgb: Rgb,
    /// Intensity `0.0..=1.0`, relative to the lamp's master brightness.
    pub level: f64,
}

#[derive(Clone, Copy)]
enum Ease {
    Linear,
    Smooth,
    Hold,
}

#[derive(Clone)]
struct Key {
    t: f64,
    rgb: Rgb,
    level: f64,
    ease: Ease,
}

#[derive(Clone)]
enum Kind {
    Keyframes {
        keys: Vec<Key>,
        space: Space,
    },
    Rainbow {
        period: f64,
        sat: f64,
    },
    Breathe {
        rgb: Rgb,
        period: f64,
        lo: f64,
        hi: f64,
    },
    Candle {
        rgb: Rgb,
        depth: f64,
    },
    Aurora {
        palette: Vec<Rgb>,
        rate: f64,
    },
}

/// A validated animation.
#[derive(Clone)]
pub struct Animation {
    /// Frames per second the animation wants to be sampled at.
    pub fps: f64,
    /// Length in seconds; `None` runs forever.
    pub duration: Option<f64>,
    /// Whether it wraps around at the end of `duration`.
    pub looping: bool,
    /// When a non-looping animation ends: switch the lamp off instead of holding the last frame.
    pub then_off: bool,
    kind: Kind,
}

fn num(v: &Value, k: &str, default: f64) -> Result<f64, Error> {
    match v.get(k) {
        None | Some(Value::Null) => Ok(default),
        Some(x) => x
            .as_f64()
            .filter(|n| n.is_finite())
            .ok_or_else(|| Error(format!("{k} must be a finite number"))),
    }
}

/// Validate a spec and build an [`Animation`].
///
/// # Errors
/// Returns an [`Error`] naming the first rule broken.
pub fn load(spec: &Value) -> Result<Animation, Error> {
    if !spec.is_object() {
        return err("animation spec must be an object");
    }
    let (kind, duration, looping) = match spec.get("type").and_then(Value::as_str) {
        Some("keyframes") => load_keyframes(spec)?,
        Some("gen") => load_gen(spec)?,
        _ => return err("type must be 'keyframes' or 'gen'"),
    };
    let fps = num(spec, "fps", 20.0)?;
    if !(0.1..=MAX_FPS).contains(&fps) {
        return err(format!("fps must be 0.1-{MAX_FPS}"));
    }
    let then_off = match spec.get("then").and_then(Value::as_str).unwrap_or("hold") {
        "hold" => false,
        "off" => true,
        _ => return err("then must be 'hold' or 'off'"),
    };
    Ok(Animation {
        fps,
        duration,
        looping,
        then_off,
        kind,
    })
}

fn load_keyframes(spec: &Value) -> Result<(Kind, Option<f64>, bool), Error> {
    let duration = num(spec, "duration", 0.0)?;
    if !(duration > 0.0 && duration <= MAX_DURATION) {
        return err(format!("duration must be in (0, {MAX_DURATION}] seconds"));
    }
    let looping = spec.get("loop").and_then(Value::as_bool).unwrap_or(false);
    let space = match spec
        .get("space")
        .and_then(Value::as_str)
        .unwrap_or("linear")
    {
        "linear" => Space::Linear,
        "srgb" | "rgb" => Space::Srgb,
        "hsv" => Space::Hsv,
        _ => return err("space must be 'linear', 'srgb' or 'hsv'"),
    };
    let raw = spec
        .get("keys")
        .and_then(Value::as_array)
        .filter(|k| (2..=MAX_KEYS).contains(&k.len()))
        .ok_or_else(|| Error(format!("keys must be a list of 2..{MAX_KEYS}")))?;
    let mut keys = Vec::with_capacity(raw.len() + 1);
    for k in raw {
        let t = k
            .get("t")
            .and_then(Value::as_f64)
            .ok_or_else(|| Error("each key needs a numeric 't'".into()))?;
        if !(0.0..=duration).contains(&t) {
            return err(format!("key t={t} outside 0..duration"));
        }
        let ease = match k.get("ease").and_then(Value::as_str).unwrap_or("linear") {
            "linear" => Ease::Linear,
            "smooth" => Ease::Smooth,
            "hold" => Ease::Hold,
            e => return err(format!("bad ease {e:?}")),
        };
        let level = num(k, "level", 100.0)?;
        if !(0.0..=100.0).contains(&level) {
            return err("level must be 0-100");
        }
        let rgb = color::parse(k.get("color").unwrap_or(&Value::String("#ffffff".into())))?;
        keys.push(Key {
            t,
            rgb,
            level: level / 100.0,
            ease,
        });
    }
    keys.sort_by(|a, b| a.t.total_cmp(&b.t));
    if looping {
        if let (Some(first), Some(last)) = (keys.first().cloned(), keys.last()) {
            if last.t < duration {
                keys.push(Key {
                    t: duration,
                    ease: Ease::Linear,
                    ..first
                });
            }
        }
    }
    Ok((Kind::Keyframes { keys, space }, Some(duration), looping))
}

fn load_gen(spec: &Value) -> Result<(Kind, Option<f64>, bool), Error> {
    let params = spec.get("params").cloned().unwrap_or(Value::Null);
    let p = |k: &str, d: f64| num(&params, k, d).map_err(|e| Error(format!("bad params: {e}")));
    let colour = |k: &str, d: &str| color::parse(params.get(k).unwrap_or(&Value::String(d.into())));
    let kind = match spec.get("gen").and_then(Value::as_str) {
        Some("rainbow") => Kind::Rainbow {
            period: p("period", 30.0)?,
            sat: p("saturation", 100.0)? / 100.0,
        },
        Some("breathe") => Kind::Breathe {
            rgb: colour("color", "warm")?,
            period: p("period", 6.0)?,
            lo: p("min", 15.0)? / 100.0,
            hi: p("max", 100.0)? / 100.0,
        },
        Some("candle") => Kind::Candle {
            rgb: colour("color", "candle")?,
            depth: p("flicker", 35.0)? / 100.0,
        },
        Some("aurora") => {
            let pal = match params.get("palette") {
                None | Some(Value::Null) => vec![
                    Value::from("#00ff90"),
                    Value::from("#0090ff"),
                    Value::from("#8a2be2"),
                ],
                Some(Value::Array(a)) if (2..=12).contains(&a.len()) => a.clone(),
                _ => return err("palette must be a list of 2..12 colours"),
            };
            Kind::Aurora {
                palette: pal.iter().map(color::parse).collect::<Result<_, _>>()?,
                rate: p("speed", 0.15)?,
            }
        }
        _ => return err("gen must be one of rainbow, breathe, candle, aurora"),
    };
    if let Kind::Rainbow { period, .. } | Kind::Breathe { period, .. } = &kind {
        if *period <= 0.0 {
            return err("period must be > 0");
        }
    }
    let duration = match spec.get("duration") {
        None | Some(Value::Null) => None,
        Some(v) => {
            let d = v
                .as_f64()
                .ok_or_else(|| Error("duration must be a number".into()))?;
            if !(d > 0.0 && d <= MAX_DURATION) {
                return err("duration out of range");
            }
            Some(d)
        }
    };
    Ok((kind, duration, duration.is_none()))
}

/// Smooth pseudo-noise in `[-1, 1]` from incommensurate sines (deterministic, no dependencies).
fn noise(t: f64, seed: f64) -> f64 {
    (t + seed).sin() * 0.5
        + (t * 2.31 + seed * 1.7).sin() * 0.3
        + (t * 4.77 + seed * 2.9).sin() * 0.2
}

impl Animation {
    /// True once a non-looping animation has run its full duration at time `t` seconds.
    pub fn finished(&self, t: f64) -> bool {
        !self.looping && self.duration.is_some_and(|d| t >= d)
    }

    /// The animation at time `t` seconds from its start. Non-looping animations hold their last frame after the end.
    pub fn sample(&self, t: f64) -> Sample {
        match &self.kind {
            Kind::Keyframes { keys, space } => {
                let dur = self.duration.unwrap_or(1.0);
                let t = if self.looping { t.rem_euclid(dur) } else { t };
                sample_keys(keys, *space, t)
            }
            Kind::Rainbow { period, sat } => {
                let [r, g, b] = color::hsv_to_rgb((t / period).rem_euclid(1.0), *sat, 1.0);
                Sample {
                    rgb: [r * 255.0, g * 255.0, b * 255.0],
                    level: 1.0,
                }
            }
            Kind::Breathe {
                rgb,
                period,
                lo,
                hi,
            } => {
                let k = (1.0 - (2.0 * PI * t / period).cos()) / 2.0;
                Sample {
                    rgb: *rgb,
                    level: lerp(*lo, *hi, k),
                }
            }
            Kind::Candle { rgb, depth } => {
                let n = (noise(t * 3.0, 1.3) + noise(t * 9.0, 4.1) * 0.5) / 1.5;
                let level = (0.72 + n * depth).clamp(0.05, 1.0);
                let cool = (1.0 - level) * 0.35; // a dimmer flame drifts redder
                Sample {
                    rgb: [rgb[0], rgb[1] * (1.0 - cool), rgb[2] * (1.0 - cool)],
                    level,
                }
            }
            Kind::Aurora { palette, rate } => {
                let span = palette.len().saturating_sub(1) as f64;
                let pos = (0.5 + 0.5 * noise(t * rate, 2.2)) * span;
                let i = (pos as usize).min(palette.len().saturating_sub(2));
                let (a, b) = (palette.get(i), palette.get(i + 1));
                let rgb = match (a, b) {
                    (Some(a), Some(b)) => mix(*a, *b, pos - i as f64, Space::Hsv),
                    _ => [0.0; 3],
                };
                Sample {
                    rgb,
                    level: 0.55 + 0.45 * (0.5 + 0.5 * noise(t * rate * 1.7, 7.7)),
                }
            }
        }
    }
}

fn sample_keys(keys: &[Key], space: Space, t: f64) -> Sample {
    let (Some(first), Some(last)) = (keys.first(), keys.last()) else {
        return Sample {
            rgb: [0.0; 3],
            level: 0.0,
        };
    };
    if t <= first.t {
        return Sample {
            rgb: first.rgb,
            level: first.level,
        };
    }
    for w in keys.windows(2) {
        if let [a, b] = w {
            if t <= b.t {
                let span = b.t - a.t;
                let raw = if span > 0.0 { (t - a.t) / span } else { 1.0 };
                let u = match a.ease {
                    Ease::Hold => 0.0,
                    Ease::Smooth => raw * raw * (3.0 - 2.0 * raw),
                    Ease::Linear => raw,
                };
                return Sample {
                    rgb: mix(a.rgb, b.rgb, u, space),
                    level: lerp(a.level, b.level, u),
                };
            }
        }
    }
    Sample {
        rgb: last.rgb,
        level: last.level,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1.0
    }

    #[test]
    fn keyframes_interpolate_loop_and_hold() {
        let a = load(
            &json!({"type":"keyframes","duration":10,"loop":true,"space":"srgb",
            "keys":[{"t":0,"color":"#000000","level":0},{"t":5,"color":"#ff0000","level":100}]}),
        )
        .unwrap();
        let s = a.sample(2.5);
        assert!(close(s.rgb[0], 127.5) && (s.level - 0.5).abs() < 1e-9);
        assert!(close(a.sample(5.0).rgb[0], 255.0));
        assert!(close(a.sample(7.5).rgb[0], 127.5), "implicit return leg");
        assert!(close(a.sample(17.5).rgb[0], 127.5), "wraps");
        let once =
            load(&json!({"type":"keyframes","duration":4,"keys":[{"t":0},{"t":4,"color":"red"}]}))
                .unwrap();
        assert!(!once.finished(3.9) && once.finished(4.0));
        assert!(
            close(once.sample(99.0).rgb[0], 255.0),
            "holds the last frame"
        );
    }

    #[test]
    fn linear_space_differs_from_srgb_space() {
        let spec = |space: &str| {
            json!({"type":"keyframes","duration":2,"space":space,
                "keys":[{"t":0,"color":"#000000"},{"t":2,"color":"#ffffff"}]})
        };
        let lin = load(&spec("linear")).unwrap().sample(1.0).rgb[0];
        let enc = load(&spec("srgb")).unwrap().sample(1.0).rgb[0];
        let alias = load(&spec("rgb")).unwrap().sample(1.0).rgb[0];
        assert!(lin > enc + 20.0 && (enc - alias).abs() < 1e-9);
    }

    #[test]
    fn validation_rejects_bad_specs() {
        for bad in [
            json!({"type":"keyframes","duration":10,"keys":[{"t":0}]}),
            json!({"type":"keyframes","duration":99999999,"keys":[{"t":0},{"t":1}]}),
            json!({"type":"keyframes","duration":10,"keys":[{"t":0,"color":"nope"},{"t":5}]}),
            json!({"type":"keyframes","duration":10,"space":"lab","keys":[{"t":0},{"t":5}]}),
            json!({"type":"keyframes","duration":10,"keys":[{"t":0},{"t":50}]}),
            json!({"type":"keyframes","duration":10,"keys":[{"t":0,"level":101},{"t":5}]}),
            json!({"type":"gen","gen":"os.system"}),
            json!({"type":"gen","gen":"rainbow","fps":1000}),
            json!({"type":"gen","gen":"rainbow","params":{"period":0}}),
            json!({"type":"gen","gen":"rainbow","then":"explode"}),
            json!({"type":"exec","code":"x"}),
            json!([1, 2]),
        ] {
            assert!(load(&bad).is_err(), "accepted {bad}");
        }
    }

    #[test]
    fn generators_stay_in_range_forever() {
        for g in [
            json!({"type":"gen","gen":"rainbow"}),
            json!({"type":"gen","gen":"breathe"}),
            json!({"type":"gen","gen":"candle"}),
            json!({"type":"gen","gen":"aurora"}),
        ] {
            let a = load(&g).unwrap();
            for i in 0..5000 {
                let s = a.sample(f64::from(i) * 0.37);
                assert!(
                    s.rgb.iter().all(|x| (0.0..=255.001).contains(x))
                        && (0.0..=1.0001).contains(&s.level),
                    "{g} at {i}"
                );
            }
        }
    }

    #[test]
    fn names() {
        assert!(valid_name("sunrise") && valid_name("a_b-9") && valid_name("9lives"));
        assert!(
            !valid_name("")
                && !valid_name("Up")
                && !valid_name("-x")
                && !valid_name("../etc")
                && !valid_name(&"a".repeat(41))
        );
    }
}
