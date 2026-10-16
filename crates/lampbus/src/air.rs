//! `lampbus air`: steer lamps by broadcast, with no connection.
//!
//! Builds the lamp's advertising frame ([`lampbus_air`]) and hands it to BlueZ's `btmgmt` tool, which advertises it
//! for a few seconds as non-connectable manufacturer data. Linux only, and it needs a Bluetooth adapter that can
//! advertise (any recent USB one) and permission to use `btmgmt` (root, or `setcap cap_net_admin+ep` on it).
//! `--print` shows the advertisement as hex instead, for any other tool that can broadcast raw data.

use lampbus_air::{encrypt, on_air, Sender, CHANNEL, COMPANY_ID};
use std::path::Path;
use std::process::Command;

/// What to broadcast.
#[derive(Debug, Clone, PartialEq)]
pub enum What {
    Colour { rgb: [u8; 3], brightness: u8 },
    Off,
    Effect { mode: u8, brightness: u8, speed: u8 },
    LineOrder(u8),
}

/// The advertising data (AD structures) for one command, as hex: length, type 0xFF, company id, then the on-air bytes.
pub fn ad_hex(what: &What, sender: &Sender, rand: u8, index: u8) -> Result<String, String> {
    let plain = match what {
        What::Colour { rgb, brightness } => sender.colour(rand, index, *rgb, *brightness),
        What::Off => sender.off(rand, index),
        What::Effect {
            mode,
            brightness,
            speed,
        } => sender.effect(rand, index, *mode, *brightness, *speed),
        What::LineOrder(o) => sender.line_order(rand, index, *o),
    };
    let mut out = [0u8; 32];
    let n = on_air(CHANNEL, &encrypt(&plain), &mut out).ok_or("frame does not fit")?;
    let body = out.get(..n).ok_or("frame does not fit")?;
    let [lo, hi] = COMPANY_ID.to_le_bytes();
    let mut ad = vec![
        u8::try_from(body.len() + 3).map_err(|e| e.to_string())?,
        0xff,
        lo,
        hi,
    ];
    ad.extend_from_slice(body);
    Ok(ad.iter().map(|b| format!("{b:02x}")).collect())
}

/// The `btmgmt` arguments that advertise `hex` for `secs` seconds as advertising instance 1. No `-c`, `-g`, `-l` or
/// `-m`: those make BlueZ add a Flags field, which would shift the frame and break it.
pub fn btmgmt_args(hex: &str, secs: u64) -> Vec<String> {
    ["add-adv", "-d", hex, "-t", &secs.to_string(), "1"]
        .map(String::from)
        .to_vec()
}

/// The sender's stable random code, kept in the config directory like the vendor app keeps one per phone.
pub fn sender_code(dir: &Path) -> [u8; 3] {
    let file = dir.join("air-code");
    if let Ok(text) = std::fs::read_to_string(&file) {
        let v: Vec<u8> = (0..3)
            .filter_map(|i| {
                text.get(2 * i..2 * i + 2)
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
            })
            .collect();
        if let [a, b, c] = v.as_slice() {
            return [*a, *b, *c];
        }
    }
    let mut c = [0u8; 3];
    let _ = getrandom::fill(&mut c);
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(&file, format!("{:02x}{:02x}{:02x}\n", c[0], c[1], c[2]));
    c
}

/// Broadcast one command (or print it).
pub fn run(
    what: &What,
    group: u8,
    secs: u64,
    print: bool,
    config_dir: &Path,
) -> Result<(), String> {
    let mut r = [0u8; 2];
    getrandom::fill(&mut r).map_err(|e| e.to_string())?;
    let sender = Sender {
        group,
        code: sender_code(config_dir),
    };
    let hex = ad_hex(what, &sender, r[0], r[1])?;
    if print || !cfg!(target_os = "linux") {
        println!("advertise, non-connectable, no flags, for {secs} s: {hex}");
        if !print {
            println!(
                "(sending needs Linux with BlueZ's btmgmt: sudo btmgmt {})",
                btmgmt_args(&hex, secs).join(" ")
            );
        }
        return Ok(());
    }
    let _ = Command::new("btmgmt").args(["rm-adv", "1"]).output();
    let out = Command::new("btmgmt")
        .args(btmgmt_args(&hex, secs))
        .output()
        .map_err(|e| format!("cannot run btmgmt ({e}): install the bluez package; it needs root or the cap_net_admin capability"))?;
    if !out.status.success() {
        return Err(format!(
            "btmgmt failed: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    std::thread::sleep(std::time::Duration::from_secs(secs));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_advertisement_is_length_type_company_then_the_on_air_bytes() {
        let s = Sender {
            group: 1,
            code: [0x11, 0x22, 0x33],
        };
        let hex = ad_hex(
            &What::Colour {
                rgb: [255, 0, 0],
                brightness: 100,
            },
            &s,
            0x2a,
            1,
        )
        .unwrap();
        // 27 bytes follow the length byte: type 0xff, company 0xfff0, and 24 on-air bytes (vendor library vector)
        assert_eq!(
            hex,
            "1bfff0ff1887521e45f9e030df6bd7cf9e977da0a3e91ec9036feb33"
        );
        assert_eq!(
            hex.len(),
            2 * 28,
            "28 bytes, inside the 31 an advertisement allows"
        );
    }

    #[test]
    fn btmgmt_gets_no_flag_options() {
        let a = btmgmt_args("1bff", 3);
        assert_eq!(a, ["add-adv", "-d", "1bff", "-t", "3", "1"]);
        assert!(!a
            .iter()
            .any(|x| matches!(x.as_str(), "-c" | "-g" | "-l" | "-m")));
    }

    #[test]
    fn the_sender_code_is_stable() {
        let dir = std::env::temp_dir().join(format!("lampbus-air-test-{}", std::process::id()));
        let first = sender_code(&dir);
        assert_eq!(sender_code(&dir), first);
        let _ = std::fs::remove_dir_all(dir);
    }
}
