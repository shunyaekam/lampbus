//! The output pipeline on arbitrary floats: never panics, and the quantiser never leaves its range, however hostile
//! the input or long the run.
#![no_main]

use arbitrary::Arbitrary;
use lampbus_render::pipeline::{lightness_to_luminance, Calibration, Dither, Smoother};
use libfuzzer_sys::fuzz_target;

#[derive(Arbitrary, Debug)]
struct Input {
    frames: Vec<([f64; 4], f64)>,
    attack: f64,
    release: f64,
    gamma: f64,
    max: [u8; 4],
}

fuzz_target!(|i: Input| {
    let mut smoother = Smoother::new(i.attack, i.release);
    let mut dither = Dither::new();
    let cal = Calibration { gamma: i.gamma };
    for (v, dt) in i.frames.iter().take(512) {
        let s = smoother.step(*v, *dt);
        let q = dither.quantize(s, i.max);
        for (x, m) in q.iter().zip(i.max) {
            assert!(*x <= m);
        }
        let _ = cal.apply(v[0]);
        let _ = lightness_to_luminance(v[1]);
    }
});
