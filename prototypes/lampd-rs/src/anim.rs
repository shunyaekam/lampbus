//! Declarative animations: validated JSON in, (rgb, intensity) per timestamp out.
//! No code from an animation file is ever executed, so an agent can author them safely.
//!
//! Same spec as the Python build (see library/animations/*.json for examples):
//!   keyframes: {"type":"keyframes","duration":s,"loop":bool,"space":"rgb"|"hsv","then":"hold"|"off",
//!               "keys":[{"t":s,"color":..,"level":0-100,"ease":"linear"|"smooth"|"hold"}]}
//!   generator: {"type":"gen","gen":"rainbow"|"breathe"|"candle"|"aurora","params":{..},"duration":s?}

use serde_json::Value;
use std::f64::consts::PI;

pub type Rgb = [f64; 3];
const MAX_KEYS: usize = 500;
const MAX_DURATION: f64 = 86400.0;
const MAX_FPS: f64 = 40.0;

const NAMED: [(&str, Rgb); 14] = [
    ("red", [255., 0., 0.]), ("orange", [255., 110., 0.]), ("amber", [255., 150., 20.]),
    ("yellow", [255., 210., 0.]), ("green", [0., 255., 0.]), ("cyan", [0., 255., 255.]),
    ("blue", [0., 0., 255.]), ("purple", [140., 0., 255.]), ("magenta", [255., 0., 200.]),
    ("pink", [255., 80., 140.]), ("white", [255., 255., 255.]), ("warm", [255., 147., 41.]),
    ("candle", [255., 120., 20.]), ("cool", [200., 225., 255.]),
];

pub fn valid_name(n: &str) -> bool {
    let b = n.as_bytes();
    !b.is_empty() && b.len() <= 40
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-' || *c == b'_')
}

/// Accepts "#rrggbb", a colour name, [r,g,b], or {"h":0-360,"s":0-100,"v":0-100}.
pub fn parse_color(v: &Value) -> Result<Rgb, String> {
    let bad = || format!("bad colour {v}");
    match v {
        Value::String(s) => {
            if let Some((_, rgb)) = NAMED.iter().find(|(n, _)| n.eq_ignore_ascii_case(s)) {
                return Ok(*rgb);
            }
            let h = s.trim().trim_start_matches('#');
            if h.len() != 6 || !h.is_ascii() {
                return Err(bad());
            }
            let n = u32::from_str_radix(h, 16).map_err(|_| bad())?;
            Ok([((n >> 16) & 255) as f64, ((n >> 8) & 255) as f64, (n & 255) as f64])
        }
        Value::Array(a) if a.len() == 3 => {
            let mut out = [0.0; 3];
            for (i, x) in a.iter().enumerate() {
                out[i] = x.as_f64().ok_or_else(bad)?.clamp(0.0, 255.0);
            }
            Ok(out)
        }
        Value::Object(o) => {
            let g = |k: &str, d: f64| o.get(k).map_or(Some(d), |x| x.as_f64());
            let (h, s, vv) = (g("h", f64::NAN).ok_or_else(bad)?, g("s", 100.).ok_or_else(bad)?, g("v", 100.).ok_or_else(bad)?);
            if h.is_nan() {
                return Err(bad());
            }
            let (r, g2, b) = hsv_to_rgb(h.rem_euclid(360.0) / 360.0, s / 100.0, vv / 100.0);
            Ok([r * 255.0, g2 * 255.0, b * 255.0])
        }
        _ => Err(bad()),
    }
}

pub fn hsv_to_rgb(h: f64, s: f64, v: f64) -> (f64, f64, f64) {
    let s = s.clamp(0.0, 1.0);
    let v = v.clamp(0.0, 1.0);
    let i = (h.rem_euclid(1.0) * 6.0).floor();
    let f = h.rem_euclid(1.0) * 6.0 - i;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    match i as i32 % 6 {
        0 => (v, t, p), 1 => (q, v, p), 2 => (p, v, t), 3 => (p, q, v), 4 => (t, p, v), _ => (v, p, q),
    }
}

pub fn rgb_to_hsv(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
    let d = mx - mn;
    let h = if d == 0.0 { 0.0 } else if mx == r { ((g - b) / d).rem_euclid(6.0) } else if mx == g { (b - r) / d + 2.0 } else { (r - g) / d + 4.0 };
    (h / 6.0, if mx == 0.0 { 0.0 } else { d / mx }, mx)
}

fn lerp(a: f64, b: f64, u: f64) -> f64 {
    a + (b - a) * u
}

fn mix(a: Rgb, b: Rgb, u: f64, hsv: bool) -> Rgb {
    if hsv {
        let (ha, sa, va) = rgb_to_hsv(a[0] / 255.0, a[1] / 255.0, a[2] / 255.0);
        let (hb, sb, vb) = rgb_to_hsv(b[0] / 255.0, b[1] / 255.0, b[2] / 255.0);
        let dh = (hb - ha + 0.5).rem_euclid(1.0) - 0.5; // shortest way round the hue wheel
        let (r, g, bl) = hsv_to_rgb((ha + dh * u).rem_euclid(1.0), lerp(sa, sb, u), lerp(va, vb, u));
        [r * 255.0, g * 255.0, bl * 255.0]
    } else {
        [lerp(a[0], b[0], u), lerp(a[1], b[1], u), lerp(a[2], b[2], u)]
    }
}

/// Smooth pseudo-noise in [-1, 1] from incommensurate sines (deterministic, dependency-free).
fn noise(t: f64, seed: f64) -> f64 {
    (t + seed).sin() * 0.5 + (t * 2.31 + seed * 1.7).sin() * 0.3 + (t * 4.77 + seed * 2.9).sin() * 0.2
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

enum Kind {
    Keyframes { keys: Vec<Key>, hsv: bool },
    Rainbow { period: f64, sat: f64 },
    Breathe { rgb: Rgb, period: f64, lo: f64, hi: f64 },
    Candle { rgb: Rgb, depth: f64 },
    Aurora { palette: Vec<Rgb>, rate: f64 },
}

pub struct Animation {
    pub fps: f64,
    pub duration: Option<f64>,
    pub looping: bool,
    pub then_off: bool,
    kind: Kind,
}

fn field_f64(o: &Value, k: &str, d: f64) -> Result<f64, String> {
    match o.get(k) {
        None | Some(Value::Null) => Ok(d),
        Some(v) => v.as_f64().ok_or_else(|| format!("{k} must be a number")),
    }
}

pub fn load(spec: &Value) -> Result<Animation, String> {
    let obj = spec.as_object().ok_or("animation spec must be an object")?;
    let ty = obj.get("type").and_then(Value::as_str);
    let (kind, duration, looping) = match ty {
        Some("keyframes") => load_keyframes(spec)?,
        Some("gen") => load_gen(spec)?,
        _ => return Err("type must be 'keyframes' or 'gen'".into()),
    };
    let fps = field_f64(spec, "fps", 20.0)?;
    if !(0.1..=MAX_FPS).contains(&fps) {
        return Err(format!("fps must be 0.1-{MAX_FPS}"));
    }
    let then_off = match spec.get("then").and_then(Value::as_str).unwrap_or("hold") {
        "hold" => false,
        "off" => true,
        _ => return Err("then must be 'hold' or 'off'".into()),
    };
    Ok(Animation { fps, duration, looping, then_off, kind })
}

fn load_keyframes(spec: &Value) -> Result<(Kind, Option<f64>, bool), String> {
    let duration = field_f64(spec, "duration", 0.0)?;
    if !(duration > 0.0 && duration <= MAX_DURATION) {
        return Err(format!("duration must be in (0, {MAX_DURATION}] seconds"));
    }
    let looping = spec.get("loop").and_then(Value::as_bool).unwrap_or(false);
    let hsv = match spec.get("space").and_then(Value::as_str).unwrap_or("rgb") {
        "rgb" => false,
        "hsv" => true,
        _ => return Err("space must be 'rgb' or 'hsv'".into()),
    };
    let raw = spec.get("keys").and_then(Value::as_array).filter(|k| (2..=MAX_KEYS).contains(&k.len()))
        .ok_or_else(|| format!("keys must be a list of 2..{MAX_KEYS}"))?;
    let mut keys = Vec::with_capacity(raw.len() + 1);
    for k in raw {
        let t = k.get("t").and_then(Value::as_f64).ok_or("each key needs a numeric 't'")?;
        if !(0.0..=duration).contains(&t) {
            return Err(format!("key t={t} outside 0..duration"));
        }
        let ease = match k.get("ease").and_then(Value::as_str).unwrap_or("linear") {
            "linear" => Ease::Linear,
            "smooth" => Ease::Smooth,
            "hold" => Ease::Hold,
            e => return Err(format!("bad ease {e:?}")),
        };
        let level = field_f64(k, "level", 100.0)?;
        if !(0.0..=100.0).contains(&level) {
            return Err("level must be 0-100".into());
        }
        let rgb = parse_color(k.get("color").unwrap_or(&Value::String("#ffffff".into())))?;
        keys.push(Key { t, rgb, level: level / 100.0, ease });
    }
    keys.sort_by(|a, b| a.t.total_cmp(&b.t));
    if looping && keys.last().is_some_and(|k| k.t < duration) {
        let first = keys[0].clone();
        keys.push(Key { t: duration, ease: Ease::Linear, ..first });
    }
    Ok((Kind::Keyframes { keys, hsv }, Some(duration), looping))
}

fn load_gen(spec: &Value) -> Result<(Kind, Option<f64>, bool), String> {
    let params = spec.get("params").cloned().unwrap_or(Value::Null);
    let p = |k: &str, d: f64| field_f64(&params, k, d).map_err(|e| format!("bad params: {e}"));
    let color_of = |k: &str, d: &str| parse_color(params.get(k).unwrap_or(&Value::String(d.into())));
    let kind = match spec.get("gen").and_then(Value::as_str) {
        Some("rainbow") => Kind::Rainbow { period: p("period", 30.)?, sat: p("saturation", 100.)? / 100. },
        Some("breathe") => Kind::Breathe { rgb: color_of("color", "warm")?, period: p("period", 6.)?,
            lo: p("min", 15.)? / 100., hi: p("max", 100.)? / 100. },
        Some("candle") => Kind::Candle { rgb: color_of("color", "candle")?, depth: p("flicker", 35.)? / 100. },
        Some("aurora") => {
            let pal = match params.get("palette") {
                None | Some(Value::Null) => vec![Value::from("#00ff90"), Value::from("#0090ff"), Value::from("#8a2be2")],
                Some(Value::Array(a)) if (2..=12).contains(&a.len()) => a.clone(),
                _ => return Err("palette must be a list of 2..12 colours".into()),
            };
            Kind::Aurora { palette: pal.iter().map(parse_color).collect::<Result<_, _>>()?, rate: p("speed", 0.15)? }
        }
        _ => return Err("gen must be one of (rainbow, breathe, candle, aurora)".into()),
    };
    if let Kind::Rainbow { period, .. } | Kind::Breathe { period, .. } = &kind {
        if *period <= 0.0 {
            return Err("period must be > 0".into());
        }
    }
    let duration = match spec.get("duration") {
        None | Some(Value::Null) => None,
        Some(v) => {
            let d = v.as_f64().ok_or("duration must be a number")?;
            if !(d > 0.0 && d <= MAX_DURATION) {
                return Err("duration out of range".into());
            }
            Some(d)
        }
    };
    Ok((kind, duration, duration.is_none()))
}

impl Animation {
    /// Colour (0-255 per channel) and intensity (0-1, relative to master brightness) at time `t` seconds.
    pub fn sample(&self, t: f64) -> (Rgb, f64) {
        match &self.kind {
            Kind::Keyframes { keys, hsv } => {
                let dur = self.duration.unwrap_or(1.0);
                let t = if self.looping { t.rem_euclid(dur) } else { t };
                if t <= keys[0].t {
                    return (keys[0].rgb, keys[0].level);
                }
                for w in keys.windows(2) {
                    let (a, b) = (&w[0], &w[1]);
                    if t <= b.t {
                        let span = b.t - a.t;
                        let raw = if span > 0.0 { (t - a.t) / span } else { 1.0 };
                        let u = match a.ease { Ease::Hold => 0.0, Ease::Smooth => raw * raw * (3.0 - 2.0 * raw), Ease::Linear => raw };
                        return (mix(a.rgb, b.rgb, u, *hsv), lerp(a.level, b.level, u));
                    }
                }
                let last = &keys[keys.len() - 1];
                (last.rgb, last.level)
            }
            Kind::Rainbow { period, sat } => {
                let (r, g, b) = hsv_to_rgb((t / period).rem_euclid(1.0), *sat, 1.0);
                ([r * 255.0, g * 255.0, b * 255.0], 1.0)
            }
            Kind::Breathe { rgb, period, lo, hi } => {
                let k = (1.0 - (2.0 * PI * t / period).cos()) / 2.0;
                (*rgb, lerp(*lo, *hi, k))
            }
            Kind::Candle { rgb, depth } => {
                let n = (noise(t * 3.0, 1.3) + noise(t * 9.0, 4.1) * 0.5) / 1.5;
                let level = (0.72 + n * depth).clamp(0.05, 1.0);
                let cool = (1.0 - level) * 0.35; // dimmer flame drifts redder
                ([rgb[0], rgb[1] * (1.0 - cool), rgb[2] * (1.0 - cool)], level)
            }
            Kind::Aurora { palette, rate } => {
                let pos = (0.5 + 0.5 * noise(t * rate, 2.2)) * (palette.len() - 1) as f64;
                let i = (pos as usize).min(palette.len() - 2);
                let rgb = mix(palette[i], palette[i + 1], pos - i as f64, true);
                (rgb, 0.55 + 0.45 * (0.5 + 0.5 * noise(t * rate * 1.7, 7.7)))
            }
        }
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
    fn colour_parsing() {
        assert_eq!(parse_color(&json!("#ff8800")).unwrap(), [255., 136., 0.]);
        assert_eq!(parse_color(&json!("warm")).unwrap(), [255., 147., 41.]);
        assert_eq!(parse_color(&json!([1, 2, 3])).unwrap(), [1., 2., 3.]);
        assert!(parse_color(&json!("nope")).is_err());
        let c = parse_color(&json!({"h": 120, "s": 100, "v": 100})).unwrap();
        assert!(close(c[0], 0.) && close(c[1], 255.) && close(c[2], 0.));
    }

    #[test]
    fn keyframes_interpolate_and_loop() {
        let a = load(&json!({"type":"keyframes","duration":10,"loop":true,
            "keys":[{"t":0,"color":"#000000","level":0},{"t":5,"color":"#ff0000","level":100}]})).unwrap();
        let (c, l) = a.sample(2.5);
        assert!(close(c[0], 127.5) && close(l, 0.5));
        let (c, _) = a.sample(5.0);
        assert!(close(c[0], 255.));
        let (c, _) = a.sample(7.5); // implicit return leg: red back to black
        assert!(close(c[0], 127.5));
        let (c, _) = a.sample(17.5); // wraps around the loop
        assert!(close(c[0], 127.5));
    }

    #[test]
    fn validation_rejects_bad_specs() {
        for bad in [
            json!({"type":"keyframes","duration":10,"keys":[{"t":0}]}),
            json!({"type":"keyframes","duration":99999999,"keys":[{"t":0},{"t":1}]}),
            json!({"type":"keyframes","duration":10,"keys":[{"t":0,"color":"nope"},{"t":5}]}),
            json!({"type":"gen","gen":"os.system"}),
            json!({"type":"gen","gen":"rainbow","fps":1000}),
            json!({"type":"exec","code":"x"}),
            json!([1, 2]),
        ] {
            assert!(load(&bad).is_err(), "accepted {bad}");
        }
    }

    #[test]
    fn generators_stay_in_range() {
        for g in [json!({"type":"gen","gen":"rainbow"}), json!({"type":"gen","gen":"breathe"}),
                  json!({"type":"gen","gen":"candle"}), json!({"type":"gen","gen":"aurora"})] {
            let a = load(&g).unwrap();
            for i in 0..2000 {
                let (c, l) = a.sample(i as f64 * 0.37);
                assert!(c.iter().all(|x| (0.0..=255.001).contains(x)) && (0.0..=1.0001).contains(&l), "{g} at {i}");
            }
        }
    }
}
