//! Any JSON as a colour: parsing must never panic, and a parsed colour is always within 0..=255 per channel.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(data) else { return };
    if let Ok(c) = lampbus_render::color::parse(&v) {
        assert!(c.iter().all(|x| x.is_finite() && *x >= 0.0 && *x <= 255.001), "{c:?}");
    }
});
