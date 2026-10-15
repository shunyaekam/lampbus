//! Forward Matter commands to a running `lampbus serve` through its control language (`POST /api/control`).
//!
//! Matter sends power, level and colour as separate commands in quick succession (for example "move to colour" then
//! "move to level"). They are coalesced for a few milliseconds into one control object, so the lamp sees one change.

#![forbid(unsafe_code)]

use serde_json::{json, Map, Value};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::OnceLock;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Msg {
    Power(bool),
    /// Matter level, 1..=254.
    Level(u8),
    /// sRGB-encoded colour.
    Color([u8; 3]),
}

static TX: OnceLock<Sender<Msg>> = OnceLock::new();
/// Off while the Matter stack replays its stored defaults at startup (which must not reach the lamp).
static ENABLED: AtomicBool = AtomicBool::new(false);

/// Start forwarding. Call once startup has finished.
pub fn enable() {
    ENABLED.store(true, Ordering::SeqCst);
}

pub fn power(on: bool) {
    send(Msg::Power(on));
}

pub fn level(l: u8) {
    send(Msg::Level(l));
}

pub fn color(rgb: [u8; 3]) {
    send(Msg::Color(rgb));
}

fn send(m: Msg) {
    if !ENABLED.load(Ordering::SeqCst) {
        return;
    }
    if let Some(tx) = TX.get() {
        let _ = tx.send(m);
    }
}

/// Combine a burst of messages (later ones win) into one control object.
pub fn compose(batch: &[Msg], device: Option<&str>) -> Option<Value> {
    let (mut power, mut level, mut color) = (None, None, None);
    for m in batch {
        match m {
            Msg::Power(p) => power = Some(*p),
            Msg::Level(l) => level = Some(*l),
            Msg::Color(c) => color = Some(*c),
        }
    }
    let mut o = Map::new();
    if let Some(c) = color {
        o.insert("color".into(), json!(c));
    }
    if let Some(l) = level {
        let pct = (f64::from(l) / 254.0 * 100.0).round().clamp(1.0, 100.0);
        o.insert("brightness".into(), json!(pct));
    }
    if let Some(p) = power {
        o.insert("power".into(), json!(p));
    }
    if o.is_empty() {
        return None;
    }
    if let Some(d) = device {
        o.insert("device".into(), json!(d));
    }
    Some(Value::Object(o))
}

fn post(addr: &str, token: &str, body: &Value) -> Result<(), String> {
    let payload = body.to_string();
    let mut s =
        TcpStream::connect(addr).map_err(|e| format!("cannot reach lampbus at {addr}: {e}"))?;
    s.set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|e| e.to_string())?;
    let req = format!(
        "POST /api/control HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut raw = String::new();
    s.read_to_string(&mut raw).map_err(|e| e.to_string())?;
    let ok = raw
        .split_whitespace()
        .nth(1)
        .is_some_and(|c| c.starts_with('2'));
    if ok {
        Ok(())
    } else {
        Err(raw.lines().last().unwrap_or("no response").to_string())
    }
}

/// Start the worker that delivers coalesced commands.
pub fn start(addr: String, token: String, device: Option<String>) {
    let (tx, rx) = channel::<Msg>();
    if TX.set(tx).is_err() {
        return;
    }
    std::thread::spawn(move || {
        while let Ok(first) = rx.recv() {
            let mut batch = vec![first];
            loop {
                match rx.recv_timeout(Duration::from_millis(40)) {
                    Ok(m) => batch.push(m),
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
            if let Some(body) = compose(&batch, device.as_deref()) {
                if let Err(e) = post(&addr, &token, &body) {
                    eprintln!("lampbus-matter: {e}");
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burst_becomes_one_control_object() {
        let v = compose(
            &[
                Msg::Power(true),
                Msg::Color([1, 2, 3]),
                Msg::Level(127),
                Msg::Color([9, 8, 7]),
            ],
            Some("desk"),
        )
        .unwrap();
        assert_eq!(
            v,
            json!({"color": [9, 8, 7], "brightness": 50.0, "power": true, "device": "desk"})
        );
    }

    #[test]
    fn levels_map_onto_percent_and_never_reach_zero() {
        let b = |l| {
            compose(&[Msg::Level(l)], None).unwrap()["brightness"]
                .as_f64()
                .unwrap()
        };
        assert_eq!((b(1), b(254), b(127)), (1.0, 100.0, 50.0));
        assert_eq!(
            compose(&[Msg::Power(false)], None).unwrap(),
            json!({"power": false})
        );
        assert!(compose(&[], None).is_none());
    }
}
