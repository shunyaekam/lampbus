//! Look at a Bluetooth device the way a lamp definition author needs to: its name, GATT table, readable values and
//! whatever it announces. Read-only: nothing is ever written.

use btleplug::api::{Central, CharPropFlags, Manager as _, Peripheral as _, ScanFilter};
use btleplug::platform::{Manager, Peripheral};
use futures::StreamExt;
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::time::Duration;

fn hex(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{x:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Scan for a few seconds. With no `name`, list the named devices heard. With one (case-insensitive prefix), connect
/// and describe it, then listen for `listen` for anything it announces.
pub async fn gatt(name: Option<&str>, listen: Duration) -> Result<String, String> {
    let manager = Manager::new().await.map_err(err)?;
    let central = manager
        .adapters()
        .await
        .map_err(err)?
        .into_iter()
        .next()
        .ok_or("no Bluetooth adapter found")?;
    central
        .start_scan(ScanFilter::default())
        .await
        .map_err(err)?;
    let want = name.map(str::to_lowercase);
    let mut heard = BTreeSet::new();
    let mut found: Option<(String, Peripheral)> = None;
    for round in 0..32 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        for p in central.peripherals().await.map_err(err)? {
            let Some(n) = p
                .properties()
                .await
                .ok()
                .flatten()
                .and_then(|x| x.local_name)
            else {
                continue;
            };
            heard.insert(n.clone());
            if want
                .as_ref()
                .is_some_and(|w| n.to_lowercase().starts_with(w))
            {
                found = Some((n, p));
            }
        }
        if found.is_some() || (want.is_none() && round >= 23) {
            break;
        }
    }
    let _ = central.stop_scan().await;
    let mut out = String::new();
    let Some((n, lamp)) = found else {
        if want.is_some() {
            let _ = writeln!(out, "not found. Devices heard: {}", join(&heard));
            return Err(out.trim_end().to_string());
        }
        let _ = writeln!(out, "devices heard: {}", join(&heard));
        return Ok(out);
    };
    lamp.connect().await.map_err(|e| {
        format!("cannot connect ({e}); the lamp takes one connection at a time: close the vendor app and stop `lampbus serve`")
    })?;
    lamp.discover_services().await.map_err(err)?;
    let _ = writeln!(out, "{n}");
    for s in lamp.services() {
        let _ = writeln!(out, "  service {}", s.uuid);
        for c in &s.characteristics {
            let _ = write!(out, "    {}  {:?}", c.uuid, c.properties);
            if c.properties.contains(CharPropFlags::READ) {
                match lamp.read(c).await {
                    Ok(v) => {
                        let _ = write!(out, "  = {}  {:?}", hex(&v), String::from_utf8_lossy(&v));
                    }
                    Err(e) => {
                        let _ = write!(out, "  (read failed: {e})");
                    }
                }
            }
            let _ = writeln!(out);
        }
    }
    let mut notes = lamp.notifications().await.map_err(err)?;
    for c in lamp.characteristics() {
        if c.properties
            .intersects(CharPropFlags::NOTIFY | CharPropFlags::INDICATE)
        {
            let _ = lamp.subscribe(&c).await;
        }
    }
    let until = tokio::time::Instant::now() + listen;
    while let Ok(Some(v)) = tokio::time::timeout_at(until, notes.next()).await {
        let _ = writeln!(out, "  announced on {}: {}", v.uuid, hex(&v.value));
    }
    let _ = lamp.disconnect().await;
    Ok(out)
}

fn join(set: &BTreeSet<String>) -> String {
    set.iter().cloned().collect::<Vec<_>>().join(", ")
}
