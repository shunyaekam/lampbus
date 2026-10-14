//! Every animation shipped in `library/animations` must load and stay in range over its whole duration.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use lampbus_render::anim;
use std::fs;
use std::path::PathBuf;

#[test]
fn every_library_animation_is_valid_and_in_range() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../library/animations");
    let mut n = 0;
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        assert!(anim::valid_name(&name), "{name}");
        let spec: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let a = anim::load(&spec).unwrap_or_else(|e| panic!("{name}: {e}"));
        let end = a.duration.unwrap_or(120.0);
        for i in 0..=400 {
            let s = a.sample(end * f64::from(i) / 400.0);
            assert!(s.rgb.iter().all(|c| (0.0..=255.001).contains(c)), "{name}");
            assert!((0.0..=1.0001).contains(&s.level), "{name}");
        }
        n += 1;
    }
    assert!(n >= 8, "found only {n} library animations");
}
