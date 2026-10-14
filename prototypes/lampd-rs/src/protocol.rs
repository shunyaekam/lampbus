//! iStrip BLE lamp wire protocol, decoded from the iStrip+ Android app (com.ben.istrips 1.4.3).
//!
//! Every command is a 16-byte plaintext frame, AES-128-ECB encrypted with a fixed key and written
//! (without response) to characteristic AC52 of service AC50.
//! Plaintext layout: `54 52 00 57 | cmd | group | args... (zero padded to 16 bytes)`.

use aes::Aes128;
use aes::cipher::{Array, BlockCipherDecrypt, BlockCipherEncrypt, KeyInit};
use serde_json::{Map, Value, json};

pub type Frame = [u8; 16];

const KEY: [u8; 16] = [
    0x34, 0x52, 0x2a, 0x5b, 0x7a, 0x6e, 0x49, 0x2c, 0x08, 0x09, 0x0a, 0x9d, 0x8d, 0x2a, 0x23, 0xf8,
];
pub const WRITE_CHAR: &str = "0000ac52-1212-efde-1523-785fedbeda25";
const HEADER: [u8; 4] = [0x54, 0x52, 0x00, 0x57];

pub const CMD_COLOR: u8 = 2;
pub const CMD_RHYTHM: u8 = 3;
pub const CMD_TIMER: u8 = 4;
pub const CMD_LINE: u8 = 5;
pub const CMD_SPEED: u8 = 6;
pub const CMD_BRIGHTNESS: u8 = 7;

/// Built-in effects (command 2, mode = list index + 1 in the app).
pub const EFFECTS: [(u8, &str); 12] = [
    (1, "Fade7"), (2, "Fade3"), (3, "Breath R"), (4, "Strobe R"), (5, "Breath7"), (6, "Breath3"),
    (7, "Breath B"), (8, "Strobe B"), (9, "Flash7"), (10, "Flash3"), (11, "Breath G"), (12, "Strobe G"),
];
pub const EFFECT_ALIASES: [(&str, u8); 7] = [
    ("rainbow", 1), ("rainbow-fade", 1), ("fade", 1), ("rainbow-breathe", 5), ("breathe", 5),
    ("rainbow-flash", 9), ("flash", 9),
];

pub fn effect_by_name(name: &str) -> Option<u8> {
    let n = name.to_lowercase();
    EFFECT_ALIASES.iter().find(|(a, _)| *a == n).map(|(_, m)| *m)
        .or_else(|| EFFECTS.iter().find(|(_, e)| e.to_lowercase() == n).map(|(m, _)| *m))
}

pub fn encrypt(frame: &Frame) -> Frame {
    let cipher = Aes128::new(&Array::from(KEY));
    let mut block = Array::from(*frame);
    cipher.encrypt_block(&mut block);
    block.into()
}

#[cfg(test)]
pub fn decrypt(frame: &Frame) -> Frame {
    let cipher = Aes128::new(&Array::from(KEY));
    let mut block = Array::from(*frame);
    cipher.decrypt_block(&mut block);
    block.into()
}

fn clamp(v: f64, lo: f64, hi: f64) -> u8 {
    v.round().clamp(lo, hi) as u8
}

pub fn frame(cmd: u8, group: u8, args: &[u8]) -> Frame {
    let mut f = [0u8; 16];
    f[..4].copy_from_slice(&HEADER);
    f[4] = cmd;
    f[5] = group;
    for (i, a) in args.iter().take(10).enumerate() {
        f[6 + i] = *a;
    }
    f
}

/// mode 0 = static colour, 1..12 = built-in effect. kind: 0 RGB, 1 white, 2 CCT.
pub fn color(r: f64, g: f64, b: f64, brightness: f64, speed: f64, mode: u8, kind: u8) -> Frame {
    frame(CMD_COLOR, 1, &[
        mode, clamp(r, 0., 255.), clamp(g, 0., 255.), clamp(b, 0., 255.),
        clamp(brightness, 0., 100.), clamp(speed, 1., 100.), kind,
    ])
}

pub fn off() -> Frame {
    color(0., 0., 0., 100., 100., 0, 0)
}

pub fn brightness(v: f64) -> Frame {
    frame(CMD_BRIGHTNESS, 1, &[clamp(v, 0., 100.)])
}

pub fn speed(v: f64) -> Frame {
    frame(CMD_SPEED, 1, &[clamp(v, 1., 100.)])
}

/// Validated raw plaintext frame; only control commands 2..=7 are allowed.
pub fn raw(hex: &str) -> Result<Frame, String> {
    let clean: String = hex.chars().filter(|c| !c.is_whitespace()).collect();
    if clean.len() != 32 || !clean.is_ascii() {
        return Err("raw frame must be 16 bytes (32 hex chars)".into());
    }
    let mut f = [0u8; 16];
    for i in 0..16 {
        f[i] = u8::from_str_radix(&clean[2 * i..2 * i + 2], 16).map_err(|_| "bad hex")?;
    }
    if f[..4] != HEADER || !(2..=7).contains(&f[4]) {
        return Err("raw frame must start 54 52 00 57 <cmd 2-7>".into());
    }
    Ok(f)
}

pub fn hex(f: &[u8]) -> String {
    f.iter().map(|b| format!("{b:02x}")).collect()
}

fn num(args: &Map<String, Value>, key: &str, default: Option<f64>) -> Result<f64, String> {
    match args.get(key) {
        Some(v) => v.as_f64().ok_or_else(|| format!("{key} must be a number")),
        None => default.ok_or_else(|| format!("missing argument {key}")),
    }
}

/// Build a frame for a named raw operation (POST /api/raw {"op": ..., "args": {...}}).
pub fn build(op: &str, a: &Map<String, Value>) -> Result<Frame, String> {
    Ok(match op {
        "color" => color(num(a, "r", None)?, num(a, "g", None)?, num(a, "b", None)?,
            num(a, "brightness", Some(100.))?, num(a, "speed", Some(100.))?,
            num(a, "mode", Some(0.))? as u8, num(a, "kind", Some(0.))? as u8),
        "off" => off(),
        "brightness" => brightness(num(a, "value", None)?),
        "speed" => speed(num(a, "value", None)?),
        "line_sequence" => frame(CMD_LINE, 1, &[num(a, "order", None)? as u8]),
        "rhythm" => frame(CMD_RHYTHM, 1, &[num(a, "status", None)? as u8,
            num(a, "fft", Some(0.))? as u8, num(a, "sensitivity", Some(0.))? as u8]),
        "timer" => {
            let g = |k| num(a, k, None).map(|v| v as u8);
            let flags = (if g("on_enable")? > 0 { 16 } else { 0 }) | (if g("off_enable")? > 0 { 32 } else { 0 })
                | g("weekday")?; // 1=Mon..7=Sun, caller supplies the lamp's clock
            frame(CMD_TIMER, 1, &[flags, g("hour")?, g("minute")?, g("second")?,
                g("on_days")?, g("on_hour")?, g("on_min")?, g("off_days")?, g("off_hour")?, g("off_min")?])
        }
        _ => return Err(format!("unknown op {op:?}; known: {}", OPS.iter().map(|o| o.0).collect::<Vec<_>>().join(", "))),
    })
}

pub const OPS: [(&str, &str, &str); 7] = [
    ("color", "r g b [brightness=100] [speed=100] [mode=0] [kind=0]", "Static colour (or built-in effect via mode). brightness/speed 0-100."),
    ("off", "", "Turn the lamp off (colour command with RGB 0)."),
    ("brightness", "value", "Set brightness 0-100."),
    ("speed", "value", "Set effect speed 1-100."),
    ("line_sequence", "order", "Colour-channel order index (mapping not decoded)."),
    ("rhythm", "status [fft] [sensitivity]", "Music mode frame (partly decoded)."),
    ("timer", "on_enable off_enable weekday hour minute second on_days on_hour on_min off_days off_hour off_min",
        "Lamp-side on/off timer; weekday/hour/minute/second set the lamp's clock. UNVERIFIED weekday bit order."),
];

pub fn ops_json() -> Value {
    let mut m = Map::new();
    for (name, params, doc) in OPS {
        m.insert(name.into(), json!({"params": params, "doc": doc}));
    }
    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(s: &str) -> Frame {
        let mut f = [0u8; 16];
        for i in 0..16 {
            f[i] = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap();
        }
        f
    }

    /// (name, plaintext, ciphertext) produced by the earlier implementation that was verified on a real lamp.
    const GOLDEN: [(&str, &str, &str); 7] = [
        ("red100", "54520057020100ff0000646400000000", "1273622a87797e5c768211ee59308e5b"),
        ("off", "54520057020100000000646400000000", "7f77944a5498752f8929a2e435c4be8d"),
        ("bright50", "54520057070132000000000000000000", "d4914856960e6f088828210fda8de563"),
        ("rainbow", "54520057020101000000646400000000", "d69d7e97e04c353539d3683fb216f50b"),
        ("warm70", "54520057020100ff9329466400000000", "90c50c29818567dd710f2db8d2880e31"),
        ("speed30", "5452005706011e000000000000000000", "dea51bc5ad29ff63a204c2e7acf95a1d"),
        ("timer", "545200570401330c1e0f1f071e1f1700", "a47e998ae14495900aaf3b04251d282f"),
    ];

    #[test]
    fn frames_and_encryption_match_reference_vectors() {
        let built: [(&str, Frame); 7] = [
            ("red100", color(255., 0., 0., 100., 100., 0, 0)),
            ("off", off()),
            ("bright50", brightness(50.)),
            ("rainbow", color(0., 0., 0., 100., 100., 1, 0)),
            ("warm70", color(255., 147., 41., 70., 100., 0, 0)),
            ("speed30", speed(30.)),
            ("timer", build("timer", json!({"on_enable":1,"off_enable":1,"weekday":3,"hour":12,"minute":30,"second":15,
                "on_days":31,"on_hour":7,"on_min":30,"off_days":31,"off_hour":23,"off_min":0}).as_object().unwrap()).unwrap()),
        ];
        for ((name, plain, cipher), (bname, frame)) in GOLDEN.iter().zip(built.iter()) {
            assert_eq!(name, bname);
            assert_eq!(hex(frame), *plain, "plaintext {name}");
            assert_eq!(hex(&encrypt(frame)), *cipher, "ciphertext {name}");
            assert_eq!(decrypt(&unhex(cipher)), unhex(plain), "decrypt {name}");
        }
    }

    #[test]
    fn raw_validation() {
        assert!(raw("54520057020100ff0000646400000000").is_ok());
        assert!(raw("00000000000000000000000000000000").is_err());
        assert!(raw("5452005701").is_err());
        assert!(raw("54520057ff0100ff0000646400000000").is_err());
    }
}
