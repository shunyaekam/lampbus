//! The over-the-air side of iStrip lamps.
//!
//! Besides the connected (GATT) protocol, these lamps can be steered, and can answer, by **advertising**: the sender
//! puts one 16-byte frame into a non-connectable advertisement and every lamp in range acts on it. No connection,
//! no one-connection limit, many lamps at once. This crate is that frame layer, reimplemented from the vendor app's
//! native library (`libWirelessEncoder.so`) and verified against the real library byte for byte (see the tests):
//!
//! 1. [`encrypt`] / [`decrypt`]: a keyed, byte-chained transform of the 16-byte frame.
//! 2. [`on_air`]: wraps the encrypted frame so that, once the radio applies its own channel whitening, the bits on
//!    the air form the proprietary packet the lamp's receiver expects: header, address, payload, CRC-16, with a
//!    second whitening layer and bit reversal. The app advertises the result as manufacturer data under
//!    [`COMPANY_ID`], non-connectable, on channel 37.
//! 3. [`parse_status`]: decodes what a lamp advertises back (on or off, mode, brightness, group, line order).
//!
//! It has no dependencies, allocates nothing, and contains no `unsafe`. Transmitting needs a radio that can advertise
//! arbitrary manufacturer data (a Linux BlueZ host, an ESP32, a phone); macOS cannot.
#![no_std]
#![forbid(unsafe_code)]

/// The manufacturer id the vendor app advertises under.
pub const COMPANY_ID: u16 = 0xFFF0;
/// Bytes 1 to 3 of every command frame: the product identifier.
pub const PRODUCT: [u8; 3] = [0x54, 0x00, 0x1d];
/// The advertising channel the on-air framing is built for (and the whitening seed).
pub const CHANNEL: u8 = 37;
/// Longest data [`on_air`] accepts.
pub const MAX_DATA: usize = 44;

/// Frame types (byte 4 of a frame).
pub mod kind {
    /// Colour, effect or off.
    pub const COMMAND: u8 = 0xc3;
    /// Set the LED line order.
    pub const LINE_ORDER: u8 = 0xc4;
    /// A lamp announcing itself.
    pub const DISCOVER: u8 = 0x42;
    /// A lamp reporting its state.
    pub const STATUS: u8 = 0x43;
    /// A lamp reporting its line order.
    pub const LINE_ORDER_REPLY: u8 = 0x46;
}

const KEY: [u8; 3] = [0x43, 0x71, 0x5c];
const TABLE: [u8; 16] = [
    0x95, 0x83, 0x38, 0x2a, 0xbc, 0x71, 0xc6, 0xa9, 0xfd, 0x54, 0x67, 0x32, 0xe6, 0x1e, 0x4f, 0xdb,
];

/// Encrypt a 16-byte frame.
pub fn encrypt(frame: &[u8; 16]) -> [u8; 16] {
    let mut out = [0u8; 16];
    let [first, rest @ ..] = frame;
    let o0 = (first ^ 0x7c).wrapping_sub(0x3a);
    let seed = (KEY[0].wrapping_sub(KEY[2]) ^ KEY[1].wrapping_add(*first)).wrapping_add(o0);
    out[0] = o0;
    let mut prev = *first;
    for ((o, cur), t) in out.iter_mut().skip(1).zip(rest).zip(TABLE.iter().skip(1)) {
        *o = cur.wrapping_sub(prev).wrapping_add(seed) ^ t;
        prev = *cur;
    }
    out
}

/// Decrypt a 16-byte frame; the inverse of [`encrypt`].
pub fn decrypt(frame: &[u8; 16]) -> [u8; 16] {
    let mut out = [0u8; 16];
    let [first, rest @ ..] = frame;
    let o0 = first.wrapping_add(0x3a) ^ 0x7c;
    let seed = (KEY[0].wrapping_sub(KEY[2]) ^ KEY[1].wrapping_add(o0)).wrapping_add(*first);
    out[0] = o0;
    let mut prev = o0;
    for ((o, cur), t) in out.iter_mut().skip(1).zip(rest).zip(TABLE.iter().skip(1)) {
        prev = (cur ^ t).wrapping_sub(seed).wrapping_add(prev);
        *o = prev;
    }
    out
}

/// CRC-16 (polynomial 0x1021) over the key bytes in reverse order, then each data byte bit-reversed; the result is
/// bit-reversed and inverted, as the lamp's receiver computes it.
fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0xffffu16;
    for b in KEY
        .iter()
        .rev()
        .copied()
        .chain(data.iter().map(|d| d.reverse_bits()))
    {
        crc ^= u16::from(b) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc.reverse_bits() ^ 0xffff
}

/// The data-whitening register of Bluetooth Low Energy (x^7 + x^4 + 1), seeded with a channel number.
struct Lfsr([u8; 7]);

impl Lfsr {
    fn new(seed: u8) -> Lfsr {
        let bit = |n: u8| (seed >> n) & 1;
        Lfsr([1, bit(5), bit(4), bit(3), bit(2), bit(1), bit(0)])
    }

    fn next(&mut self) -> u8 {
        let [s0, s1, s2, s3, s4, s5, s6] = self.0;
        self.0 = [s6, s0, s1, s2, s3 ^ s6, s4, s5];
        s6
    }

    fn whiten(&mut self, data: &mut [u8]) {
        for b in data {
            let mut acc = 0u8;
            for k in 0..8 {
                acc |= (((*b >> k) & 1) ^ self.next()) << k;
            }
            *b = acc;
        }
    }
}

/// Wrap `data` (an encrypted frame) for advertising on `channel` (37 for the vendor protocol).
///
/// Writes `data.len() + 8` bytes to `out` and returns that count: the manufacturer data to advertise under
/// [`COMPANY_ID`]. `None` when `data` is longer than [`MAX_DATA`] or `out` is too short.
pub fn on_air(channel: u8, data: &[u8], out: &mut [u8]) -> Option<usize> {
    let n = data.len();
    if n > MAX_DATA {
        return None;
    }
    let mut buf = [0u8; MAX_DATA + 20];
    // bytes 0..12 stand for the radio's own header, advertiser address and AD header: they only advance the whitening
    buf.get_mut(12..18)?
        .copy_from_slice(&[0x71, 0x0f, 0x55, KEY[2], KEY[1], KEY[0]]);
    buf.get_mut(18..18 + n)?.copy_from_slice(data);
    for b in buf.get_mut(12..18)? {
        *b = b.reverse_bits();
    }
    buf.get_mut(18 + n..20 + n)?
        .copy_from_slice(&crc16(data).to_le_bytes());
    Lfsr::new(0x3f).whiten(buf.get_mut(15..20 + n)?);
    Lfsr::new(channel).whiten(buf.get_mut(..20 + n)?);
    out.get_mut(..n + 8)?.copy_from_slice(buf.get(12..20 + n)?);
    Some(n + 8)
}

/// What a lamp reports about itself in an advertisement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    /// Frame type: [`kind::DISCOVER`], [`kind::STATUS`] or [`kind::LINE_ORDER_REPLY`].
    pub kind: u8,
    /// Current mode: 0 for a static colour, 1 to 12 for a built-in effect.
    pub mode: u8,
    /// Brightness, 0 to 100.
    pub brightness: u8,
    /// The lamp's own id (three bytes), or the line order in a [`kind::LINE_ORDER_REPLY`] (its first byte).
    pub id: [u8; 3],
    /// Group.
    pub group: u8,
    /// Powered on.
    pub on: bool,
    /// Two vendor bytes that identify the product family.
    pub family: [u8; 2],
    /// The sender's random code, echoed back.
    pub code: [u8; 3],
}

/// Decode manufacturer data received from a lamp (the 16 bytes after the company id). `None` if it is not an
/// iStrip frame.
pub fn parse_status(manufacturer_data: &[u8]) -> Option<Status> {
    let raw = <&[u8; 16]>::try_from(manufacturer_data).ok()?;
    let [mode, p0, p1, p2, kind, brightness, i0, i1, i2, fam_a, group, flags, fam_b, c0, c1, c2] =
        decrypt(raw);
    ([p0, p1, p2] == PRODUCT).then_some(Status {
        kind,
        mode,
        brightness,
        id: [i0, i1, i2],
        group,
        on: flags & 1 == 1,
        family: [fam_a, fam_b],
        code: [c0, c1, c2],
    })
}

/// Builds command frames (the plain 16 bytes, before [`encrypt`]).
#[derive(Debug, Clone, Copy)]
pub struct Sender {
    /// Group to address; lamps ignore other groups in broadcast mode.
    pub group: u8,
    /// The sender's three-byte random code (the app keeps one per phone).
    pub code: [u8; 3],
}

impl Sender {
    fn frame(&self, kind: u8, rand: u8, index: u8) -> [u8; 16] {
        let mut f = [0u8; 16];
        f[0] = rand;
        [f[1], f[2], f[3]] = PRODUCT;
        f[4] = kind;
        f[5] = index;
        f[10] = self.group;
        [f[13], f[14], f[15]] = self.code;
        f
    }

    /// A static colour. `rand` and `index` vary per frame so repeats are not mistaken for duplicates.
    pub fn colour(&self, rand: u8, index: u8, rgb: [u8; 3], brightness: u8) -> [u8; 16] {
        let mut f = self.frame(kind::COMMAND, rand, index);
        [f[7], f[8], f[9]] = rgb;
        f[12] = brightness;
        f
    }

    /// A built-in effect (`mode` 1 to 12).
    pub fn effect(&self, rand: u8, index: u8, mode: u8, brightness: u8, speed: u8) -> [u8; 16] {
        let mut f = self.frame(kind::COMMAND, rand, index);
        f[6] = mode;
        f[7] = brightness;
        f[8] = speed;
        f[12] = brightness;
        f
    }

    /// Off.
    pub fn off(&self, rand: u8, index: u8) -> [u8; 16] {
        self.frame(kind::COMMAND, rand, index)
    }

    /// Set the LED line order (0 to 5).
    pub fn line_order(&self, rand: u8, index: u8, order: u8) -> [u8; 16] {
        let mut f = self.frame(kind::LINE_ORDER, rand, index);
        f[6] = order;
        f
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Golden vectors: the real libWirelessEncoder.so (arm64) run under CPU emulation, 2026-10-15.
    const CIPHER: [([u8; 16], [u8; 16]); 5] = [
        (
            [0x00; 16],
            [
                0x42, 0x5b, 0xe0, 0xf2, 0x64, 0xa9, 0x1e, 0x71, 0x25, 0x8c, 0xbf, 0xea, 0x3e, 0xc6,
                0x97, 0x03,
            ],
        ),
        (
            [
                0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
                0x0e, 0x0f,
            ],
            [
                0x42, 0x5a, 0xe1, 0xf3, 0x65, 0xa8, 0x1f, 0x70, 0x24, 0x8d, 0xbe, 0xeb, 0x3f, 0xc7,
                0x96, 0x02,
            ],
        ),
        (
            [0xff; 16],
            [
                0x49, 0x63, 0xd8, 0xca, 0x5c, 0x91, 0x26, 0x49, 0x1d, 0xb4, 0x87, 0xd2, 0x06, 0xfe,
                0xaf, 0x3b,
            ],
        ),
        (
            [
                0x3c, 0xa3, 0x34, 0x72, 0xd7, 0xfb, 0xe1, 0x7a, 0x01, 0x29, 0x38, 0x93, 0x32, 0xe6,
                0x05, 0xfb,
            ],
            [
                0x06, 0x34, 0xd9, 0xa4, 0x09, 0x05, 0xf0, 0x40, 0x2a, 0x2c, 0x38, 0x99, 0x09, 0x1a,
                0x20, 0x9d,
            ],
        ),
        (
            [
                0xa0, 0x6b, 0xcb, 0x80, 0xb2, 0xb6, 0xc0, 0x27, 0xae, 0x2d, 0x95, 0x93, 0xea, 0x48,
                0x9e, 0x0c,
            ],
            [
                0xa2, 0xe0, 0xc0, 0x67, 0x76, 0xed, 0x64, 0x56, 0xe2, 0x43, 0x67, 0xa4, 0x09, 0xe8,
                0xa1, 0xdd,
            ],
        ),
    ];

    const AIR: [(u8, &[u8], &[u8]); 5] = [
        (
            37,
            &[
                0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
                0x0e, 0x0f,
            ],
            &[
                0x18, 0x87, 0x52, 0x1e, 0x45, 0xf9, 0xfc, 0x70, 0xa1, 0xf7, 0x51, 0x6d, 0xc9, 0xae,
                0x11, 0x65, 0x57, 0x47, 0x08, 0x9f, 0xeb, 0x12, 0xba, 0x58,
            ],
        ),
        (
            38,
            &[
                0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
                0x0e, 0x0f,
            ],
            &[
                0xf5, 0xbe, 0x67, 0x9d, 0xe8, 0x72, 0x75, 0x30, 0x13, 0x4b, 0x92, 0x72, 0xfe, 0xe4,
                0x4e, 0xe0, 0xa1, 0xdb, 0x92, 0x5e, 0x3d, 0xd7, 0xfe, 0x78,
            ],
        ),
        (
            39,
            &[0xaa; 16],
            &[
                0xae, 0xa9, 0x74, 0x1c, 0x8c, 0x0b, 0x58, 0xa4, 0xd5, 0x76, 0x82, 0xd7, 0xbf, 0x70,
                0xd9, 0xc0, 0xac, 0xf1, 0xbd, 0xb9, 0x2b, 0xce, 0x7d, 0x27,
            ],
        ),
        (
            37,
            &[
                0xbc, 0xba, 0xec, 0xd8, 0x2e, 0xcc, 0xff, 0x3b, 0xd9, 0xfb, 0xcb, 0x84, 0xd7, 0xf5,
                0x0c, 0x72,
            ],
            &[
                0x18, 0x87, 0x52, 0x1e, 0x45, 0xf9, 0x40, 0xcb, 0x4f, 0x2c, 0x7b, 0xa4, 0x30, 0x92,
                0xc0, 0x97, 0x96, 0xc8, 0xd3, 0x67, 0xe9, 0x6f, 0xda, 0x21,
            ],
        ),
        (
            37,
            &[0x00; 5],
            &[
                0x18, 0x87, 0x52, 0x1e, 0x45, 0xf9, 0xfc, 0x71, 0xa3, 0xf4, 0x55, 0x59, 0x88,
            ],
        ),
    ];

    #[test]
    fn the_cipher_matches_the_vendor_library() {
        for (plain, wire) in CIPHER {
            assert_eq!(encrypt(&plain), wire);
            assert_eq!(decrypt(&wire), plain);
        }
    }

    #[test]
    fn the_on_air_framing_matches_the_vendor_library() {
        for (channel, data, expect) in AIR {
            let mut out = [0u8; 64];
            let n = on_air(channel, data, &mut out).unwrap();
            assert_eq!(out.get(..n).unwrap(), expect);
        }
    }

    #[test]
    fn the_cipher_round_trips_everything() {
        let mut x = 1u32;
        for _ in 0..2000 {
            let mut f = [0u8; 16];
            for b in &mut f {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                *b = (x >> 24) as u8;
            }
            assert_eq!(decrypt(&encrypt(&f)), f);
        }
    }

    #[test]
    fn an_app_shaped_colour_frame_survives_the_whole_trip() {
        let s = Sender {
            group: 1,
            code: [0x11, 0x22, 0x33],
        };
        let plain = s.colour(0x2a, 7, [0xff, 0x93, 0x29], 40);
        assert_eq!(plain[..5], [0x2a, 0x54, 0x00, 0x1d, 0xc3]);
        let wire = encrypt(&plain);
        let got = parse_status(&wire).unwrap();
        // a command frame and a status reply share one wire layout but not its meaning: byte 5 is a counter here
        assert_eq!(
            (got.kind, got.group, got.code),
            (kind::COMMAND, 1, [0x11, 0x22, 0x33])
        );
        assert_eq!(decrypt(&wire)[12], 40, "brightness travels in byte 12");
        let mut out = [0u8; 32];
        assert_eq!(on_air(CHANNEL, &wire, &mut out), Some(24));
    }

    #[test]
    fn status_decoding_reads_power_group_and_family() {
        let mut f = [0u8; 16];
        [f[1], f[2], f[3]] = PRODUCT;
        f[4] = kind::STATUS;
        f[0] = 3;
        f[5] = 70;
        f[10] = 2;
        f[11] = 1;
        let s = parse_status(&encrypt(&f)).unwrap();
        assert_eq!((s.mode, s.brightness, s.group, s.on), (3, 70, 2, true));
        assert!(parse_status(&[0u8; 16]).is_none(), "not an iStrip frame");
        assert!(parse_status(&[0u8; 15]).is_none(), "wrong length");
    }

    #[test]
    fn bad_lengths_are_refused_not_panicked() {
        let mut out = [0u8; 8];
        assert_eq!(on_air(CHANNEL, &[0u8; 16], &mut out), None);
        assert_eq!(on_air(CHANNEL, &[0u8; MAX_DATA + 1], &mut [0u8; 99]), None);
    }
}
