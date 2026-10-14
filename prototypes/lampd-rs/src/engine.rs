//! Lamp state + source arbitration, as a single actor task (no locks).
//!
//! Priority, highest first: live stream (screen/music, expires 2 s after its last frame) >
//! animation > built-in effect > static colour. Everything ends up as frames handed to the Device.

use crate::anim::{self, Animation};
use crate::device::Device;
use crate::library::{Kind, LibError, Library};
use crate::protocol::{self, Frame};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{Instant, sleep_until};

const STREAM_TIMEOUT: Duration = Duration::from_secs(2);
const SAVE_DELAY: Duration = Duration::from_secs(1);

pub enum Error {
    Bad(String),
    NotFound(String),
}

impl From<String> for Error {
    fn from(s: String) -> Self {
        Error::Bad(s)
    }
}

impl From<&str> for Error {
    fn from(s: &str) -> Self {
        Error::Bad(s.into())
    }
}

impl From<LibError> for Error {
    fn from(e: LibError) -> Self {
        match e {
            LibError::NotFound(m) => Error::NotFound(m),
            LibError::Invalid(m) => Error::Bad(m),
        }
    }
}

type Reply = oneshot::Sender<Result<Value, Error>>;

pub enum Cmd {
    Snapshot,
    Power(Option<bool>),
    Color { color: Value, brightness: Option<f64> },
    Hs { hue: f64, sat: f64, brightness: Option<f64> },
    Brightness(f64),
    Effect { effect: Value, speed: Option<f64>, brightness: Option<f64> },
    Play { name: Option<String>, spec: Option<Value>, brightness: Option<f64> },
    StopAnim,
    ApplyPreset(String),
    SaveCurrent(String),
    Raw(Frame),
    Stream { rgb: [f64; 3], brightness: f64 },
}

struct Request {
    cmd: Cmd,
    reply: Option<Reply>,
}

/// What API handlers hold: send commands, read the library, watch state changes.
#[derive(Clone)]
pub struct Handle {
    tx: mpsc::Sender<Request>,
    pub events: watch::Receiver<Value>,
    pub library: Arc<Library>,
}

impl Handle {
    pub async fn call(&self, cmd: Cmd) -> Result<Value, Error> {
        let (reply, rx) = oneshot::channel();
        self.tx.send(Request { cmd, reply: Some(reply) }).await.map_err(|_| Error::Bad("engine stopped".into()))?;
        rx.await.map_err(|_| Error::Bad("engine stopped".into()))?
    }

    /// Fire-and-forget (live stream frames): drops the frame if the engine is busy.
    pub fn stream(&self, rgb: [f64; 3], brightness: f64) {
        let _ = self.tx.try_send(Request { cmd: Cmd::Stream { rgb, brightness }, reply: None });
    }
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
struct State {
    on: bool,
    rgb: [u8; 3],
    brightness: u8,
    effect: Option<u8>,
    speed: u8,
    animation: Option<String>,
    active: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        State { on: false, rgb: [255, 255, 255], brightness: 100, effect: None, speed: 100, animation: None, active: None }
    }
}

struct Ticker {
    t0: Instant,
    n: u64,
}

struct Engine {
    s: State,
    device: Device,
    library: Arc<Library>,
    state_path: PathBuf,
    anim: Option<Animation>,
    ticker: Option<Ticker>,
    stream_until: Option<Instant>,
    save_at: Option<Instant>,
    held: isize,
    events: watch::Sender<Value>,
}

pub fn spawn(device: Device, library: Arc<Library>, state_path: PathBuf) -> Handle {
    let (tx, mut rx) = mpsc::channel::<Request>(256);
    let (events, events_rx) = watch::channel(Value::Null);
    let mut e = Engine {
        s: State::default(), device: device.clone(), library: library.clone(), state_path, anim: None, ticker: None,
        stream_until: None, save_at: None, held: 0, events,
    };
    e.load_state();
    e.publish();
    let mut status = device.status.clone();
    tokio::spawn(async move {
        loop {
            let deadline = e.next_deadline();
            let timer = async {
                match deadline {
                    Some(t) => sleep_until(t).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                req = rx.recv() => {
                    let Some(req) = req else { return };
                    let result = e.handle(req.cmd);
                    e.sync_hold();
                    if let Some(reply) = req.reply { let _ = reply.send(result); }
                }
                _ = timer => { e.on_timer(); e.sync_hold(); }
                r = status.changed() => { if r.is_err() { return; } e.publish(); }
            }
        }
    });
    Handle { tx, events: events_rx, library }
}

fn clamp_pct(v: f64) -> u8 {
    v.round().clamp(1.0, 100.0) as u8
}

impl Engine {
    // -- state ------------------------------------------------------------------------------
    fn load_state(&mut self) {
        let Ok(text) = std::fs::read_to_string(&self.state_path) else { return };
        match serde_json::from_str::<State>(&text) {
            Ok(saved) => self.s = saved,
            Err(e) => return eprintln!("lampd: could not restore state: {e}"),
        }
        if let Some(name) = self.s.animation.clone() {
            let loaded = self.library.get(Kind::Animations, &name).ok().and_then(|spec| anim::load(&spec).ok());
            match loaded {
                Some(a) => self.anim = Some(a),
                None => self.s.animation = None,
            }
        }
    }

    fn mark_dirty(&mut self) {
        self.save_at.get_or_insert(Instant::now() + SAVE_DELAY);
    }

    fn save(&mut self) {
        self.save_at = None;
        let tmp = self.state_path.with_extension("tmp");
        if let Some(dir) = self.state_path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if std::fs::write(&tmp, serde_json::to_string(&self.s).unwrap()).is_ok() {
            let _ = std::fs::rename(&tmp, &self.state_path);
        }
    }

    fn streaming(&self) -> bool {
        self.stream_until.is_some_and(|t| Instant::now() < t)
    }

    fn snapshot(&self) -> Value {
        let s = &self.s;
        let source = if !s.on { "off" } else if self.streaming() { "stream" } else if s.animation.is_some() { "animation" }
            else if s.effect.is_some() { "effect" } else { "color" };
        let st = self.device.status.borrow().clone();
        json!({"on": s.on, "rgb": s.rgb, "brightness": s.brightness, "effect": s.effect, "speed": s.speed,
               "animation": s.animation, "active": s.active, "source": source,
               "connected": st.connected, "last_error": st.last_error})
    }

    fn publish(&mut self) {
        self.mark_dirty();
        self.events.send_replace(self.snapshot());
    }

    /// Hold the BLE connection open while an animation ticks or a stream is live.
    fn sync_hold(&mut self) {
        let want = self.ticker.is_some() as isize + self.streaming() as isize;
        if want != self.held {
            self.device.hold(want - self.held);
            self.held = want;
        }
    }

    // -- timers -----------------------------------------------------------------------------
    fn next_deadline(&self) -> Option<Instant> {
        let tick = self.ticker.as_ref().zip(self.anim.as_ref())
            .map(|(t, a)| t.t0 + Duration::from_secs_f64(t.n as f64 / a.fps));
        [tick, self.stream_until, self.save_at].into_iter().flatten().min()
    }

    fn on_timer(&mut self) {
        let now = Instant::now();
        if self.save_at.is_some_and(|t| now >= t) {
            self.save();
        }
        if self.stream_until.is_some_and(|t| now >= t) {
            self.stream_until = None;
            self.refresh(); // fall back to whatever was underneath
        }
        let due = self.ticker.as_ref().zip(self.anim.as_ref())
            .is_some_and(|(t, a)| now >= t.t0 + Duration::from_secs_f64(t.n as f64 / a.fps));
        if due {
            self.tick(now);
        }
    }

    fn tick(&mut self, now: Instant) {
        let (Some(anim), Some(tk)) = (self.anim.as_ref(), self.ticker.as_mut()) else { return };
        let t = now.duration_since(tk.t0).as_secs_f64();
        let finished = anim.duration.is_some_and(|d| !anim.looping && t >= d);
        let at = if finished { anim.duration.unwrap_or(t) } else { t };
        let (rgb, level) = anim.sample(at);
        let then_off = anim.then_off;
        tk.n += 1;
        if self.s.on && !self.streaming() {
            let bri = (self.s.brightness as f64 * level).round().max(1.0);
            if finished && then_off {
                self.s.on = false;
                self.device.submit(protocol::off(), false);
            } else {
                self.device.submit(protocol::color(rgb[0], rgb[1], rgb[2], bri, 100., 0, 0), false);
            }
        }
        if finished {
            self.s.rgb = [rgb[0].round() as u8, rgb[1].round() as u8, rgb[2].round() as u8];
            self.s.animation = None;
            self.s.active = None;
            self.anim = None;
            self.ticker = None;
            self.publish();
        }
    }

    // -- rendering --------------------------------------------------------------------------
    fn stop_anim(&mut self) {
        self.anim = None;
        self.ticker = None;
        self.s.animation = None;
    }

    fn clear_modes(&mut self) {
        self.stop_anim();
        self.s.effect = None;
        self.s.active = None;
    }

    /// Recompute and send what the lamp should show now, then tell listeners.
    fn refresh(&mut self) {
        let s = self.s.clone();
        if self.streaming() && s.on {
            // stream frames drive the lamp
        } else if !s.on {
            self.ticker = None; // pause the animation (restarts on power-on) and free the connection
            self.device.submit(protocol::off(), false);
        } else if self.anim.is_some() {
            if self.ticker.is_none() {
                self.ticker = Some(Ticker { t0: Instant::now(), n: 0 });
            }
        } else if let Some(mode) = s.effect {
            self.device.submit(protocol::color(0., 0., 0., s.brightness as f64, s.speed as f64, mode, 0), false);
        } else {
            self.device.submit(protocol::color(s.rgb[0] as f64, s.rgb[1] as f64, s.rgb[2] as f64, s.brightness as f64, 100., 0, 0), false);
        }
        self.publish();
    }

    // -- commands ---------------------------------------------------------------------------
    fn handle(&mut self, cmd: Cmd) -> Result<Value, Error> {
        match cmd {
            Cmd::Snapshot => {}
            Cmd::Power(p) => {
                self.s.on = p.unwrap_or(!self.s.on);
                self.refresh();
            }
            Cmd::Color { color, brightness } => self.set_color(&color, brightness, true)?,
            Cmd::Hs { hue, sat, brightness } => {
                let (r, g, b) = anim::hsv_to_rgb(hue.rem_euclid(360.0) / 360.0, sat.clamp(0.0, 100.0) / 100.0, 1.0);
                self.s.rgb = [(r * 255.0).round() as u8, (g * 255.0).round() as u8, (b * 255.0).round() as u8];
                self.clear_modes();
                if let Some(b) = brightness {
                    self.s.brightness = clamp_pct(b);
                }
                self.s.on = true;
                self.refresh();
            }
            Cmd::Brightness(v) => {
                self.s.brightness = clamp_pct(v);
                self.refresh();
            }
            Cmd::Effect { effect, speed, brightness } => self.set_effect(&effect, speed, brightness)?,
            Cmd::Play { name, spec, brightness } => self.play(name, spec, brightness)?,
            Cmd::StopAnim => {
                self.stop_anim();
                self.s.active = None;
                self.refresh();
            }
            Cmd::ApplyPreset(name) => self.apply_preset(&name)?,
            Cmd::SaveCurrent(name) => {
                let s = &self.s;
                let mut spec = match (&s.animation, s.effect) {
                    (Some(a), _) if a != "(inline)" => json!({"animation": a}),
                    (_, Some(m)) => json!({"effect": {"mode": m, "speed": s.speed}}),
                    _ => json!({"color": s.rgb}),
                };
                spec["brightness"] = json!(s.brightness);
                return Ok(self.library.save(Kind::Presets, &name, spec)?);
            }
            Cmd::Raw(frame) => {
                self.device.submit(frame, true);
                return Ok(json!({"sent": protocol::hex(&frame)}));
            }
            Cmd::Stream { rgb, brightness } => {
                self.push_stream(rgb, brightness);
                return Ok(Value::Null);
            }
        }
        Ok(self.snapshot())
    }

    fn set_color(&mut self, color: &Value, brightness: Option<f64>, power: bool) -> Result<(), Error> {
        let c = anim::parse_color(color)?;
        self.s.rgb = [c[0].round() as u8, c[1].round() as u8, c[2].round() as u8];
        if let Some(b) = brightness {
            self.s.brightness = clamp_pct(b);
        }
        self.clear_modes();
        if power {
            self.s.on = true;
        }
        self.refresh();
        Ok(())
    }

    fn set_effect(&mut self, effect: &Value, speed: Option<f64>, brightness: Option<f64>) -> Result<(), Error> {
        let mode = match effect {
            Value::Number(n) => n.as_u64().and_then(|m| u8::try_from(m).ok()),
            Value::String(s) => protocol::effect_by_name(s).or_else(|| s.parse().ok()),
            _ => None,
        };
        let mode = mode.filter(|m| (1..=12).contains(m)).ok_or_else(|| format!("unknown effect {effect}; use 1-12 or a name"))?;
        self.stop_anim();
        self.s.effect = Some(mode);
        self.s.active = Some(format!("effect:{mode}"));
        self.s.on = true;
        if let Some(sp) = speed {
            self.s.speed = clamp_pct(sp);
        }
        if let Some(b) = brightness {
            self.s.brightness = clamp_pct(b);
        }
        self.refresh();
        Ok(())
    }

    fn play(&mut self, name: Option<String>, spec: Option<Value>, brightness: Option<f64>) -> Result<(), Error> {
        let spec = match (&spec, &name) {
            (Some(s), _) => s.clone(),
            (None, Some(n)) => self.library.get(Kind::Animations, n)?,
            (None, None) => return Err("need an animation name or spec".into()),
        };
        let animation = anim::load(&spec)?; // validate before touching the current state
        self.stop_anim();
        self.anim = Some(animation);
        self.s.animation = Some(name.clone().unwrap_or_else(|| "(inline)".into()));
        self.s.effect = None;
        self.s.on = true;
        self.s.active = name.map(|n| format!("anim:{n}"));
        if let Some(b) = brightness {
            self.s.brightness = clamp_pct(b);
        }
        self.refresh();
        Ok(())
    }

    fn apply_preset(&mut self, name: &str) -> Result<(), Error> {
        let spec = self.library.get(Kind::Presets, name)?;
        let brightness = spec.get("brightness").and_then(Value::as_f64);
        if let Some(a) = spec.get("animation").and_then(Value::as_str) {
            self.play(Some(a.into()), None, brightness)?;
        } else if let Some(e) = spec.get("effect") {
            let mode = e.get("mode").cloned().unwrap_or(json!(1));
            self.set_effect(&mode, e.get("speed").and_then(Value::as_f64), brightness)?;
        } else if let Some(c) = spec.get("color") {
            self.set_color(c, brightness, spec.get("power").and_then(Value::as_bool).unwrap_or(true))?;
        } else {
            if let Some(b) = brightness {
                self.s.brightness = clamp_pct(b);
            }
            if let Some(p) = spec.get("power").and_then(Value::as_bool) {
                self.s.on = p;
            }
            self.refresh();
        }
        self.s.active = Some(format!("preset:{name}"));
        self.publish();
        Ok(())
    }

    /// One frame from an external live source (screen/music). Overrides everything else.
    fn push_stream(&mut self, rgb: [f64; 3], brightness: f64) {
        if !self.s.on {
            return;
        }
        let was = self.streaming();
        self.stream_until = Some(Instant::now() + STREAM_TIMEOUT);
        let level = (brightness * self.s.brightness as f64 / 100.0).round().clamp(1.0, 100.0);
        self.device.submit(protocol::color(rgb[0], rgb[1], rgb[2], level, 100., 0, 0), false);
        if !was {
            self.publish();
        }
    }
}
