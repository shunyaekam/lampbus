//! Conformance: every definition in `definitions/` must reproduce its independent vectors byte for byte.
//!
//! The vectors come from `tools/oracles`, whose oracles re-implement the upstream projects' frame building
//! (or, for the iStrip lamp, the hardware-verified prototype) with no shared code. A pass here means one small
//! interpreter, driven only by data, reproduces every family.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use lampbus_defs::{parse_vectors, Definition, Status};
use std::fs;
use std::path::PathBuf;

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../definitions")
}

fn load_all() -> Vec<(String, Definition)> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir()).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if name.ends_with(".json") && !name.ends_with(".vectors.json") {
            let def = Definition::from_json(&fs::read_to_string(&path).unwrap())
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            out.push((name, def));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn vectors_for(id: &str) -> Vec<lampbus_defs::Vector> {
    let text = fs::read_to_string(dir().join(format!("{id}.vectors.json")))
        .unwrap_or_else(|_| panic!("missing {id}.vectors.json"));
    parse_vectors(&text).unwrap()
}

#[test]
fn at_least_four_families_and_one_verified_on_hardware() {
    let defs = load_all();
    assert!(defs.len() >= 4, "only {} definitions", defs.len());
    assert!(defs
        .iter()
        .any(|(_, d)| d.status == Status::HardwareVerified));
}

#[test]
fn file_name_matches_id() {
    for (file, def) in load_all() {
        assert_eq!(file, format!("{}.json", def.id));
    }
}

#[test]
fn every_vector_reproduces_exactly() {
    for (file, def) in load_all() {
        let vectors = vectors_for(&def.id);
        let failures = def.verify_vectors(&vectors);
        assert!(failures.is_empty(), "{file}: {failures:#?}");
    }
}

#[test]
fn every_command_has_at_least_one_vector() {
    for (file, def) in load_all() {
        let vectors = vectors_for(&def.id);
        for cmd in def.commands.keys() {
            assert!(
                vectors.iter().any(|v| &v.command == cmd),
                "{file}: no vector for command {cmd:?}"
            );
        }
    }
}

/// Frames have a fixed length per command and encoding never fails for in-range, finite inputs.
#[test]
fn encoding_is_total_and_fixed_length() {
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % 1000) as f64 / 999.0
    };
    for (file, def) in load_all() {
        for (name, cmd) in &def.commands {
            let expect = cmd.frame_len();
            for _ in 0..200 {
                let args: Vec<(&str, f64)> = cmd
                    .params
                    .iter()
                    .map(|(k, p)| (k.as_str(), p.min + next() * (p.max - p.min)))
                    .collect();
                let out = def
                    .encode(name, &args)
                    .unwrap_or_else(|e| panic!("{file}/{name}: {e}"));
                assert_eq!(out.plain.len(), expect, "{file}/{name}");
                assert_eq!(out.wire.len(), expect, "{file}/{name}");
            }
        }
    }
}

#[test]
fn capabilities_match_what_each_family_can_do() {
    let defs = load_all();
    let caps = |id: &str| defs.iter().find(|(_, d)| d.id == id).unwrap().1.caps();

    let i = caps("istrip-ssl");
    assert_eq!(i.off.as_deref(), Some("off"));
    assert!(i.on.is_none(), "powered on by sending a colour");
    let color = i.color.unwrap();
    assert_eq!(
        color.brightness.map(|r| (r.min, r.max)),
        Some((0.0, 100.0)),
        "brightness rides inside the colour frame"
    );
    assert!(i.brightness.is_some() && i.effect.is_some() && i.effect_speed.is_some());
    let effect = i.effect.unwrap();
    assert_eq!((effect.mode.min, effect.mode.max), (1.0, 12.0));

    let e = caps("elk-bledom");
    assert!(e.on.is_some() && e.off.is_some());
    assert!(
        e.color.unwrap().brightness.is_none(),
        "separate brightness command"
    );
    assert_eq!(
        e.brightness.map(|b| (b.range.min, b.range.max)),
        Some((0.0, 100.0))
    );
    assert!(e.effect.is_none());

    let t = caps("triones");
    assert!(
        t.brightness.is_none(),
        "no brightness control: the core must scale the colour"
    );

    let g = caps("govee-h61");
    assert_eq!(
        g.brightness.map(|b| (b.range.min, b.range.max)),
        Some((0.0, 255.0))
    );
}
