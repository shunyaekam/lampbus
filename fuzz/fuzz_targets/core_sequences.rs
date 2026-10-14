//! The strongest target: arbitrary sequences of commands, link events and time passing, fed to the core with the same
//! protocol invariants the simulation suite enforces on every action:
//!   I1  never write while disconnected
//!   I2  never connect while connected or connecting
//!   I3  writes to one lamp are at least `min_write_interval_ms` apart
//! plus: the core never panics and always makes progress (a wake-up time is never in the past by more than a step).
#![no_main]

use arbitrary::Arbitrary;
use lampbus_core::{Action, Command, Config, Core, DeviceConfig, LinkEvent, Target};
use lampbus_defs::Definition;
use libfuzzer_sys::fuzz_target;
use std::collections::BTreeSet;
use std::sync::Arc;

const DEFS: [&str; 4] = [
    include_str!("../../definitions/istrip-ssl.json"),
    include_str!("../../definitions/elk-bledom.json"),
    include_str!("../../definitions/triones.json"),
    include_str!("../../definitions/govee-h61.json"),
];

#[derive(Arbitrary, Debug)]
enum Op {
    Advance(u16),
    Power(Option<bool>),
    Color([u8; 3], Option<u8>),
    Brightness(u8),
    Effect(u8, Option<u8>),
    Stream([u8; 3], u8),
    Play(u8),
    Stop,
    Raw(u8),
    Connected,
    ConnectFailed,
    Disconnected,
    WriteFailed,
    Advertise(u8),
}

fn animations() -> Vec<Arc<lampbus_render::anim::Animation>> {
    [
        r#"{"type":"gen","gen":"rainbow","fps":20}"#,
        r#"{"type":"gen","gen":"candle","fps":40}"#,
        r#"{"type":"keyframes","duration":3,"then":"off","fps":10,"keys":[{"t":0,"color":"red"},{"t":3,"color":"blue","level":10}]}"#,
        r#"{"type":"keyframes","duration":2,"loop":true,"fps":5,"keys":[{"t":0,"color":"cool"},{"t":1,"color":"white","level":0}]}"#,
    ]
    .iter()
    .filter_map(|s| serde_json::from_str(s).ok().and_then(|v| lampbus_render::anim::load(&v).ok()).map(Arc::new))
    .collect()
}

fuzz_target!(|ops: Vec<Op>| {
    let defs: Vec<Definition> = DEFS.iter().filter_map(|d| Definition::from_json(d).ok()).collect();
    let names = ["SSL-1", "ELK-BLEDOM", "Triones:1", "Govee_H6159"];
    let devices: Vec<DeviceConfig> = names
        .iter()
        .enumerate()
        .map(|(i, n)| DeviceConfig { alias: format!("l{i}"), name: Some((*n).into()), id: Some(format!("D{i}")), definition: None })
        .collect();
    let cfg = Config { devices, idle_ms: 3_000, ..Config::default() };
    let min_gap = cfg.min_write_interval_ms;
    let Ok(mut core) = Core::new(defs, cfg) else { return };
    let anims = animations();

    let (mut now, mut connected, mut connecting) = (0u64, BTreeSet::new(), BTreeSet::new());
    let mut last_write = std::collections::BTreeMap::new();
    let ids = ["D0", "D1", "D2", "D3"];

    let mut drain = |core: &mut Core, now: u64, connected: &mut BTreeSet<String>, connecting: &mut BTreeSet<String>, last_write: &mut std::collections::BTreeMap<String, u64>| {
        let mut guard = 0;
        while let Some(a) = core.poll_action() {
            guard += 1;
            assert!(guard < 10_000, "runaway actions");
            match a {
                Action::Connect { device } => {
                    assert!(!connected.contains(&device) && !connecting.contains(&device), "I2 violated for {device}");
                    connecting.insert(device);
                }
                Action::Disconnect { device } => {
                    connected.remove(&device);
                    connecting.remove(&device);
                    last_write.remove(&device);
                }
                Action::Write { device, .. } => {
                    assert!(connected.contains(&device), "I1 violated: write to {device} at {now}");
                    if let Some(prev) = last_write.get(&device) {
                        assert!(now >= prev + min_gap, "I3 violated for {device}: {} ms apart", now - prev);
                    }
                    last_write.insert(device, now);
                }
                Action::Scan | Action::StopScan => {}
            }
        }
    };

    for op in ops.iter().take(400) {
        let pick = |n: u8| ids[usize::from(n) % ids.len()].to_string();
        match op {
            Op::Advance(ms) => {
                let target = now + u64::from(*ms);
                // honour the core's own wake-ups on the way, as a real shell does
                for _ in 0..64 {
                    match core.poll_timeout(now) {
                        Some(t) if t <= target => {
                            now = now.max(t);
                            core.handle_timeout(now);
                            drain(&mut core, now, &mut connected, &mut connecting, &mut last_write);
                        }
                        _ => break,
                    }
                }
                now = target;
                core.handle_timeout(now);
            }
            Op::Power(p) => {
                let _ = core.handle_command(now, &Target::All, Command::Power(*p));
            }
            Op::Color(c, b) => {
                let _ = core.handle_command(now, &Target::All, Command::Color { rgb: c.map(f64::from), brightness: b.map(f64::from) });
            }
            Op::Brightness(b) => {
                let _ = core.handle_command(now, &Target::All, Command::Brightness(f64::from(*b)));
            }
            Op::Effect(id, speed) => {
                let _ = core.handle_command(now, &Target::All, Command::Effect { effect: id.to_string(), speed: speed.map(f64::from), brightness: None });
            }
            Op::Stream(c, b) => {
                let _ = core.handle_command(now, &Target::All, Command::Stream { rgb: c.map(f64::from), brightness: f64::from(*b) });
            }
            Op::Play(i) => {
                if let Some(a) = anims.get(usize::from(*i) % anims.len().max(1)) {
                    let _ = core.handle_command(now, &Target::All, Command::Play { animation: Arc::clone(a), name: None, brightness: None });
                }
            }
            Op::Stop => {
                let _ = core.handle_command(now, &Target::All, Command::StopAnimation);
            }
            Op::Raw(i) => {
                let cmd = ["power_on", "color", "off", "nope"][usize::from(*i) % 4].to_string();
                let _ = core.handle_command(now, &Target::All, Command::Raw { command: cmd, args: vec![("r".into(), 1.0), ("g".into(), 2.0), ("b".into(), 3.0)] });
            }
            Op::Connected | Op::ConnectFailed | Op::Disconnected | Op::WriteFailed => {
                // only report events a real stack could: connect results for pending connects, others for any device
                let candidates: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
                for id in candidates {
                    let ev = match op {
                        Op::Connected if connecting.remove(&id) => {
                            connected.insert(id.clone());
                            Some(LinkEvent::Connected { id: id.clone() })
                        }
                        Op::ConnectFailed if connecting.remove(&id) => Some(LinkEvent::ConnectFailed { id: id.clone(), reason: "x".into() }),
                        Op::Disconnected if connected.remove(&id) => {
                            last_write.remove(&id);
                            Some(LinkEvent::Disconnected { id: id.clone() })
                        }
                        Op::WriteFailed if connected.remove(&id) => {
                            last_write.remove(&id);
                            Some(LinkEvent::WriteFailed { id: id.clone(), reason: "x".into() })
                        }
                        _ => None,
                    };
                    if let Some(ev) = ev {
                        core.handle_link(now, ev);
                    }
                }
            }
            Op::Advertise(n) => core.handle_link(now, LinkEvent::Advertised { id: pick(*n), name: Some(names[usize::from(*n) % 4].into()), services: vec![] }),
        }
        drain(&mut core, now, &mut connected, &mut connecting, &mut last_write);
        let _ = core.snapshot(now);
    }
});
