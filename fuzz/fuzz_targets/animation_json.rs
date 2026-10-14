//! Any JSON as an animation spec: loading must never panic, and a valid animation must sample to in-range values at
//! any time, including absurd ones.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(data) else { return };
    let Ok(a) = lampbus_render::anim::load(&v) else { return };
    assert!(a.fps >= 0.1 && a.fps <= lampbus_render::anim::MAX_FPS);
    for t in [0.0, 0.5, 1.0, 59.9, 3600.0, 1e9, -5.0, f64::MAX] {
        let s = a.sample(t);
        assert!(s.rgb.iter().all(|c| c.is_finite() && *c >= -1e-6 && *c <= 255.001), "rgb {:?} at {t}", s.rgb);
        assert!(s.level.is_finite() && s.level >= -1e-6 && s.level <= 1.0001, "level {} at {t}", s.level);
        let _ = a.finished(t);
    }
});
