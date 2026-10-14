//! Deterministic simulation of the core against a fake Bluetooth stack with a virtual clock.
//!
//! The harness enforces protocol invariants on every action the core emits: never write while disconnected, never
//! connect twice, always pace writes. Scenarios then check behaviour against the four real lamp definitions, using the
//! definitions themselves to compute the exact bytes expected on the wire.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use lampbus_core::{Action, Command, Config, Core, DeviceConfig, Error, LinkEvent, Ms, Target};
use lampbus_defs::Definition;
use lampbus_render::anim;
use serde_json::json;
use std::collections::BTreeSet;
use std::sync::Arc;

fn defs() -> Vec<Definition> {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../definitions");
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        let n = p.file_name().unwrap().to_string_lossy().to_string();
        if n.ends_with(".json") && !n.ends_with(".vectors.json") {
            out.push(Definition::from_json(&std::fs::read_to_string(&p).unwrap()).unwrap());
        }
    }
    out
}

fn def(id: &str) -> Definition {
    defs().into_iter().find(|d| d.id == id).unwrap()
}

/// Exact bytes the definition produces for a command.
fn wire(id: &str, cmd: &str, args: &[(&str, f64)]) -> Vec<u8> {
    def(id).encode(cmd, args).unwrap().wire
}

#[derive(Debug, Clone, PartialEq)]
enum Ev {
    Scan,
    StopScan,
    Connect(String),
    Disconnect(String),
    Write(String, Vec<u8>),
}

struct Sim {
    core: Core,
    now: Ms,
    log: Vec<(Ms, Ev)>,
    pending: Vec<(Ms, LinkEvent)>,
    connected: BTreeSet<String>,
    connecting: BTreeSet<String>,
    last_write: std::collections::BTreeMap<String, Ms>,
    min_interval: Ms,
    connect_delay: Ms,
    fail_connects: u32,
    fail_next_write: bool,
    adverts: Vec<(String, Option<String>, Vec<String>)>,
}

fn dev(alias: &str, name: &str, id: &str) -> DeviceConfig {
    DeviceConfig {
        alias: alias.into(),
        name: Some(name.into()),
        id: Some(id.into()),
        definition: None,
    }
}

impl Sim {
    fn new(devices: Vec<DeviceConfig>) -> Sim {
        Sim::with(Config {
            devices,
            ..Config::default()
        })
    }

    fn with(cfg: Config) -> Sim {
        Sim::with_defs(defs(), cfg)
    }

    fn with_defs(defs: Vec<Definition>, cfg: Config) -> Sim {
        let min_interval = cfg.min_write_interval_ms;
        Sim {
            core: Core::new(defs, cfg).unwrap(),
            now: 0,
            log: Vec::new(),
            pending: Vec::new(),
            connected: BTreeSet::new(),
            connecting: BTreeSet::new(),
            last_write: Default::default(),
            min_interval,
            connect_delay: 200,
            fail_connects: 0,
            fail_next_write: false,
            adverts: Vec::new(),
        }
    }

    fn schedule(&mut self, at: Ms, e: LinkEvent) {
        self.pending.push((at, e));
        self.pending.sort_by_key(|(t, _)| *t);
    }

    fn drain(&mut self) {
        while let Some(a) = self.core.poll_action() {
            match a {
                Action::Scan => {
                    self.log.push((self.now, Ev::Scan));
                    for (id, name, services) in self.adverts.clone() {
                        self.schedule(self.now + 50, LinkEvent::Advertised { id, name, services });
                    }
                }
                Action::StopScan => self.log.push((self.now, Ev::StopScan)),
                Action::Connect { device } => {
                    assert!(
                        !self.connected.contains(&device),
                        "connect while connected: {device}"
                    );
                    assert!(
                        !self.connecting.contains(&device),
                        "connect while connecting: {device}"
                    );
                    self.connecting.insert(device.clone());
                    self.log.push((self.now, Ev::Connect(device.clone())));
                    let at = self.now + self.connect_delay;
                    if self.fail_connects > 0 {
                        self.fail_connects -= 1;
                        self.schedule(
                            at,
                            LinkEvent::ConnectFailed {
                                id: device,
                                reason: "boom".into(),
                            },
                        );
                    } else {
                        self.schedule(at, LinkEvent::Connected { id: device });
                    }
                }
                Action::Disconnect { device } => {
                    self.log.push((self.now, Ev::Disconnect(device.clone())));
                    self.connected.remove(&device);
                    self.connecting.remove(&device);
                    self.last_write.remove(&device);
                    self.schedule(self.now, LinkEvent::Disconnected { id: device });
                }
                Action::Write { device, data, .. } => {
                    assert!(
                        self.connected.contains(&device),
                        "write while not connected at {}",
                        self.now
                    );
                    if let Some(prev) = self.last_write.get(&device) {
                        assert!(
                            self.now >= prev + self.min_interval,
                            "writes {}ms apart at {}",
                            self.now - prev,
                            self.now
                        );
                    }
                    self.last_write.insert(device.clone(), self.now);
                    self.log.push((self.now, Ev::Write(device.clone(), data)));
                    if self.fail_next_write {
                        self.fail_next_write = false;
                        self.schedule(
                            self.now,
                            LinkEvent::WriteFailed {
                                id: device,
                                reason: "gatt error".into(),
                            },
                        );
                    }
                }
            }
        }
    }

    fn deliver(&mut self, e: LinkEvent) {
        match &e {
            LinkEvent::Connected { id } => {
                self.connecting.remove(id);
                self.connected.insert(id.clone());
            }
            LinkEvent::ConnectFailed { id, .. }
            | LinkEvent::Disconnected { id }
            | LinkEvent::WriteFailed { id, .. } => {
                self.connecting.remove(id);
                self.connected.remove(id);
                self.last_write.remove(id);
            }
            LinkEvent::Advertised { .. } => {}
        }
        self.core.handle_link(self.now, e);
        self.drain();
    }

    fn run_until(&mut self, t: Ms) {
        for _ in 0..200_000 {
            let next_event = self.pending.first().map(|(at, _)| *at);
            let next_timer = self.core.poll_timeout(self.now);
            let Some(next) = [next_event, next_timer]
                .into_iter()
                .flatten()
                .min()
                .filter(|n| *n <= t)
            else {
                break;
            };
            self.now = self.now.max(next);
            while self.pending.first().is_some_and(|(at, _)| *at <= self.now) {
                let (_, e) = self.pending.remove(0);
                self.deliver(e);
            }
            self.core.handle_timeout(self.now);
            self.drain();
        }
        self.now = self.now.max(t);
        self.core.handle_timeout(self.now);
        self.drain();
    }

    fn advance(&mut self, ms: Ms) {
        self.run_until(self.now + ms);
    }

    fn cmd(&mut self, target: &Target, c: Command) -> Result<usize, Error> {
        let r = self.core.handle_command(self.now, target, c);
        self.drain();
        r
    }

    fn all(&mut self, c: Command) {
        self.cmd(&Target::All, c).unwrap();
    }

    fn writes(&self, device: &str) -> Vec<Vec<u8>> {
        self.log
            .iter()
            .filter_map(|(_, e)| match e {
                Ev::Write(d, b) if d == device => Some(b.clone()),
                _ => None,
            })
            .collect()
    }

    fn count(&self, f: impl Fn(&Ev) -> bool) -> usize {
        self.log.iter().filter(|(_, e)| f(e)).count()
    }

    fn mark(&self) -> usize {
        self.log.len()
    }

    fn since(&self, mark: usize) -> Vec<&Ev> {
        self.log[mark..].iter().map(|(_, e)| e).collect()
    }

    fn writes_since(&self, mark: usize, device: &str) -> Vec<Vec<u8>> {
        self.since(mark)
            .into_iter()
            .filter_map(|e| match e {
                Ev::Write(d, b) if d == device => Some(b.clone()),
                _ => None,
            })
            .collect()
    }
}

fn color(rgb: [f64; 3], brightness: Option<f64>) -> Command {
    Command::Color { rgb, brightness }
}

fn animation(spec: serde_json::Value) -> Arc<anim::Animation> {
    Arc::new(anim::load(&spec).unwrap())
}

const ISTRIP: &str = "istrip-ssl";

// ---- lowering across lamp families ---------------------------------------------------------------------------------

#[test]
fn elk_connects_on_demand_then_sends_power_brightness_colour_in_order() {
    let mut s = Sim::new(vec![dev("elk", "ELK-BLEDOM", "E1")]);
    s.all(color([255.0, 0.0, 0.0], Some(50.0)));
    assert_eq!(s.count(|e| matches!(e, Ev::Connect(_))), 1);
    assert!(
        s.writes("E1").is_empty(),
        "nothing is written before the link is up"
    );
    s.advance(1000);
    assert_eq!(
        s.writes("E1"),
        vec![
            wire("elk-bledom", "power_on", &[]),
            wire("elk-bledom", "brightness", &[("level", 50.0)]),
            wire(
                "elk-bledom",
                "color",
                &[("r", 255.0), ("g", 0.0), ("b", 0.0)]
            ),
        ]
    );
}

#[test]
fn istrip_colour_is_one_frame_with_inline_brightness() {
    let mut s = Sim::new(vec![dev("lamp", "SSL-E089ED", "S1")]);
    s.all(color([255.0, 0.0, 0.0], Some(30.0)));
    s.advance(1000);
    assert_eq!(
        s.writes("S1"),
        vec![wire(
            ISTRIP,
            "color",
            &[("r", 255.0), ("g", 0.0), ("b", 0.0), ("brightness", 30.0)]
        )]
    );
}

#[test]
fn triones_scales_the_colour_because_it_has_no_brightness_control() {
    let mut s = Sim::new(vec![dev("tri", "Triones:1234", "T1")]);
    s.all(color([200.0, 100.0, 50.0], Some(50.0)));
    s.advance(1000);
    assert_eq!(
        s.writes("T1"),
        vec![
            wire("triones", "power_on", &[]),
            wire(
                "triones",
                "color",
                &[("r", 100.0), ("g", 50.0), ("b", 25.0)]
            )
        ]
    );
}

#[test]
fn govee_brightness_uses_its_own_0_to_255_range() {
    let mut s = Sim::new(vec![dev("g", "Govee_H6159", "G1")]);
    s.all(color([10.0, 20.0, 30.0], Some(50.0)));
    s.advance(1000);
    let w = s.writes("G1");
    assert_eq!(
        w[1],
        wire("govee-h61", "brightness", &[("level", 128.0)]),
        "50% of 255 rounds to 128"
    );
}

#[test]
fn off_uses_the_off_command_and_is_idempotent() {
    let mut s = Sim::new(vec![dev("elk", "ELK-BLEDOM", "E1")]);
    s.all(color([1.0, 2.0, 3.0], Some(100.0)));
    s.advance(500);
    let m = s.mark();
    s.all(Command::Power(Some(false)));
    s.advance(500);
    assert_eq!(
        s.writes_since(m, "E1"),
        vec![wire("elk-bledom", "power_off", &[])]
    );
    let m = s.mark();
    s.all(Command::Power(Some(false)));
    s.advance(500);
    assert!(s.writes_since(m, "E1").is_empty(), "already off");
}

#[test]
fn a_lamp_with_no_off_command_is_turned_off_with_black() {
    let minimal = r#"{
        "schema": "lampbus.device/0", "id": "mini", "name": "Mini",
        "match": {"name_prefix": ["MINI-"]},
        "gatt": {"service": "0000fff0-0000-1000-8000-00805f9b34fb", "write": "0000fff3-0000-1000-8000-00805f9b34fb", "mode": "without-response"},
        "roles": {"color_rgb": "color"},
        "commands": {"color": {"params": {"r": {}, "g": {}, "b": {}}, "frame": [1, "$r", "$g", "$b"]}}
    }"#;
    let mut all = defs();
    all.push(Definition::from_json(minimal).unwrap());
    let mut core = Sim::with_defs(
        all,
        Config {
            devices: vec![dev("m", "MINI-1", "M1")],
            ..Config::default()
        },
    );
    core.all(color([9.0, 8.0, 7.0], None));
    core.advance(500);
    core.all(Command::Power(Some(false)));
    core.advance(500);
    assert_eq!(core.writes("M1"), vec![vec![1, 9, 8, 7], vec![1, 0, 0, 0]]);
}

#[test]
fn effects_by_alias_id_and_name_and_unsupported_where_missing() {
    let mut s = Sim::new(vec![
        dev("lamp", "SSL-E089ED", "S1"),
        dev("elk", "ELK-BLEDOM", "E1"),
    ]);
    let eff = |e: &str| Command::Effect {
        effect: e.into(),
        speed: None,
        brightness: None,
    };
    s.cmd(&Target::Named("lamp".into()), eff("rainbow"))
        .unwrap();
    s.advance(500);
    assert_eq!(
        s.writes("S1"),
        vec![wire(ISTRIP, "effect", &[("mode", 1.0)])]
    );
    s.cmd(&Target::Named("lamp".into()), eff("Flash7")).unwrap();
    s.cmd(&Target::Named("lamp".into()), eff("12")).unwrap();
    assert!(matches!(
        s.cmd(&Target::Named("lamp".into()), eff("nope")),
        Err(Error::Bad(_))
    ));
    assert!(matches!(
        s.cmd(&Target::Named("lamp".into()), eff("13")),
        Err(Error::Bad(_))
    ));
    assert!(matches!(
        s.cmd(&Target::Named("elk".into()), eff("rainbow")),
        Err(Error::Unsupported(_))
    ));
    // All: applies where it can, silently skips where it cannot
    assert_eq!(s.cmd(&Target::All, eff("rainbow")).unwrap(), 1);
}

// ---- state tracking and traffic reduction -------------------------------------------------------------------------

#[test]
fn only_the_newest_command_is_sent_after_a_slow_connect() {
    let mut s = Sim::new(vec![dev("lamp", "SSL-E089ED", "S1")]);
    s.connect_delay = 500;
    for r in [10.0, 20.0, 30.0, 40.0] {
        s.all(color([r, 0.0, 0.0], None));
        s.advance(50);
    }
    s.advance(2000);
    assert_eq!(
        s.writes("S1"),
        vec![wire(
            ISTRIP,
            "color",
            &[("r", 40.0), ("g", 0.0), ("b", 0.0)]
        )]
    );
}

#[test]
fn unchanged_state_sends_nothing_and_only_changed_parts_are_resent() {
    let mut s = Sim::new(vec![dev("elk", "ELK-BLEDOM", "E1")]);
    s.all(color([255.0, 0.0, 0.0], Some(100.0)));
    s.advance(1000);
    let m = s.mark();
    s.all(color([255.0, 0.0, 0.0], Some(100.0)));
    s.advance(500);
    assert!(s.writes_since(m, "E1").is_empty());
    s.all(Command::Brightness(40.0));
    s.advance(500);
    assert_eq!(
        s.writes_since(m, "E1"),
        vec![wire("elk-bledom", "brightness", &[("level", 40.0)])],
        "brightness only"
    );
}

#[test]
fn idle_lamps_disconnect_and_a_new_command_reconnects_and_resends_everything() {
    let mut s = Sim::with(Config {
        devices: vec![dev("elk", "ELK-BLEDOM", "E1")],
        idle_ms: 5_000,
        ..Config::default()
    });
    s.all(color([255.0, 0.0, 0.0], Some(100.0)));
    s.advance(4_000);
    assert_eq!(
        s.count(|e| matches!(e, Ev::Disconnect(_))),
        0,
        "still within the idle window"
    );
    s.advance(3_000);
    assert_eq!(s.count(|e| matches!(e, Ev::Disconnect(_))), 1);
    let m = s.mark();
    s.all(Command::Brightness(40.0));
    s.advance(1_000);
    assert_eq!(
        s.writes_since(m, "E1"),
        vec![
            wire("elk-bledom", "power_on", &[]),
            wire("elk-bledom", "brightness", &[("level", 40.0)]),
            wire(
                "elk-bledom",
                "color",
                &[("r", 255.0), ("g", 0.0), ("b", 0.0)]
            ),
        ],
        "after a reconnect the core cannot assume anything, so it resends full state"
    );
}

#[test]
fn raw_commands_bypass_the_state_tracking() {
    let mut s = Sim::new(vec![dev("elk", "ELK-BLEDOM", "E1")]);
    s.all(Command::Raw {
        command: "power_on".into(),
        args: vec![],
    });
    s.advance(500);
    assert_eq!(s.writes("E1"), vec![wire("elk-bledom", "power_on", &[])]);
    let r = s.cmd(
        &Target::All,
        Command::Raw {
            command: "nope".into(),
            args: vec![],
        },
    );
    assert!(matches!(r, Err(Error::Bad(_))));
}

// ---- link management -----------------------------------------------------------------------------------------------

#[test]
fn connect_failures_back_off_exponentially_then_succeed() {
    let mut s = Sim::new(vec![dev("lamp", "SSL-E089ED", "S1")]);
    s.fail_connects = 3;
    s.all(color([1.0, 2.0, 3.0], None));
    s.advance(30_000);
    let connects: Vec<Ms> = s
        .log
        .iter()
        .filter(|(_, e)| matches!(e, Ev::Connect(_)))
        .map(|(t, _)| *t)
        .collect();
    assert_eq!(connects.len(), 4, "{connects:?}");
    let gaps: Vec<Ms> = connects.windows(2).map(|w| w[1] - w[0]).collect();
    // each gap = connect delay (200) + backoff (1s, 2s, 4s)
    assert_eq!(gaps, vec![1_200, 2_200, 4_200]);
    assert_eq!(
        s.writes("S1").len(),
        1,
        "the command still arrives once the lamp is reachable"
    );
}

#[test]
fn a_failed_write_drops_the_link_reconnects_and_resends() {
    let mut s = Sim::new(vec![dev("lamp", "SSL-E089ED", "S1")]);
    s.all(color([1.0, 2.0, 3.0], None));
    s.advance(500);
    s.fail_next_write = true;
    s.all(color([9.0, 9.0, 9.0], None));
    s.advance(10_000);
    let w = s.writes("S1");
    assert_eq!(
        w.last().unwrap(),
        &wire(ISTRIP, "color", &[("r", 9.0), ("g", 9.0), ("b", 9.0)])
    );
    assert_eq!(s.count(|e| matches!(e, Ev::Connect(_))), 2);
    let snap = s.core.snapshot(s.now);
    assert!(snap.lamps[0].connected);
}

#[test]
fn an_unexpected_disconnect_is_recovered_when_something_still_needs_the_lamp() {
    let mut s = Sim::new(vec![dev("lamp", "SSL-E089ED", "S1")]);
    s.all(Command::Play {
        animation: animation(json!({"type":"gen","gen":"rainbow","fps":10})),
        name: Some("r".into()),
        brightness: None,
    });
    s.advance(1_000);
    s.deliver(LinkEvent::Disconnected { id: "S1".into() });
    let m = s.mark();
    s.advance(6_000);
    assert!(s.count(|e| matches!(e, Ev::Connect(_))) >= 2);
    assert!(
        !s.writes_since(m, "S1").is_empty(),
        "animation resumed after reconnecting"
    );
}

// ---- discovery -----------------------------------------------------------------------------------------------------

#[test]
fn a_configured_lamp_waits_for_its_advertisement_then_connects() {
    let mut s = Sim::new(vec![DeviceConfig {
        alias: "lamp".into(),
        name: Some("SSL-E089ED".into()),
        id: None,
        definition: None,
    }]);
    s.adverts = vec![
        ("OTHER".into(), Some("Some TV".into()), vec![]),
        ("S9".into(), Some("SSL-E089ED".into()), vec![]),
    ];
    s.all(color([5.0, 5.0, 5.0], None));
    assert_eq!(s.count(|e| matches!(e, Ev::Scan)), 1);
    s.advance(2_000);
    assert_eq!(s.count(|e| matches!(e, Ev::Connect(d) if d == "S9")), 1);
    assert_eq!(
        s.count(|e| matches!(e, Ev::StopScan)),
        1,
        "scanning stops once nothing needs it"
    );
    assert_eq!(s.writes("S9").len(), 1);
    assert_eq!(s.core.snapshot(s.now).lamps[0].id.as_deref(), Some("S9"));
}

#[test]
fn auto_adopt_adds_only_lamps_that_match_a_definition() {
    let mut s = Sim::with(Config {
        auto_adopt: true,
        ..Config::default()
    });
    s.adverts = vec![
        ("A".into(), Some("ELK-BLEDOM".into()), vec![]),
        ("B".into(), Some("Washer".into()), vec![]),
        (
            "C".into(),
            None,
            vec!["0000af30-0000-1000-8000-00805f9b34fb".into()],
        ),
    ];
    assert!(
        matches!(
            s.cmd(&Target::All, color([1.0; 3], None)),
            Err(Error::NoMatchingDevice(_))
        ),
        "nothing known yet"
    );
    s.core.discover(s.now);
    s.drain();
    s.advance(1_000);
    let snap = s.core.snapshot(s.now);
    let mut aliases: Vec<_> = snap
        .lamps
        .iter()
        .map(|l| (l.alias.clone(), l.definition.clone()))
        .collect();
    aliases.sort();
    assert_eq!(
        aliases,
        vec![
            ("C".to_string(), "istrip-ssl".to_string()),
            ("ELK-BLEDOM".to_string(), "elk-bledom".to_string())
        ]
    );
    s.advance(20_000);
    assert_eq!(s.count(|e| matches!(e, Ev::StopScan)), 1);
}

#[test]
fn a_lamp_that_never_shows_up_backs_off_and_reports_why() {
    let mut s = Sim::new(vec![DeviceConfig {
        alias: "lamp".into(),
        name: Some("SSL-NOPE".into()),
        id: None,
        definition: Some(ISTRIP.into()),
    }]);
    s.all(color([5.0, 5.0, 5.0], None));
    s.advance(15_500); // the scan times out at 15 s; the retry is due at 16 s
    let snap = s.core.snapshot(s.now);
    assert_eq!(
        snap.lamps[0].last_error.as_deref(),
        Some("not found while scanning")
    );
    assert_eq!(snap.lamps[0].link, "backoff");
    assert!(!snap.scanning);
}

#[test]
fn targets_select_by_alias_id_or_all() {
    let mut s = Sim::new(vec![
        dev("desk", "SSL-E089ED", "S1"),
        dev("shelf", "ELK-BLEDOM", "E1"),
    ]);
    s.cmd(&Target::Named("DESK".into()), color([1.0, 1.0, 1.0], None))
        .unwrap();
    s.cmd(&Target::Named("E1".into()), color([2.0, 2.0, 2.0], None))
        .unwrap();
    s.advance(1_000);
    assert_eq!(s.writes("S1").len(), 1);
    assert_eq!(s.writes("E1").len(), 3);
    assert!(matches!(
        s.cmd(&Target::Named("nope".into()), Command::Power(None)),
        Err(Error::NoMatchingDevice(_))
    ));
    assert_eq!(s.cmd(&Target::All, Command::Power(Some(false))).unwrap(), 2);
}

#[test]
fn configuration_errors_are_reported() {
    let bad = |d: DeviceConfig| {
        Core::new(
            defs(),
            Config {
                devices: vec![d],
                ..Config::default()
            },
        )
        .err()
    };
    assert!(matches!(
        bad(DeviceConfig {
            alias: "x".into(),
            ..Default::default()
        }),
        Some(Error::Config(_))
    ));
    assert!(matches!(
        bad(dev("x", "Unknown-Lamp", "1")),
        Some(Error::Config(_))
    ));
    assert!(matches!(
        bad(DeviceConfig {
            alias: "x".into(),
            name: Some("a".into()),
            id: None,
            definition: Some("ghost".into())
        }),
        Some(Error::Config(_))
    ));
}

// ---- animation and streams -----------------------------------------------------------------------------------------

#[test]
fn an_animation_ticks_at_its_frame_rate_and_keeps_the_connection_alive() {
    let mut s = Sim::with(Config {
        devices: vec![dev("lamp", "SSL-E089ED", "S1")],
        idle_ms: 1_000,
        ..Config::default()
    });
    s.all(Command::Play {
        animation: animation(json!({"type":"gen","gen":"rainbow","params":{"period":5},"fps":10})),
        name: Some("rainbow".into()),
        brightness: None,
    });
    s.advance(6_000);
    assert_eq!(
        s.count(|e| matches!(e, Ev::Disconnect(_))),
        0,
        "idle timer must not fire while animating"
    );
    let n = s.writes("S1").len();
    assert!(
        (50..=62).contains(&n),
        "about 10 frames/s for ~5.8 s, got {n}"
    );
    assert_eq!(s.core.snapshot(s.now).lamps[0].source, "animation");
    s.all(Command::StopAnimation);
    s.advance(3_000);
    assert_eq!(
        s.count(|e| matches!(e, Ev::Disconnect(_))),
        1,
        "idle drop resumes after the animation stops"
    );
}

#[test]
fn a_finished_animation_with_then_off_switches_the_lamp_off() {
    let mut s = Sim::new(vec![dev("lamp", "SSL-E089ED", "S1")]);
    s.all(Command::Play {
        animation: animation(json!({"type":"keyframes","duration":2,"fps":5,"then":"off",
            "keys":[{"t":0,"color":"#ff0000"},{"t":2,"color":"#ff0000","level":10}]})),
        name: Some("fade".into()),
        brightness: None,
    });
    s.advance(5_000);
    assert_eq!(s.writes("S1").last().unwrap(), &wire(ISTRIP, "off", &[]));
    let snap = s.core.snapshot(s.now);
    assert!(!snap.lamps[0].on && snap.lamps[0].animation.is_none());
}

#[test]
fn power_off_pauses_an_animation_and_power_on_restarts_it() {
    let mut s = Sim::new(vec![dev("lamp", "SSL-E089ED", "S1")]);
    s.all(Command::Play {
        animation: animation(json!({"type":"gen","gen":"rainbow","fps":10})),
        name: None,
        brightness: None,
    });
    s.advance(2_000);
    s.all(Command::Power(Some(false)));
    s.advance(500);
    let m = s.mark();
    s.advance(3_000);
    assert!(s.writes_since(m, "S1").is_empty(), "no frames while off");
    s.all(Command::Power(Some(true)));
    s.advance(1_000);
    assert!(s.writes_since(m, "S1").len() > 5, "frames resume");
}

#[test]
fn a_live_stream_overrides_the_look_and_falls_back_when_it_stops() {
    let mut s = Sim::new(vec![dev("lamp", "SSL-E089ED", "S1")]);
    s.all(color([0.0, 0.0, 255.0], Some(100.0)));
    s.advance(1_000);
    for i in 0..20 {
        // a changing signal: identical frames are (correctly) not re-sent
        s.all(Command::Stream {
            rgb: [100.0 + f64::from(i) * 7.0, 0.0, 0.0],
            brightness: 100.0,
        });
        s.advance(50);
    }
    assert_eq!(s.core.snapshot(s.now).lamps[0].source, "stream");
    let during = s.writes("S1");
    assert!(during.len() > 3, "stream frames were sent");
    s.advance(2_500);
    let snap = s.core.snapshot(s.now);
    assert_eq!(snap.lamps[0].source, "color");
    assert_eq!(
        s.writes("S1").last().unwrap(),
        &wire(ISTRIP, "color", &[("r", 0.0), ("g", 0.0), ("b", 255.0)]),
        "back to blue"
    );
}

#[test]
fn streams_are_ignored_while_the_lamp_is_off() {
    let mut s = Sim::new(vec![dev("lamp", "SSL-E089ED", "S1")]);
    s.all(Command::Power(Some(false)));
    s.advance(500);
    let m = s.mark();
    s.all(Command::Stream {
        rgb: [255.0, 0.0, 0.0],
        brightness: 100.0,
    });
    s.advance(500);
    assert!(s.writes_since(m, "S1").is_empty());
}

#[test]
fn dithering_shows_levels_between_the_lamps_brightness_steps() {
    // a very slow fade between 0% and 2.5% of master: the lamp has 100 steps, so most of the range lies between steps
    let mut s = Sim::new(vec![dev("lamp", "SSL-E089ED", "S1")]);
    s.all(Command::Play {
        animation: animation(
            json!({"type":"keyframes","duration":20,"fps":20,"then":"hold",
            "keys":[{"t":0,"color":"#ff8000","level":0},{"t":20,"color":"#ff8000","level":2.5}]}),
        ),
        name: None,
        brightness: Some(100.0),
    });
    s.advance(21_000);
    let seen: BTreeSet<Vec<u8>> = s.writes("S1").into_iter().collect();
    let frame = |b: f64| {
        wire(
            ISTRIP,
            "color",
            &[("r", 255.0), ("g", 128.0), ("b", 0.0), ("brightness", b)],
        )
    };
    let levels: Vec<f64> = [0.0, 1.0, 2.0, 3.0]
        .into_iter()
        .filter(|b| seen.contains(&frame(*b)))
        .collect();
    assert_eq!(
        levels,
        vec![0.0, 1.0, 2.0, 3.0]
            .into_iter()
            .take(levels.len())
            .collect::<Vec<_>>()
    );
    assert!(levels.len() >= 3, "levels seen: {levels:?}");
    // the average brightness sent should track the ideal ramp (mean of 0..2.5 = 1.25), not a rounded staircase
}

#[test]
fn dithering_can_be_turned_off() {
    let mut s = Sim::with(Config {
        devices: vec![dev("lamp", "SSL-E089ED", "S1")],
        dither: false,
        ..Config::default()
    });
    s.all(Command::Play {
        animation: animation(json!({"type":"keyframes","duration":10,"fps":20,"keys":[
            {"t":0,"color":"#ff8000","level":0},{"t":10,"color":"#ff8000","level":0.4}]})),
        name: None,
        brightness: Some(100.0),
    });
    s.advance(11_000);
    let frame = |b: f64| {
        wire(
            ISTRIP,
            "color",
            &[("r", 255.0), ("g", 128.0), ("b", 0.0), ("brightness", b)],
        )
    };
    let odd: Vec<_> = s
        .writes("S1")
        .into_iter()
        .filter(|w| *w != frame(0.0))
        .collect();
    assert!(
        odd.is_empty(),
        "0.4 of a step always rounds to zero without dithering; saw {} other frames",
        odd.len()
    );
}

#[test]
fn snapshot_reports_what_a_ui_needs() {
    let mut s = Sim::new(vec![dev("lamp", "SSL-E089ED", "S1")]);
    s.all(color([1.0, 2.0, 3.0], Some(40.0)));
    s.advance(500);
    let snap = s.core.snapshot(s.now);
    let l = &snap.lamps[0];
    assert_eq!(
        (l.on, l.rgb, l.brightness, l.connected, l.link, l.source),
        (true, [1, 2, 3], 40, true, "ready", "color")
    );
    assert!(l.caps.color && l.caps.brightness && l.caps.effects.len() == 12);
    let js = serde_json::to_value(&snap).unwrap();
    assert_eq!(js["lamps"][0]["definition"], "istrip-ssl");
    assert!(s.core.take_changed() && !s.core.take_changed());
}

#[test]
fn holding_the_last_frame_of_an_animation_keeps_its_brightness() {
    let mut s = Sim::new(vec![dev("elk", "ELK-BLEDOM", "E1")]);
    s.all(Command::Play {
        animation: animation(
            json!({"type":"keyframes","duration":2,"fps":10,"then":"hold",
            "keys":[{"t":0,"color":"#ff0000","level":100},{"t":2,"color":"#00ff00","level":20}]}),
        ),
        name: None,
        brightness: Some(100.0),
    });
    s.advance(5_000);
    let snap = s.core.snapshot(s.now);
    assert_eq!(
        (
            snap.lamps[0].rgb,
            snap.lamps[0].brightness,
            snap.lamps[0].source
        ),
        ([0, 255, 0], 20, "color")
    );
    assert_eq!(
        s.writes("E1").last().unwrap(),
        &wire(
            "elk-bledom",
            "color",
            &[("r", 0.0), ("g", 255.0), ("b", 0.0)]
        )
    );
    let held = wire("elk-bledom", "brightness", &[("level", 20.0)]);
    assert!(
        s.writes("E1").contains(&held),
        "the lamp was left at 20% brightness, not 100%"
    );
}

#[test]
fn stopping_an_animation_freezes_what_it_is_showing() {
    let mut s = Sim::new(vec![dev("elk", "ELK-BLEDOM", "E1")]);
    s.all(Command::Play {
        animation: animation(
            json!({"type":"keyframes","duration":10,"fps":10,"space":"srgb",
            "keys":[{"t":0,"color":"#000000","level":100},{"t":10,"color":"#c8c8c8","level":50}]}),
        ),
        name: Some("fade".into()),
        brightness: Some(100.0),
    });
    s.advance(5_000); // halfway: colour 100, level 75%
    s.all(Command::StopAnimation);
    s.advance(1_000);
    let l = &s.core.snapshot(s.now).lamps[0];
    assert!(l.animation.is_none() && l.source == "color");
    assert!(
        (i32::from(l.rgb[0]) - 100).abs() <= 3,
        "frozen colour near the halfway value, got {:?}",
        l.rgb
    );
    assert!(
        (i32::from(l.brightness) - 75).abs() <= 2,
        "frozen intensity near 75%, got {}",
        l.brightness
    );
    let before = s.writes("E1").len();
    s.advance(3_000);
    assert_eq!(
        s.writes("E1").len(),
        before,
        "nothing changes after the freeze"
    );
}
