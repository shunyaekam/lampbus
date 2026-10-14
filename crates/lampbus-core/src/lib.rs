//! Sans-I/O controller core for Bluetooth lamps.
//!
//! The core never touches Bluetooth, clocks or threads. A *shell* (desktop, phone, browser, microcontroller, an
//! agent's process) feeds it three kinds of input and carries out what it asks for:
//!
//! ```text
//!   commands      ──►            ┌──────┐            ──►  Action::Scan / Connect / Write / Disconnect
//!   link events   ──►  Core      │      │
//!   time (Ms)     ──►            └──────┘            ──►  poll_timeout(): when to call back next
//! ```
//!
//! Lamps are described by [`lampbus_defs::Definition`]s, so the same core drives every family. The core decides
//! what each lamp should show (static colour, built-in effect, animation, or a live stream, in that order of
//! priority from lowest to highest), passes it through [`lampbus_render`]'s pipeline, lowers it to the exact
//! frames the lamp understands, and manages the link: connect on demand, keep it while something is animating,
//! drop it when idle, back off after failures.
//!
//! Time is a plain `u64` of milliseconds from any monotonic epoch the shell chooses, which makes the whole core
//! deterministic and testable with a virtual clock.

mod lamp;
mod lower;

use lamp::{Lamp, Link};
use lampbus_defs::{Definition, WriteMode};
use lampbus_render::anim::Animation;
use lampbus_render::color::hsv_to_rgb;
use serde::Serialize;
use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;

/// Milliseconds on the shell's monotonic clock.
pub type Ms = u64;

/// How the master brightness maps to the lamp's brightness steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LevelCurve {
    /// Pass the level straight through.
    #[default]
    Linear,
    /// Treat the level as perceived lightness, so equal steps look equal (CIE L*).
    Perceptual,
}

/// A lamp the user has named.
#[derive(Debug, Clone, Default)]
pub struct DeviceConfig {
    /// Name to address it by.
    pub alias: String,
    /// Advertised Bluetooth name to look for.
    pub name: Option<String>,
    /// Platform device id, if already known (skips scanning).
    pub id: Option<String>,
    /// Definition id; inferred from `name` when omitted.
    pub definition: Option<String>,
}

/// Tunables. Defaults are conservative and fine for most lamps.
#[derive(Debug, Clone)]
pub struct Config {
    /// Lamps to control.
    pub devices: Vec<DeviceConfig>,
    /// Also adopt any advertising lamp that matches a definition.
    pub auto_adopt: bool,
    /// Drop the connection after this long with nothing to do (0 keeps it forever).
    pub idle_ms: Ms,
    /// Minimum gap between writes to one lamp.
    pub min_write_interval_ms: Ms,
    /// A live stream is considered over this long after its last frame.
    pub stream_timeout_ms: Ms,
    /// Give up on a connect after this long.
    pub connect_timeout_ms: Ms,
    /// Stop scanning after this long.
    pub scan_timeout_ms: Ms,
    /// Colour response curve exponent (1.0 = none).
    pub color_gamma: f64,
    /// Brightness curve.
    pub level_curve: LevelCurve,
    /// Temporal dithering for animations and streams.
    pub dither: bool,
    /// Smoothing time constant when a streamed value rises.
    pub stream_attack_ms: f64,
    /// Smoothing time constant when a streamed value falls.
    pub stream_release_ms: f64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            devices: Vec::new(),
            auto_adopt: false,
            idle_ms: 120_000,
            min_write_interval_ms: 25,
            stream_timeout_ms: 2_000,
            connect_timeout_ms: 30_000,
            scan_timeout_ms: 15_000,
            color_gamma: 1.0,
            level_curve: LevelCurve::Linear,
            dither: true,
            stream_attack_ms: 30.0,
            stream_release_ms: 150.0,
        }
    }
}

/// Which lamps a command applies to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// Every lamp.
    All,
    /// One lamp, by alias or device id.
    Named(String),
}

/// What the user (or an adapter) wants.
#[derive(Debug, Clone)]
pub enum Command {
    /// Switch on or off; `None` toggles.
    Power(Option<bool>),
    /// A static colour (`0..=255` per channel), optionally with a master brightness percent. Switches the lamp on.
    Color {
        /// Red, green, blue.
        rgb: [f64; 3],
        /// Master brightness percent.
        brightness: Option<f64>,
    },
    /// A static colour from hue `0..=360` and saturation `0..=100`. Switches the lamp on.
    Hs {
        /// Hue in degrees.
        hue: f64,
        /// Saturation percent.
        sat: f64,
        /// Master brightness percent.
        brightness: Option<f64>,
    },
    /// Master brightness percent.
    Brightness(f64),
    /// A built-in effect by name, alias or numeric id.
    Effect {
        /// Name, alias or id.
        effect: String,
        /// Speed percent.
        speed: Option<f64>,
        /// Master brightness percent.
        brightness: Option<f64>,
    },
    /// Play a validated animation.
    Play {
        /// The animation.
        animation: Arc<Animation>,
        /// Library name, for display.
        name: Option<String>,
        /// Master brightness percent.
        brightness: Option<f64>,
    },
    /// Stop any animation and keep exactly what it is showing right now.
    StopAnimation,
    /// One frame of a live source (screen, music). Overrides everything until it stops arriving.
    Stream {
        /// Red, green, blue.
        rgb: [f64; 3],
        /// Intensity percent, relative to master brightness.
        brightness: f64,
    },
    /// Send a named protocol command from the lamp's definition, bypassing everything above.
    Raw {
        /// Command name.
        command: String,
        /// Arguments.
        args: Vec<(String, f64)>,
    },
    /// Set the display label of what is active (for example `preset:night`).
    Label(Option<String>),
}

/// Something that happened on the link, reported by the shell.
#[derive(Debug, Clone)]
pub enum LinkEvent {
    /// A device was seen while scanning.
    Advertised {
        /// Platform device id.
        id: String,
        /// Advertised local name.
        name: Option<String>,
        /// Advertised service UUIDs, lowercase.
        services: Vec<String>,
    },
    /// A requested connection is up and the write characteristic is usable.
    Connected {
        /// Device id.
        id: String,
    },
    /// A requested connection failed.
    ConnectFailed {
        /// Device id.
        id: String,
        /// Why.
        reason: String,
    },
    /// The device disconnected (on its own or after a requested disconnect).
    Disconnected {
        /// Device id.
        id: String,
    },
    /// A write failed.
    WriteFailed {
        /// Device id.
        id: String,
        /// Why.
        reason: String,
    },
}

/// What the shell must do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Start scanning and report each advertisement as [`LinkEvent::Advertised`].
    Scan,
    /// Stop scanning.
    StopScan,
    /// Connect, discover services, then report [`LinkEvent::Connected`] or [`LinkEvent::ConnectFailed`].
    Connect {
        /// Device id.
        device: String,
    },
    /// Disconnect (idempotent).
    Disconnect {
        /// Device id.
        device: String,
    },
    /// Write one frame, then carry on; report [`LinkEvent::WriteFailed`] only on failure.
    Write {
        /// Device id.
        device: String,
        /// Service UUID.
        service: String,
        /// Characteristic UUID.
        characteristic: String,
        /// Write mode.
        mode: WriteMode,
        /// Bytes to write.
        data: Vec<u8>,
    },
}

/// Why a command or construction failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// No lamp matches the target.
    NoMatchingDevice(String),
    /// The lamp cannot do this.
    Unsupported(String),
    /// The request is malformed.
    Bad(String),
    /// A configuration problem.
    Config(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NoMatchingDevice(t) => write!(f, "no lamp matches {t:?}"),
            Error::Unsupported(m) => write!(f, "unsupported: {m}"),
            Error::Bad(m) => write!(f, "bad request: {m}"),
            Error::Config(m) => write!(f, "configuration: {m}"),
        }
    }
}

impl std::error::Error for Error {}

/// One effect a lamp offers.
#[derive(Debug, Clone, Serialize)]
pub struct EffectView {
    /// Id to pass.
    pub id: u8,
    /// Name.
    pub name: String,
}

/// What a lamp can do, for UIs and adapters.
#[derive(Debug, Clone, Serialize)]
pub struct CapsView {
    /// Can show colours.
    pub color: bool,
    /// Has brightness control (native or emulated by scaling the colour).
    pub brightness: bool,
    /// Built-in effects.
    pub effects: Vec<EffectView>,
}

/// One lamp's state.
#[derive(Debug, Clone, Serialize)]
pub struct LampState {
    /// Platform device id, once known.
    pub id: Option<String>,
    /// Alias.
    pub alias: String,
    /// Definition id.
    pub definition: String,
    /// `down`, `connecting`, `ready` or `backoff`.
    pub link: &'static str,
    /// Whether commands can be written right now.
    pub connected: bool,
    /// Powered on.
    pub on: bool,
    /// Static colour.
    pub rgb: [u8; 3],
    /// Master brightness percent.
    pub brightness: u8,
    /// Active built-in effect id.
    pub effect: Option<u8>,
    /// Running animation.
    pub animation: Option<String>,
    /// Label of what is active.
    pub active: Option<String>,
    /// `off`, `color`, `effect`, `animation` or `stream`.
    pub source: &'static str,
    /// Last problem, if any.
    pub last_error: Option<String>,
    /// Frames waiting to be written. Zero with `link == "ready"` means the lamp has everything.
    pub pending: usize,
    /// What the lamp can do.
    pub caps: CapsView,
}

/// All lamps.
#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    /// Lamps.
    pub lamps: Vec<LampState>,
    /// Whether a scan is running.
    pub scanning: bool,
}

/// The controller.
pub struct Core {
    cfg: Config,
    defs: Vec<Arc<Definition>>,
    lamps: Vec<Lamp>,
    actions: VecDeque<Action>,
    scanning: bool,
    scan_started: Ms,
    discover_until: Option<Ms>,
    changed: bool,
}

fn matches_ad(def: &Definition, name: Option<&str>, services: &[String]) -> bool {
    let by_name = name.is_some_and(|n| {
        def.matching
            .name_prefix
            .iter()
            .any(|p| n.starts_with(p.as_str()))
    });
    let by_service = services
        .iter()
        .any(|s| def.matching.service_uuids.iter().any(|d| d == s));
    by_name || by_service
}

impl Core {
    /// Build a core from definitions and configuration.
    ///
    /// # Errors
    /// [`Error::Config`] if a configured lamp names an unknown definition or cannot be matched to one.
    pub fn new(defs: Vec<Definition>, cfg: Config) -> Result<Core, Error> {
        let defs: Vec<Arc<Definition>> = defs.into_iter().map(Arc::new).collect();
        let mut lamps = Vec::new();
        for d in &cfg.devices {
            if d.name.is_none() && d.id.is_none() {
                return Err(Error::Config(format!(
                    "device {:?} needs a name or an id",
                    d.alias
                )));
            }
            let def = match &d.definition {
                Some(id) => defs
                    .iter()
                    .find(|x| &x.id == id)
                    .ok_or_else(|| Error::Config(format!("unknown definition {id:?}")))?,
                None => defs
                    .iter()
                    .find(|x| matches_ad(x, d.name.as_deref(), &[]))
                    .ok_or_else(|| {
                        Error::Config(format!(
                            "no definition matches device {:?}; set \"definition\"",
                            d.alias
                        ))
                    })?,
            };
            let mut lamp = Lamp::new(d.id.clone(), d.alias.clone(), Arc::clone(def), &cfg);
            lamp.bind_name = d.name.clone();
            lamps.push(lamp);
        }
        Ok(Core {
            cfg,
            defs,
            lamps,
            actions: VecDeque::new(),
            scanning: false,
            scan_started: 0,
            discover_until: None,
            changed: true,
        })
    }

    /// Look for lamps for the next `scan_timeout_ms`, adopting matches if `auto_adopt` is on.
    pub fn discover(&mut self, now: Ms) {
        self.discover_until = Some(now + self.cfg.scan_timeout_ms);
        self.pump(now);
    }

    fn select(&self, target: &Target) -> Vec<usize> {
        self.lamps
            .iter()
            .enumerate()
            .filter(|(_, l)| match target {
                Target::All => true,
                Target::Named(n) => {
                    l.alias.eq_ignore_ascii_case(n) || l.id.as_deref() == Some(n.as_str())
                }
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Apply a command to the targeted lamps. Returns how many it applied to.
    ///
    /// # Errors
    /// [`Error::NoMatchingDevice`] if nothing matches; otherwise the first error if it failed everywhere.
    pub fn handle_command(
        &mut self,
        now: Ms,
        target: &Target,
        cmd: Command,
    ) -> Result<usize, Error> {
        let idx = self.select(target);
        if idx.is_empty() {
            let t = match target {
                Target::All => "all (no lamps known yet)".to_string(),
                Target::Named(n) => n.clone(),
            };
            return Err(Error::NoMatchingDevice(t));
        }
        let (mut ok, mut first_err) = (0, None);
        for i in idx {
            match self.apply(i, now, &cmd) {
                Ok(()) => ok += 1,
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
        self.changed = true;
        self.pump(now);
        match (ok, first_err) {
            (0, Some(e)) => Err(e),
            _ => Ok(ok),
        }
    }

    fn apply(&mut self, i: usize, now: Ms, cmd: &Command) -> Result<(), Error> {
        let cfg = self.cfg.clone();
        let Some(lamp) = self.lamps.get_mut(i) else {
            return Ok(());
        };
        if !matches!(cmd, Command::Raw { .. } | Command::Label(_)) {
            lamp.touched = true;
        }
        let pct = |v: f64| v.clamp(0.0, 100.0);
        let byte = |v: f64| v.round().clamp(0.0, 255.0) as u8;
        match cmd {
            Command::Power(p) => lamp.power(p.unwrap_or(!lamp.look.on), now),
            Command::Color { rgb, brightness } => {
                if lamp.caps.color.is_none() {
                    return Err(Error::Unsupported(format!(
                        "{} cannot show colours",
                        lamp.alias
                    )));
                }
                lamp.stop_animation();
                lamp.look.effect = None;
                lamp.look.active = None;
                lamp.look.rgb = [byte(rgb[0]), byte(rgb[1]), byte(rgb[2])];
                if let Some(b) = brightness {
                    lamp.look.brightness = pct(*b);
                }
                lamp.power(true, now);
            }
            Command::Hs {
                hue,
                sat,
                brightness,
            } => {
                let [r, g, b] = hsv_to_rgb(hue.rem_euclid(360.0) / 360.0, pct(*sat) / 100.0, 1.0);
                return self.apply(
                    i,
                    now,
                    &Command::Color {
                        rgb: [r * 255.0, g * 255.0, b * 255.0],
                        brightness: *brightness,
                    },
                );
            }
            Command::Brightness(v) => {
                lamp.look.brightness = pct(*v);
                lamp.dirty = true;
            }
            Command::Effect {
                effect,
                speed,
                brightness,
            } => {
                let ec = lamp.caps.effect.as_ref().ok_or_else(|| {
                    Error::Unsupported(format!("{} has no built-in effects", lamp.alias))
                })?;
                let id = effect
                    .parse::<u8>()
                    .ok()
                    .filter(|id| {
                        lamp.def.effects.is_empty() || lamp.def.effects.iter().any(|e| e.id == *id)
                    })
                    .or_else(|| {
                        lamp.def
                            .effects
                            .iter()
                            .find(|e| e.matches(effect))
                            .map(|e| e.id)
                    })
                    .filter(|id| (ec.mode.min..=ec.mode.max).contains(&f64::from(*id)))
                    .ok_or_else(|| {
                        Error::Bad(format!("{} has no effect {effect:?}", lamp.alias))
                    })?;
                lamp.stop_animation();
                let speed = speed
                    .map(pct)
                    .or_else(|| lamp.look.effect.map(|(_, s)| s))
                    .unwrap_or(100.0);
                lamp.look.effect = Some((id, speed));
                lamp.look.active = Some(format!("effect:{id}"));
                if let Some(b) = brightness {
                    lamp.look.brightness = pct(*b);
                }
                lamp.power(true, now);
            }
            Command::Play {
                animation,
                name,
                brightness,
            } => {
                if lamp.caps.color.is_none() {
                    return Err(Error::Unsupported(format!(
                        "{} cannot show colours",
                        lamp.alias
                    )));
                }
                lamp.start_animation(Arc::clone(animation), name.clone(), now);
                lamp.look.active = name.as_ref().map(|n| format!("anim:{n}"));
                if let Some(b) = brightness {
                    lamp.look.brightness = pct(*b);
                }
            }
            Command::StopAnimation => {
                lamp.freeze_animation(now);
                lamp.look.active = None;
            }
            Command::Stream { rgb, brightness } => {
                if lamp.caps.color.is_none() {
                    return Err(Error::Unsupported(format!(
                        "{} cannot show colours",
                        lamp.alias
                    )));
                }
                lamp.set_stream(*rgb, pct(*brightness) / 100.0, now, &cfg);
            }
            Command::Raw { command, args } => {
                let a: Vec<(&str, f64)> = args.iter().map(|(k, v)| (k.as_str(), *v)).collect();
                lamp.queue_raw(command, &a).map_err(Error::Bad)?;
            }
            Command::Label(l) => lamp.look.active.clone_from(l),
        }
        Ok(())
    }

    /// Report something that happened on the link.
    pub fn handle_link(&mut self, now: Ms, event: LinkEvent) {
        self.changed = true;
        match event {
            LinkEvent::Advertised { id, name, services } => self.advertised(id, name, &services),
            LinkEvent::Connected { id } => {
                if let Some(l) = self.lamp_by_id(&id) {
                    if matches!(l.link, Link::Connecting { .. }) {
                        l.link_up(now);
                    }
                }
            }
            LinkEvent::ConnectFailed { id, reason } => {
                if let Some(l) = self.lamp_by_id(&id) {
                    l.link_down(now, Some(reason), true);
                }
            }
            LinkEvent::Disconnected { id } => {
                if let Some(l) = self.lamp_by_id(&id) {
                    if matches!(l.link, Link::Ready | Link::Connecting { .. }) {
                        l.link_down(now, Some("disconnected".into()), true);
                    }
                }
            }
            LinkEvent::WriteFailed { id, reason } => {
                if let Some(l) = self.lamp_by_id(&id) {
                    l.link_down(now, Some(reason), true);
                    self.actions.push_back(Action::Disconnect { device: id });
                }
            }
        }
        self.pump(now);
    }

    fn lamp_by_id(&mut self, id: &str) -> Option<&mut Lamp> {
        self.lamps.iter_mut().find(|l| l.id.as_deref() == Some(id))
    }

    fn advertised(&mut self, id: String, name: Option<String>, services: &[String]) {
        if self
            .lamps
            .iter()
            .any(|l| l.id.as_deref() == Some(id.as_str()))
        {
            return;
        }
        // a configured lamp waiting for its id
        if let Some(l) = self
            .lamps
            .iter_mut()
            .find(|l| l.id.is_none() && l.bind_name.is_some() && l.bind_name == name)
        {
            l.id = Some(id);
            return;
        }
        if self.cfg.auto_adopt {
            if let Some(def) = self
                .defs
                .iter()
                .find(|d| matches_ad(d, name.as_deref(), services))
            {
                let alias = name.clone().unwrap_or_else(|| id.clone());
                if !self
                    .lamps
                    .iter()
                    .any(|l| l.alias.eq_ignore_ascii_case(&alias))
                {
                    self.lamps
                        .push(Lamp::new(Some(id), alias, Arc::clone(def), &self.cfg));
                }
            }
        }
    }

    /// Handle time passing. Call when `poll_timeout()` says so, and whenever the shell has the time anyway.
    pub fn handle_timeout(&mut self, now: Ms) {
        let cfg = self.cfg.clone();
        for l in &mut self.lamps {
            self.changed |= l.on_timer(now, &cfg, &mut self.actions);
        }
        if self.discover_until.is_some_and(|u| now >= u) {
            self.discover_until = None;
        }
        self.pump(now);
    }

    fn pump(&mut self, now: Ms) {
        let cfg = self.cfg.clone();
        let mut need_scan = false;
        for l in &mut self.lamps {
            need_scan |= l.pump(now, &cfg, &mut self.actions);
        }
        if self.scanning && now >= self.scan_started + cfg.scan_timeout_ms {
            self.actions.push_back(Action::StopScan);
            self.scanning = false;
            for l in &mut self.lamps {
                if l.id.is_none() && l.wants_link(now) {
                    l.link_down(now, Some("not found while scanning".into()), true);
                }
            }
            need_scan = false;
        }
        let want = need_scan || self.discover_until.is_some_and(|u| now < u);
        if want && !self.scanning {
            self.actions.push_back(Action::Scan);
            self.scanning = true;
            self.scan_started = now;
        } else if !want && self.scanning {
            self.actions.push_back(Action::StopScan);
            self.scanning = false;
        }
    }

    /// The next thing the shell must do, if any.
    pub fn poll_action(&mut self) -> Option<Action> {
        self.actions.pop_front()
    }

    /// When the shell should next call [`Core::handle_timeout`]. `None` means nothing is scheduled.
    pub fn poll_timeout(&self, now: Ms) -> Option<Ms> {
        let scan = self
            .scanning
            .then(|| self.scan_started + self.cfg.scan_timeout_ms);
        let discover = self.discover_until;
        self.lamps
            .iter()
            .filter_map(|l| l.next_deadline(&self.cfg, now))
            .chain(scan)
            .chain(discover)
            .min()
    }

    /// True once if visible state changed since the last call.
    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    /// Current state of every lamp.
    pub fn snapshot(&self, now: Ms) -> Snapshot {
        let lamps = self
            .lamps
            .iter()
            .map(|l| {
                let link = match l.link {
                    Link::Down => "down",
                    Link::Connecting { .. } => "connecting",
                    Link::Ready => "ready",
                    Link::Backoff { .. } => "backoff",
                };
                LampState {
                    id: l.id.clone(),
                    alias: l.alias.clone(),
                    definition: l.def.id.clone(),
                    link,
                    connected: l.link == Link::Ready,
                    on: l.look.on,
                    rgb: l.look.rgb,
                    brightness: l.look.brightness.round() as u8,
                    effect: l.look.effect.map(|(m, _)| m),
                    animation: l
                        .look
                        .anim
                        .as_ref()
                        .map(|a| a.name.clone().unwrap_or_else(|| "(inline)".into())),
                    active: l.look.active.clone(),
                    source: l.source(now),
                    last_error: l.last_error.clone(),
                    pending: l.pending(),
                    caps: CapsView {
                        color: l.caps.color.is_some(),
                        brightness: l.caps.brightness.is_some() || l.caps.color.is_some(),
                        effects: if l.caps.effect.is_some() {
                            l.def
                                .effects
                                .iter()
                                .map(|e| EffectView {
                                    id: e.id,
                                    name: e.name.clone(),
                                })
                                .collect()
                        } else {
                            Vec::new()
                        },
                    },
                }
            })
            .collect();
        Snapshot {
            lamps,
            scanning: self.scanning,
        }
    }
}
