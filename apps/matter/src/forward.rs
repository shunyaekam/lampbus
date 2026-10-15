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
use std::sync::{Mutex, OnceLock};
use std::task::{Poll, Waker};
use std::time::{Duration, Instant};

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
        if let Ok(mut t) = LAST_SENT.lock() {
            *t = Some(Instant::now());
        }
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

// ---- lamp -> Matter: keep the Matter OnOff state equal to the real lamp ---------------------------------------------

struct Inbox {
    value: Option<bool>,
    waker: Option<Waker>,
}

static INBOX: Mutex<Inbox> = Mutex::new(Inbox {
    value: None,
    waker: None,
});
static LAST_SENT: Mutex<Option<Instant>> = Mutex::new(None);

fn deliver(on: bool) {
    if let Ok(mut i) = INBOX.lock() {
        i.value = Some(on);
        if let Some(w) = i.waker.take() {
            w.wake();
        }
    }
}

/// Resolves with the lamp's power state whenever it changes (and once at startup). Used by the OnOff cluster's
/// out-of-band hook, so the Matter attribute, and therefore Siri and the Home app, never go stale.
pub async fn lamp_state_change() -> bool {
    std::future::poll_fn(|cx| {
        let Ok(mut i) = INBOX.lock() else {
            return Poll::Pending;
        };
        match i.value.take() {
            Some(v) => Poll::Ready(v),
            None => {
                i.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    })
    .await
}

/// FNV-1a: how a long-polling client tells the daemon which state it has already seen (same function as the daemon's).
pub fn body_hash(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf2_9ce4_8422_2325, |h, &x| {
        (h ^ u64::from(x)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Whether the lamp(s) we expose are on, from a `/api/state` body. `device` restricts to one alias.
pub fn lamp_on(body: &str, device: Option<&str>) -> Option<bool> {
    let v: Value = serde_json::from_str(body).ok()?;
    let mut any = None;
    for l in v.get("lamps")?.as_array()? {
        if device.is_some_and(|d| l.get("alias").and_then(Value::as_str) != Some(d)) {
            continue;
        }
        any = Some(any.unwrap_or(false) || l.get("on").and_then(Value::as_bool)?);
    }
    any
}

fn get_state(addr: &str, token: &str, wait: Option<u64>) -> Result<String, String> {
    let mut s =
        TcpStream::connect(addr).map_err(|e| format!("cannot reach lampbus at {addr}: {e}"))?;
    s.set_read_timeout(Some(Duration::from_secs(70)))
        .map_err(|e| e.to_string())?;
    let q = wait.map_or_else(String::new, |w| format!("?wait={w}"));
    let req = format!(
        "GET /api/state{q} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
    );
    s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut raw = String::new();
    s.read_to_string(&mut raw).map_err(|e| e.to_string())?;
    let (head, body) = raw.split_once("\r\n\r\n").ok_or("no response")?;
    if !head
        .split_whitespace()
        .nth(1)
        .is_some_and(|c| c.starts_with('2'))
    {
        return Err(head.lines().next().unwrap_or("bad response").to_string());
    }
    Ok(body.to_string())
}

/// Follow the daemon's state with long polling (no timer: the request returns when something changes, or after 50 s)
/// and hand power changes to the OnOff cluster. Changes within 1.5 s of a command we forwarded ourselves are
/// re-examined later, so a half-applied command can never flip the Matter state back.
pub fn follow(addr: String, token: String, device: Option<String>) {
    std::thread::spawn(move || {
        let (mut seen, mut last_on): (Option<u64>, Option<bool>) = (None, None);
        loop {
            match get_state(&addr, &token, seen) {
                Err(_) => {
                    seen = None;
                    std::thread::sleep(Duration::from_secs(5));
                }
                Ok(body) => {
                    let quiet = LAST_SENT
                        .lock()
                        .ok()
                        .and_then(|t| *t)
                        .map_or(Duration::ZERO, |t| {
                            Duration::from_millis(1500).saturating_sub(t.elapsed())
                        });
                    if !quiet.is_zero() {
                        std::thread::sleep(quiet);
                        continue;
                    }
                    seen = Some(body_hash(body.as_bytes()));
                    if let Some(on) = lamp_on(&body, device.as_deref()) {
                        if last_on != Some(on) {
                            last_on = Some(on);
                            deliver(on);
                        }
                    }
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
    fn lamp_power_is_read_from_the_state_body() {
        let s = r#"{"lamps":[{"alias":"a","on":false},{"alias":"b","on":true}],"scanning":false}"#;
        assert_eq!(lamp_on(s, None), Some(true));
        assert_eq!(lamp_on(s, Some("a")), Some(false));
        assert_eq!(lamp_on(s, Some("zzz")), None);
        assert_eq!(lamp_on("{}", None), None);
        assert_eq!(lamp_on(r#"{"lamps":[]}"#, None), None);
    }

    #[test]
    fn the_hash_matches_the_daemons() {
        // FNV-1a 64 of "a"; the daemon pins the same value in its own test
        assert_eq!(body_hash(b"a"), 0xaf63_dc4c_8601_ec8c);
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
