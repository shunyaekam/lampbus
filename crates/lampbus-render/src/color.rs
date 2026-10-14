//! Colour parsing and conversion. Channels are `f64` in `0.0..=255.0` unless a function says otherwise.

use crate::{err, Error};
use serde_json::Value;

/// An RGB colour, each channel `0.0..=255.0`.
pub type Rgb = [f64; 3];

const NAMED: [(&str, Rgb); 14] = [
    ("red", [255., 0., 0.]),
    ("orange", [255., 110., 0.]),
    ("amber", [255., 150., 20.]),
    ("yellow", [255., 210., 0.]),
    ("green", [0., 255., 0.]),
    ("cyan", [0., 255., 255.]),
    ("blue", [0., 0., 255.]),
    ("purple", [140., 0., 255.]),
    ("magenta", [255., 0., 200.]),
    ("pink", [255., 80., 140.]),
    ("white", [255., 255., 255.]),
    ("warm", [255., 147., 41.]),
    ("candle", [255., 120., 20.]),
    ("cool", [200., 225., 255.]),
];

/// Parse `"#rrggbb"`, a colour name, `[r, g, b]`, or `{"h": 0-360, "s": 0-100, "v": 0-100}`.
///
/// # Errors
/// Returns an [`Error`] describing what was wrong.
pub fn parse(v: &Value) -> Result<Rgb, Error> {
    let bad = || Error(format!("bad colour {v}"));
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
            Ok([
                f64::from((n >> 16) & 255),
                f64::from((n >> 8) & 255),
                f64::from(n & 255),
            ])
        }
        Value::Array(a) if a.len() == 3 => {
            let mut out = [0.0; 3];
            for (slot, x) in out.iter_mut().zip(a) {
                *slot = x.as_f64().ok_or_else(bad)?.clamp(0.0, 255.0);
            }
            Ok(out)
        }
        Value::Object(o) => {
            let num = |k: &str, d: Option<f64>| match o.get(k) {
                None => d.ok_or_else(bad),
                Some(x) => x.as_f64().ok_or_else(bad),
            };
            let (h, s, vv) = (
                num("h", None)?,
                num("s", Some(100.0))?,
                num("v", Some(100.0))?,
            );
            let [r, g, b] = hsv_to_rgb(h.rem_euclid(360.0) / 360.0, s / 100.0, vv / 100.0);
            Ok([r * 255.0, g * 255.0, b * 255.0])
        }
        _ => err(format!("bad colour {v}")),
    }
}

/// HSV (`h`, `s`, `v` each in `0..=1`) to RGB (each `0..=1`).
pub fn hsv_to_rgb(h: f64, s: f64, v: f64) -> [f64; 3] {
    let (s, v) = (s.clamp(0.0, 1.0), v.clamp(0.0, 1.0));
    let h6 = h.rem_euclid(1.0) * 6.0;
    let i = h6.floor();
    let f = h6 - i;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    match i as u8 % 6 {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

/// RGB (each `0..=1`) to HSV (each `0..=1`).
pub fn rgb_to_hsv(r: f64, g: f64, b: f64) -> [f64; 3] {
    let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
    let d = mx - mn;
    let h = if d == 0.0 {
        0.0
    } else if mx == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if mx == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    [h / 6.0, if mx == 0.0 { 0.0 } else { d / mx }, mx]
}

/// sRGB-encoded value (`0..=1`) to linear light (`0..=1`).
pub fn srgb_to_linear(v: f64) -> f64 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear light (`0..=1`) to sRGB-encoded value (`0..=1`).
pub fn linear_to_srgb(v: f64) -> f64 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// The space in which colours are interpolated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Space {
    /// Interpolate in linear light (physically correct mixing). The default.
    #[default]
    Linear,
    /// Interpolate the encoded values directly.
    Srgb,
    /// Interpolate hue, saturation and value, taking the short way round the hue wheel.
    Hsv,
}

/// Interpolate from `a` to `b` by `u` in `0..=1`.
pub fn mix(a: Rgb, b: Rgb, u: f64, space: Space) -> Rgb {
    match space {
        Space::Srgb => [
            lerp(a[0], b[0], u),
            lerp(a[1], b[1], u),
            lerp(a[2], b[2], u),
        ],
        Space::Linear => {
            let f = |x: f64, y: f64| {
                let (lx, ly) = (srgb_to_linear(x / 255.0), srgb_to_linear(y / 255.0));
                linear_to_srgb(lerp(lx, ly, u)) * 255.0
            };
            [f(a[0], b[0]), f(a[1], b[1]), f(a[2], b[2])]
        }
        Space::Hsv => {
            let [ha, sa, va] = rgb_to_hsv(a[0] / 255.0, a[1] / 255.0, a[2] / 255.0);
            let [hb, sb, vb] = rgb_to_hsv(b[0] / 255.0, b[1] / 255.0, b[2] / 255.0);
            let dh = (hb - ha + 0.5).rem_euclid(1.0) - 0.5;
            let [r, g, bl] = hsv_to_rgb(
                (ha + dh * u).rem_euclid(1.0),
                lerp(sa, sb, u),
                lerp(va, vb, u),
            );
            [r * 255.0, g * 255.0, bl * 255.0]
        }
    }
}

/// Linear interpolation.
pub fn lerp(a: f64, b: f64, u: f64) -> f64 {
    a + (b - a) * u
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn parses_every_form() {
        assert_eq!(parse(&json!("#ff8800")).unwrap(), [255., 136., 0.]);
        assert_eq!(parse(&json!("WARM")).unwrap(), [255., 147., 41.]);
        assert_eq!(parse(&json!([1, 2, 300])).unwrap(), [1., 2., 255.]);
        let c = parse(&json!({"h": 120, "s": 100, "v": 100})).unwrap();
        assert!(close(c[1], 255.0, 0.5) && close(c[0], 0.0, 0.5));
        for bad in [
            json!("nope"),
            json!("#12"),
            json!([1, 2]),
            json!({"s": 5}),
            json!(5),
            json!(null),
        ] {
            assert!(parse(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn srgb_linear_round_trip() {
        for i in 0..=255 {
            let v = f64::from(i) / 255.0;
            assert!(close(linear_to_srgb(srgb_to_linear(v)), v, 1e-9), "{i}");
        }
        assert!(close(srgb_to_linear(0.5), 0.2140, 1e-3));
    }

    #[test]
    fn hsv_round_trip() {
        for h in 0..36 {
            let [r, g, b] = hsv_to_rgb(f64::from(h) / 36.0, 0.8, 0.9);
            let [h2, s2, v2] = rgb_to_hsv(r, g, b);
            assert!(
                close(h2, f64::from(h) / 36.0, 1e-9)
                    && close(s2, 0.8, 1e-9)
                    && close(v2, 0.9, 1e-9)
            );
        }
    }

    #[test]
    fn mixing_endpoints_and_midpoints() {
        let (a, b) = ([0., 0., 0.], [255., 255., 255.]);
        for sp in [Space::Linear, Space::Srgb, Space::Hsv] {
            assert!(
                mix(a, b, 0.0, sp).iter().all(|c| close(*c, 0.0, 1e-6)),
                "{sp:?}"
            );
            assert!(
                mix(a, b, 1.0, sp).iter().all(|c| close(*c, 255.0, 1e-6)),
                "{sp:?}"
            );
        }
        // linear-light mixing of black and white is brighter in encoded terms than plain averaging
        assert!(mix(a, b, 0.5, Space::Linear)[0] > mix(a, b, 0.5, Space::Srgb)[0]);
        // hsv takes the short way round: 350 degrees to 10 degrees passes through 0, not 180
        let red_ish = mix(
            parse(&json!({"h": 350})).unwrap(),
            parse(&json!({"h": 10})).unwrap(),
            0.5,
            Space::Hsv,
        );
        assert!(
            red_ish[0] > 250.0 && red_ish[1] < 5.0 && red_ish[2] < 5.0,
            "{red_ish:?}"
        );
    }
}
