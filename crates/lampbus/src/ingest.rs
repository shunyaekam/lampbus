//! Live colour input over UDP, so other programs can drive a lamp without speaking HTTP or holding a socket open.
//!
//! - **simple**: a datagram of 3 or 4 bytes, `r g b [brightness 0-100]`. Trivial to send from any language.
//! - **DDP** (Distributed Display Protocol, used by WLED, HyperHDR, LedFx and xLights): the first pixel of the data at
//!   offset 0 becomes the lamp colour, so tools built for LED strips drive a Bluetooth lamp unchanged.
//!
//! Both are unauthenticated. They are off by default and should be bound to localhost or a private (tailnet) address.

use lampbus_core::Target;
use lampbus_shell::Handle;
use tokio::net::UdpSocket;

/// `r g b` or `r g b brightness`.
pub fn parse_simple(d: &[u8]) -> Option<([f64; 3], f64)> {
    match d {
        [r, g, b] => Some(([f64::from(*r), f64::from(*g), f64::from(*b)], 100.0)),
        [r, g, b, v] => Some((
            [f64::from(*r), f64::from(*g), f64::from(*b)],
            f64::from((*v).min(100)),
        )),
        _ => None,
    }
}

/// The first pixel of a DDP packet's data, if it addresses offset 0.
///
/// Header: flags, sequence, data type, destination id, offset (u32 BE), length (u16 BE), then data; a timecode adds
/// 4 bytes when flag bit `0x10` is set.
pub fn parse_ddp(d: &[u8]) -> Option<[f64; 3]> {
    let flags = *d.first()?;
    if flags >> 6 != 1 {
        return None; // not DDP version 1
    }
    let header = if flags & 0x10 != 0 { 14 } else { 10 };
    let offset = u32::from_be_bytes([*d.get(4)?, *d.get(5)?, *d.get(6)?, *d.get(7)?]);
    let len = usize::from(u16::from_be_bytes([*d.get(8)?, *d.get(9)?]));
    if offset != 0 || len < 3 {
        return None;
    }
    let data = d.get(header..)?;
    match data {
        [r, g, b, ..] => Some([f64::from(*r), f64::from(*g), f64::from(*b)]),
        _ => None,
    }
}

pub async fn run_simple(addr: String, handle: Handle, target: Target) -> std::io::Result<()> {
    let sock = UdpSocket::bind(&addr).await?;
    let mut buf = [0u8; 16];
    loop {
        let (n, _) = sock.recv_from(&mut buf).await?;
        if let Some((rgb, bri)) = buf.get(..n).and_then(parse_simple) {
            handle.stream(target.clone(), rgb, bri);
        }
    }
}

pub async fn run_ddp(addr: String, handle: Handle, target: Target) -> std::io::Result<()> {
    let sock = UdpSocket::bind(&addr).await?;
    let mut buf = [0u8; 1500];
    loop {
        let (n, _) = sock.recv_from(&mut buf).await?;
        if let Some(rgb) = buf.get(..n).and_then(parse_ddp) {
            handle.stream(target.clone(), rgb, 100.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_frames() {
        assert_eq!(parse_simple(&[1, 2, 3]), Some(([1.0, 2.0, 3.0], 100.0)));
        assert_eq!(parse_simple(&[1, 2, 3, 40]), Some(([1.0, 2.0, 3.0], 40.0)));
        assert_eq!(
            parse_simple(&[1, 2, 3, 200]).map(|x| x.1),
            Some(100.0),
            "brightness is capped"
        );
        for bad in [&[][..], &[1][..], &[1, 2][..], &[1, 2, 3, 4, 5][..]] {
            assert!(parse_simple(bad).is_none());
        }
    }

    fn ddp(flags: u8, offset: u32, data: &[u8], timecode: bool) -> Vec<u8> {
        let mut p = vec![flags, 1, 1, 1];
        p.extend_from_slice(&offset.to_be_bytes());
        p.extend_from_slice(&(data.len() as u16).to_be_bytes());
        if timecode {
            p.extend_from_slice(&[0, 0, 0, 0]);
        }
        p.extend_from_slice(data);
        p
    }

    #[test]
    fn ddp_packets() {
        assert_eq!(
            parse_ddp(&ddp(0x41, 0, &[9, 8, 7, 6, 5, 4], false)),
            Some([9.0, 8.0, 7.0]),
            "first pixel of a strip"
        );
        assert_eq!(
            parse_ddp(&ddp(0x51, 0, &[9, 8, 7], true)),
            Some([9.0, 8.0, 7.0]),
            "with timecode"
        );
        assert!(
            parse_ddp(&ddp(0x41, 3, &[9, 8, 7], false)).is_none(),
            "not offset 0"
        );
        assert!(
            parse_ddp(&ddp(0x01, 0, &[9, 8, 7], false)).is_none(),
            "wrong version"
        );
        assert!(
            parse_ddp(&ddp(0x41, 0, &[9, 8], false)).is_none(),
            "too short"
        );
        for n in 0..12 {
            assert!(
                parse_ddp(&vec![0x41; n]).is_none() || n >= 13,
                "truncated packets never panic"
            );
        }
    }
}
