//! Print ready-to-advertise manufacturer data for a few commands, for a generic BLE advertiser app (for example
//! nRF Connect on Android): company id 0xFFF0, non-connectable, these bytes.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]
use lampbus_air::{encrypt, on_air, Sender, CHANNEL, COMPANY_ID};

fn main() {
    let s = Sender {
        group: 1,
        code: [0x11, 0x22, 0x33],
    };
    let frames = [
        ("red 100%", s.colour(0x2a, 1, [255, 0, 0], 100)),
        ("blue 100%", s.colour(0x2b, 2, [0, 0, 255], 100)),
        ("green 40%", s.colour(0x2c, 3, [0, 255, 0], 40)),
        ("off", s.off(0x2d, 4)),
    ];
    println!("company id 0x{COMPANY_ID:04X}, non-connectable, advertise on all channels\n");
    for (name, plain) in frames {
        let mut out = [0u8; 32];
        let n = on_air(CHANNEL, &encrypt(&plain), &mut out).unwrap();
        let hex: String = out[..n].iter().map(|b| format!("{b:02X}")).collect();
        println!("{name:10} {hex}");
    }
}
