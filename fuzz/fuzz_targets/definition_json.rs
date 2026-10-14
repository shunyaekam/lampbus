//! Any bytes as a lamp definition: parsing must never panic, and a definition that parses must encode every one of its
//! commands for in-range, edge and hostile argument values without panicking and with a fixed frame length.
#![no_main]

use lampbus_defs::Definition;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else { return };
    let Ok(def) = Definition::from_json(text) else { return };
    let _ = def.caps();
    for (name, cmd) in &def.commands {
        for pick in [0.0, 0.5, 1.0] {
            let args: Vec<(&str, f64)> = cmd.params.iter().map(|(k, p)| (k.as_str(), p.min + pick * (p.max - p.min))).collect();
            if let Ok(out) = def.encode(name, &args) {
                assert_eq!(out.plain.len(), cmd.frame_len());
            }
        }
        for hostile in [f64::NAN, f64::INFINITY, -1e300, 1e300] {
            let args: Vec<(&str, f64)> = cmd.params.keys().map(|k| (k.as_str(), hostile)).collect();
            let _ = def.encode(name, &args);
        }
    }
});
