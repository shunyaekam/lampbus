//! Generate (or check) conformance vectors for `definitions/*.json` from independent oracles.
//!
//! Each oracle re-implements the frame construction of an upstream open-source project, or, for the iStrip lamp,
//! of the hand-written implementation that was verified on a real lamp, with no code shared with the
//! data-driven interpreter in `lampbus-defs`. The conformance test in that crate then checks that the definitions
//! reproduce these bytes exactly.
//!
//! ```text
//! cargo run -p lampbus-oracles            # write definitions/<id>.vectors.json
//! cargo run -p lampbus-oracles -- --check # fail if the committed files differ (used in CI)
//! ```

use aes::cipher::{Array, BlockCipherEncrypt, KeyInit};
use serde_json::{json, Map, Value};
use std::path::PathBuf;
use std::process::ExitCode;

const COLORS: [(u8, u8, u8); 6] = [
    (0, 0, 0),
    (255, 255, 255),
    (255, 0, 128),
    (1, 2, 3),
    (18, 52, 86),
    (255, 147, 41),
];

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn vector(command: &str, args: Value, plain: &[u8], wire: Option<&[u8]>) -> Value {
    let mut m = Map::new();
    m.insert("command".into(), json!(command));
    m.insert("args".into(), args);
    m.insert("plain".into(), json!(hex(plain)));
    if let Some(w) = wire {
        m.insert("wire".into(), json!(hex(w)));
    }
    Value::Object(m)
}

// ---- iStrip: the implementation verified on a real SSL-E089ED lamp ------------------------------------------------
mod istrip {
    use super::*;
    const KEY: [u8; 16] = [
        0x34, 0x52, 0x2a, 0x5b, 0x7a, 0x6e, 0x49, 0x2c, 0x08, 0x09, 0x0a, 0x9d, 0x8d, 0x2a, 0x23,
        0xf8,
    ];

    fn frame(cmd: u8, group: u8, args: &[u8]) -> [u8; 16] {
        let mut f = [0u8; 16];
        let head = [0x54, 0x52, 0x00, 0x57, cmd, group];
        for (slot, byte) in f.iter_mut().zip(head.iter().chain(args)) {
            *slot = *byte;
        }
        f
    }

    fn encrypt(f: &[u8; 16]) -> [u8; 16] {
        let mut block = Array::from(*f);
        aes::Aes128::new(&Array::from(KEY)).encrypt_block(&mut block);
        block.into()
    }

    fn add(out: &mut Vec<Value>, cmd: &str, args: Value, plain: [u8; 16]) {
        out.push(vector(cmd, args, &plain, Some(&encrypt(&plain))));
    }

    pub fn vectors() -> Vec<Value> {
        let mut v = Vec::new();
        let color =
            |r, g, b, bri, speed, mode, group| frame(2, group, &[mode, r, g, b, bri, speed, 0]);
        for (r, g, b) in COLORS {
            add(
                &mut v,
                "color",
                json!({"r": r, "g": g, "b": b}),
                color(r, g, b, 100, 100, 0, 1),
            );
        }
        add(
            &mut v,
            "color",
            json!({"r": 255, "g": 0, "b": 0, "brightness": 30, "speed": 50}),
            color(255, 0, 0, 30, 50, 0, 1),
        );
        add(
            &mut v,
            "color",
            json!({"r": 9, "g": 8, "b": 7, "brightness": 0, "speed": 1, "group": 3}),
            color(9, 8, 7, 0, 1, 0, 3),
        );
        add(&mut v, "off", json!({}), color(0, 0, 0, 100, 100, 0, 1));
        add(
            &mut v,
            "off",
            json!({"brightness": 40, "speed": 20}),
            color(0, 0, 0, 40, 20, 0, 1),
        );
        for mode in [1u8, 5, 9, 12] {
            add(
                &mut v,
                "effect",
                json!({"mode": mode}),
                color(0, 0, 0, 100, 100, mode, 1),
            );
        }
        add(
            &mut v,
            "effect",
            json!({"mode": 3, "brightness": 60, "speed": 10}),
            color(0, 0, 0, 60, 10, 3, 1),
        );
        for level in [0u8, 1, 50, 100] {
            add(
                &mut v,
                "brightness",
                json!({"level": level}),
                frame(7, 1, &[level]),
            );
        }
        for value in [1u8, 30, 100] {
            add(
                &mut v,
                "speed",
                json!({"value": value}),
                frame(6, 1, &[value]),
            );
        }
        // decoded from the app, not yet checked on hardware: colour type byte, group, rhythm, timer, line order
        for ty in [1u8, 2] {
            add(
                &mut v,
                "color",
                json!({"r": 255, "g": 147, "b": 41, "type": ty}),
                frame(2, 1, &[0, 255, 147, 41, 100, 100, ty]),
            );
        }
        for group in [1u8, 7] {
            add(
                &mut v,
                "group",
                json!({"group": group}),
                frame(1, group, &[]),
            );
        }
        add(
            &mut v,
            "rhythm",
            json!({"status": 1, "fft": 200, "sensitivity": 80, "group": 2}),
            frame(3, 2, &[1, 200, 80]),
        );
        add(
            &mut v,
            "timer",
            json!({"flags": 49, "hour": 22, "minute": 30, "second": 5, "on_week": 0, "on_hour": 7, "on_minute": 0, "off_week": 0, "off_hour": 23, "off_minute": 15}),
            frame(4, 1, &[49, 22, 30, 5, 0, 7, 0, 0, 23, 15]),
        );
        for order in [0u8, 5] {
            add(
                &mut v,
                "line_order",
                json!({"order": order}),
                frame(5, 1, &[order]),
            );
        }
        v
    }
}

// ---- ELK-BLEDOM: FergusInLondon/ELK-BLEDOM device/commands.go and TheSylex/... src/device.rs ----------------------
fn elk() -> Vec<Value> {
    let mut v = vec![
        vector(
            "power_on",
            json!({}),
            &[0x7e, 0x00, 0x04, 0xf0, 0x00, 0x01, 0xff, 0x00, 0xef],
            None,
        ),
        vector(
            "power_off",
            json!({}),
            &[0x7e, 0x00, 0x04, 0x00, 0x00, 0x00, 0xff, 0x00, 0xef],
            None,
        ),
    ];
    for (r, g, b) in COLORS {
        v.push(vector(
            "color",
            json!({"r": r, "g": g, "b": b}),
            &[0x7e, 0x00, 0x05, 0x03, r, g, b, 0x00, 0xef],
            None,
        ));
    }
    for level in [0u8, 1, 50, 100] {
        v.push(vector(
            "brightness",
            json!({"level": level}),
            &[0x7e, 0x00, 0x01, level, 0x00, 0x00, 0x00, 0x00, 0xef],
            None,
        ));
    }
    v
}

// ---- Triones: lightinglibs/flux_led protocol.py (original-protocol messages) --------------------------------------
fn triones() -> Vec<Value> {
    let mut v = vec![
        vector("power_on", json!({}), &[0xcc, 0x23, 0x33], None),
        vector("power_off", json!({}), &[0xcc, 0x24, 0x33], None),
    ];
    for (r, g, b) in COLORS {
        v.push(vector(
            "color",
            json!({"r": r, "g": g, "b": b}),
            &[0x56, r, g, b, 0x00, 0xf0, 0xaa],
            None,
        ));
    }
    v
}

// ---- Govee: Beshelmek/govee_ble_lights light.py _prepareSinglePacketData ------------------------------------------
fn govee_frame(cmd: u8, payload: &[u8]) -> Vec<u8> {
    let mut frame = vec![0x33, cmd];
    frame.extend_from_slice(payload);
    frame.resize(19, 0); // pad to 19 bytes (plus checksum)
    let checksum = frame.iter().fold(0u8, |a, b| a ^ b); // XOR of all data bytes
    frame.push(checksum);
    frame
}

fn govee() -> Vec<Value> {
    let mut v = vec![
        vector("power_on", json!({}), &govee_frame(0x01, &[0x01]), None),
        vector("power_off", json!({}), &govee_frame(0x01, &[0x00]), None),
    ];
    for level in [0u8, 1, 128, 255] {
        v.push(vector(
            "brightness",
            json!({"level": level}),
            &govee_frame(0x04, &[level]),
            None,
        ));
    }
    for (r, g, b) in COLORS {
        v.push(vector(
            "color",
            json!({"r": r, "g": g, "b": b}),
            &govee_frame(0x05, &[0x02, r, g, b]),
            None,
        ));
    }
    v
}

fn main() -> ExitCode {
    let check = std::env::args().any(|a| a == "--check");
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../definitions");
    let sets: [(&str, Vec<Value>); 4] = [
        ("istrip-ssl", istrip::vectors()),
        ("elk-bledom", elk()),
        ("triones", triones()),
        ("govee-h61", govee()),
    ];
    let mut stale = false;
    for (id, vectors) in sets {
        let path = dir.join(format!("{id}.vectors.json"));
        let text =
            serde_json::to_string_pretty(&Value::Array(vectors.clone())).unwrap_or_default() + "\n";
        if check {
            let committed = std::fs::read_to_string(&path).unwrap_or_default();
            if committed != text {
                eprintln!(
                    "{id}: {} is out of date; run `cargo run -p lampbus-oracles`",
                    path.display()
                );
                stale = true;
            }
        } else if let Err(e) = std::fs::write(&path, &text) {
            eprintln!("{id}: {e}");
            return ExitCode::FAILURE;
        } else {
            println!("{id:12} {:3} vectors -> {}", vectors.len(), path.display());
        }
    }
    if stale {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
