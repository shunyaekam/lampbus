//! Definitions and the starter library compiled into the binary, so `lampbus` works with no files on disk.
//! Users extend both with directories (see the config file); user entries win over built-in ones.

/// Built-in lamp definitions.
pub const DEFINITIONS: &[&str] = &[
    include_str!("../../../definitions/elk-bledom.json"),
    include_str!("../../../definitions/govee-h61.json"),
    include_str!("../../../definitions/istrip-ssl.json"),
    include_str!("../../../definitions/triones.json"),
];

/// Built-in animations by name.
pub const ANIMATIONS: &[(&str, &str)] = &[
    (
        "aurora",
        include_str!("../../../library/animations/aurora.json"),
    ),
    (
        "breathe-calm",
        include_str!("../../../library/animations/breathe-calm.json"),
    ),
    (
        "candle",
        include_str!("../../../library/animations/candle.json"),
    ),
    (
        "campfire",
        include_str!("../../../library/animations/campfire.json"),
    ),
    (
        "lava",
        include_str!("../../../library/animations/lava.json"),
    ),
    (
        "ocean",
        include_str!("../../../library/animations/ocean.json"),
    ),
    (
        "rainbow-slow",
        include_str!("../../../library/animations/rainbow-slow.json"),
    ),
    (
        "sunrise",
        include_str!("../../../library/animations/sunrise.json"),
    ),
    (
        "sunset",
        include_str!("../../../library/animations/sunset.json"),
    ),
];

/// Built-in presets by name.
pub const PRESETS: &[(&str, &str)] = &[
    ("focus", include_str!("../../../library/presets/focus.json")),
    ("movie", include_str!("../../../library/presets/movie.json")),
    (
        "night-light",
        include_str!("../../../library/presets/night-light.json"),
    ),
    (
        "reading",
        include_str!("../../../library/presets/reading.json"),
    ),
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    fn names(dir: &str, suffix: &str, skip: &str) -> BTreeSet<String> {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(dir);
        std::fs::read_dir(root)
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.ends_with(suffix) && (skip.is_empty() || !n.ends_with(skip)))
            .map(|n| n.trim_end_matches(suffix).to_string())
            .collect()
    }

    /// Adding a file to the repository without embedding it must fail CI, not silently ship without it.
    #[test]
    fn everything_in_the_repository_is_embedded() {
        let defs: BTreeSet<String> = DEFINITIONS
            .iter()
            .map(|d| lampbus_defs::Definition::from_json(d).unwrap().id)
            .collect();
        assert_eq!(defs, names("definitions", ".json", ".vectors.json"));
        let anims: BTreeSet<String> = ANIMATIONS.iter().map(|(n, _)| (*n).to_string()).collect();
        assert_eq!(anims, names("library/animations", ".json", ""));
        let presets: BTreeSet<String> = PRESETS.iter().map(|(n, _)| (*n).to_string()).collect();
        assert_eq!(presets, names("library/presets", ".json", ""));
    }

    #[test]
    fn every_embedded_animation_is_valid() {
        for (n, text) in ANIMATIONS {
            let v: serde_json::Value = serde_json::from_str(text).unwrap();
            lampbus_render::anim::load(&v).unwrap_or_else(|e| panic!("{n}: {e}"));
        }
    }
}
