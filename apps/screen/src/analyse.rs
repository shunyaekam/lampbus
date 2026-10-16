//! Reduce a screenshot to the three numbers a lamp can show (principle P1: reduce where the data lives).
//!
//! A 4K frame is about 33 MB; the lamp takes about 31 bits per update. We sample a coarse grid, average in *linear
//! light* (averaging encoded values darkens mixed colours), and split the result into a full-chroma colour and a
//! separate intensity, because the lamp has a separate brightness dimension and a dark blue scene should be a dim
//! saturated blue, not a muddy grey.

use lampbus_render::color::{linear_to_srgb, rgb_to_hsv, srgb_to_linear};

/// What to send to the lamp.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    /// Hue and saturation at full value.
    pub rgb: [u8; 3],
    /// Intensity percent, not rounded: a slow change must not step in whole percents.
    pub level: f64,
}

/// How pixels are combined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Plain average.
    Mean,
    /// Weight colourful, bright pixels more, so a grey screen with a coloured accent shows the accent.
    Vivid,
}

fn lut() -> [f64; 256] {
    let mut t = [0.0; 256];
    for (i, v) in t.iter_mut().enumerate() {
        *v = srgb_to_linear(i as f64 / 255.0);
    }
    t
}

/// A 32-bit-per-pixel image in memory.
#[derive(Clone, Copy)]
pub struct Image<'a> {
    pub px: &'a [u8],
    pub width: usize,
    pub height: usize,
    /// Bytes per row (may exceed `width * 4`).
    pub stride: usize,
    /// Byte order: macOS captures are BGRA.
    pub bgra: bool,
}

/// How to analyse it.
#[derive(Clone, Copy)]
pub struct Params {
    pub mode: Mode,
    /// Sample columns and rows.
    pub grid: (usize, usize),
    /// Intensity percent of a black screen.
    pub min_level: u8,
}

/// Analyse an image. Returns `None` for an empty or inconsistent one.
pub fn analyse(img: &Image, p: &Params) -> Option<Frame> {
    let Image {
        px,
        width,
        height,
        stride,
        bgra,
    } = *img;
    let Params {
        mode,
        grid,
        min_level,
    } = *p;
    if width == 0
        || height == 0
        || stride < width * 4
        || px.len() < stride * (height - 1) + width * 4
    {
        return None;
    }
    let (gx, gy) = (grid.0.clamp(1, width), grid.1.clamp(1, height));
    let table = lut();
    let (mut sum, mut wsum) = ([0.0f64; 3], 0.0f64);
    for j in 0..gy {
        let y = (j * 2 + 1) * height / (gy * 2);
        for i in 0..gx {
            let x = (i * 2 + 1) * width / (gx * 2);
            let o = y * stride + x * 4;
            let (c0, c1, c2) = (*px.get(o)?, *px.get(o + 1)?, *px.get(o + 2)?);
            let (r, g, b) = if bgra { (c2, c1, c0) } else { (c0, c1, c2) };
            let w = match mode {
                Mode::Mean => 1.0,
                Mode::Vivid => {
                    let [_, s, v] = rgb_to_hsv(
                        f64::from(r) / 255.0,
                        f64::from(g) / 255.0,
                        f64::from(b) / 255.0,
                    );
                    0.02 + s * v
                }
            };
            for (acc, c) in sum.iter_mut().zip([r, g, b]) {
                *acc += w * table.get(usize::from(c)).copied().unwrap_or(0.0);
            }
            wsum += w;
        }
    }
    let lin = [sum[0] / wsum, sum[1] / wsum, sum[2] / wsum];
    let y = 0.2126 * lin[0] + 0.7152 * lin[1] + 0.0722 * lin[2];
    // CIE lightness: how bright it looks, 0..1
    let lightness = if y > 0.008_856 {
        (116.0 * y.cbrt() - 16.0) / 100.0
    } else {
        903.3 * y / 100.0
    };
    let enc = [
        linear_to_srgb(lin[0]),
        linear_to_srgb(lin[1]),
        linear_to_srgb(lin[2]),
    ];
    let peak = enc[0].max(enc[1]).max(enc[2]);
    // full chroma: scale so the strongest channel is 255 (a black screen has no hue, so it stays white-ish and dim)
    let norm = if peak > 0.0 { 1.0 / peak } else { 1.0 };
    let rgb = enc.map(|c| (c * norm * 255.0).round().clamp(0.0, 255.0) as u8);
    let level = f64::from(min_level) + (100.0 - f64::from(min_level)) * lightness.clamp(0.0, 1.0);
    Some(Frame { rgb, level })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::too_many_arguments)]
    fn analyse(
        px: &[u8],
        width: usize,
        height: usize,
        stride: usize,
        bgra: bool,
        mode: Mode,
        grid: (usize, usize),
        min_level: u8,
    ) -> Option<Frame> {
        super::analyse(
            &Image {
                px,
                width,
                height,
                stride,
                bgra,
            },
            &Params {
                mode,
                grid,
                min_level,
            },
        )
    }

    fn solid(w: usize, h: usize, bgra: [u8; 4]) -> Vec<u8> {
        bgra.iter().copied().cycle().take(w * h * 4).collect()
    }

    #[test]
    fn solid_colours() {
        let red = analyse(
            &solid(64, 36, [0, 0, 255, 255]),
            64,
            36,
            256,
            true,
            Mode::Mean,
            (8, 4),
            10,
        )
        .unwrap();
        assert_eq!(red.rgb, [255, 0, 0]);
        // Y(red) = 0.2126 -> CIE L* = 53.2 -> 10 + 90 * 0.532 = 57.9
        assert!(
            (57.0..=59.0).contains(&red.level),
            "red is mid-bright: {}",
            red.level
        );
        let white = analyse(
            &solid(64, 36, [255; 4]),
            64,
            36,
            256,
            true,
            Mode::Mean,
            (8, 4),
            10,
        )
        .unwrap();
        assert_eq!((white.rgb, white.level), ([255, 255, 255], 100.0));
        let black = analyse(
            &solid(64, 36, [0, 0, 0, 255]),
            64,
            36,
            256,
            true,
            Mode::Mean,
            (8, 4),
            10,
        )
        .unwrap();
        assert!(
            (black.level - 10.0).abs() < 1e-9,
            "black sits at the minimum level, not off"
        );
    }

    #[test]
    fn dark_saturated_scenes_stay_saturated_and_dim() {
        // dark blue (BGRA): the colour must stay a pure blue, the level low
        let f = analyse(
            &solid(32, 18, [80, 0, 0, 255]),
            32,
            18,
            128,
            true,
            Mode::Mean,
            (4, 4),
            5,
        )
        .unwrap();
        assert_eq!(f.rgb, [0, 0, 255]);
        assert!(f.level < 25.0, "{}", f.level);
    }

    #[test]
    fn averaging_happens_in_linear_light() {
        // half red, half blue: linear average (0.5, 0, 0.5) encodes to about 188, not 128
        let (w, h) = (64, 8);
        let mut px = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let o = (y * w + x) * 4;
                if x < w / 2 {
                    px[o + 2] = 255
                } else {
                    px[o] = 255
                }
                px[o + 3] = 255;
            }
        }
        let f = analyse(&px, w, h, w * 4, true, Mode::Mean, (16, 2), 10).unwrap();
        assert_eq!(
            (f.rgb[0], f.rgb[1], f.rgb[2]),
            (255, 0, 255),
            "normalised to full chroma"
        );
        let raw = analyse(&px, w, h, w * 4, true, Mode::Mean, (16, 2), 10).unwrap();
        assert_eq!(raw, f);
    }

    #[test]
    fn vivid_mode_finds_the_accent_on_a_grey_screen() {
        let (w, h) = (64, 36);
        let mut px = solid(w, h, [128, 128, 128, 255]);
        for y in 10..26 {
            for x in 20..44 {
                let o = (y * w + x) * 4;
                px[o..o + 4].copy_from_slice(&[0, 40, 255, 255]); // a strong red-orange patch
            }
        }
        let mean = analyse(&px, w, h, w * 4, true, Mode::Mean, (32, 18), 10).unwrap();
        let vivid = analyse(&px, w, h, w * 4, true, Mode::Vivid, (32, 18), 10).unwrap();
        let sat = |f: Frame| f.rgb.iter().max().unwrap_or(&0) - f.rgb.iter().min().unwrap_or(&0);
        assert!(
            sat(vivid) > sat(mean) + 40,
            "vivid {:?} should be more saturated than mean {:?}",
            vivid.rgb,
            mean.rgb
        );
        assert!(vivid.rgb[0] > vivid.rgb[2], "red-orange wins");
    }

    #[test]
    fn rows_may_be_padded_and_rgba_is_supported() {
        let (w, h, stride) = (10, 4, 64);
        let mut px = vec![9u8; stride * h]; // padding bytes are garbage that must be ignored
        for y in 0..h {
            for x in 0..w {
                let o = y * stride + x * 4;
                px[o..o + 4].copy_from_slice(&[255, 0, 0, 255]); // RGBA red
            }
        }
        let f = analyse(&px, w, h, stride, false, Mode::Mean, (5, 2), 10).unwrap();
        assert_eq!(f.rgb, [255, 0, 0]);
    }

    #[test]
    fn rejects_bad_images_instead_of_panicking() {
        assert!(analyse(&[], 0, 0, 0, true, Mode::Mean, (4, 4), 10).is_none());
        assert!(
            analyse(&[0; 16], 10, 10, 40, true, Mode::Mean, (4, 4), 10).is_none(),
            "buffer too small"
        );
        assert!(
            analyse(&[0; 400], 10, 10, 20, true, Mode::Mean, (4, 4), 10).is_none(),
            "stride smaller than a row"
        );
        assert!(
            analyse(&[0; 40], 10, 1, 40, true, Mode::Mean, (0, 0), 10).is_some(),
            "a zero grid is clamped to one sample"
        );
    }
}
