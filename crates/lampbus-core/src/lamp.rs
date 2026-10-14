//! The per-lamp state machine: what the lamp should show, what the core believes it shows, and the link to it.

use crate::lower::{lower, Frame, Out, Sent};
use crate::{Action, Config, LevelCurve, Ms};
use lampbus_defs::{Caps, Definition, Range};
use lampbus_render::anim::Animation;
use lampbus_render::color::Rgb;
use lampbus_render::pipeline::{lightness_to_luminance, Calibration, Dither, Smoother};
use std::collections::VecDeque;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Link {
    /// Not connected, and not trying.
    Down,
    /// A connect was requested.
    Connecting { since: Ms },
    /// Connected and writable.
    Ready,
    /// Waiting before the next attempt.
    Backoff { until: Ms },
}

pub(crate) struct Running {
    pub anim: Arc<Animation>,
    pub name: Option<String>,
    pub t0: Ms,
    /// Index of the next tick; tick `n` is due at `t0 + n / fps`.
    pub n: u64,
}

pub(crate) struct Look {
    pub on: bool,
    pub rgb: [u8; 3],
    /// Master brightness, percent.
    pub brightness: f64,
    /// Built-in effect: `(mode, speed percent)`.
    pub effect: Option<(u8, f64)>,
    pub anim: Option<Running>,
    /// Label for UIs: what the user last asked for (`anim:candle`, `preset:night`, ...).
    pub active: Option<String>,
}

struct Stream {
    rgb: Rgb,
    level: f64,
    until: Ms,
}

pub(crate) struct Lamp {
    pub id: Option<String>,
    pub alias: String,
    pub def: Arc<Definition>,
    pub caps: Caps,
    pub look: Look,
    pub link: Link,
    pub last_error: Option<String>,
    /// Frames lowered from `look`; replaced wholesale on every re-render (newest wins).
    queue: VecDeque<Frame>,
    /// Raw protocol frames: sent before rendered ones and never replaced by a re-render.
    raw: VecDeque<Frame>,
    sent: Sent,
    next_write_at: Ms,
    last_activity: Ms,
    backoff_ms: Ms,
    stream: Option<Stream>,
    smoother: Smoother,
    last_smooth: Option<Ms>,
    dither: Dither,
    /// Whether the last error came from lowering (so a later successful lowering may clear it).
    lower_err: bool,
    /// Whether any state command has been given; an untouched lamp is left alone when it connects.
    pub touched: bool,
    /// Advertised name this lamp binds to when it has no id yet.
    pub bind_name: Option<String>,
    pub dirty: bool,
}

fn level_units(range: Range, fraction: f64) -> f64 {
    range.min + fraction.clamp(0.0, 1.0) * (range.max - range.min)
}

impl Lamp {
    pub(crate) fn new(
        id: Option<String>,
        alias: String,
        def: Arc<Definition>,
        cfg: &Config,
    ) -> Lamp {
        let caps = def.caps();
        Lamp {
            id,
            alias,
            def,
            caps,
            look: Look {
                on: false,
                rgb: [255, 255, 255],
                brightness: 100.0,
                effect: None,
                anim: None,
                active: None,
            },
            link: Link::Down,
            last_error: None,
            queue: VecDeque::new(),
            raw: VecDeque::new(),
            sent: Sent::default(),
            next_write_at: 0,
            last_activity: 0,
            backoff_ms: 0,
            stream: None,
            smoother: Smoother::new(cfg.stream_attack_ms, cfg.stream_release_ms),
            last_smooth: None,
            dither: Dither::new(),
            lower_err: false,
            touched: false,
            bind_name: None,
            dirty: false,
        }
    }

    pub(crate) fn streaming(&self, now: Ms) -> bool {
        self.stream.as_ref().is_some_and(|s| now < s.until)
    }

    fn animating(&self) -> bool {
        self.look.on && self.look.anim.is_some()
    }

    fn nothing_queued(&self) -> bool {
        self.queue.is_empty() && self.raw.is_empty()
    }

    /// Whether the lamp needs a live connection right now.
    pub(crate) fn wants_link(&self, now: Ms) -> bool {
        !self.nothing_queued() || self.animating() || (self.look.on && self.streaming(now))
    }

    pub(crate) fn source(&self, now: Ms) -> &'static str {
        if !self.look.on {
            "off"
        } else if self.streaming(now) {
            "stream"
        } else if self.look.anim.is_some() {
            "animation"
        } else if self.look.effect.is_some() {
            "effect"
        } else {
            "color"
        }
    }

    // ---- control ------------------------------------------------------------------------------------------------

    pub(crate) fn start_animation(&mut self, anim: Arc<Animation>, name: Option<String>, now: Ms) {
        self.look.effect = None;
        self.look.anim = Some(Running {
            anim,
            name,
            t0: now,
            n: 0,
        });
        self.look.on = true;
        self.dirty = true;
    }

    pub(crate) fn stop_animation(&mut self) {
        self.look.anim = None;
        self.dirty = true;
    }

    pub(crate) fn set_stream(&mut self, rgb: Rgb, level: f64, now: Ms, cfg: &Config) {
        if !self.look.on {
            return;
        }
        self.stream = Some(Stream {
            rgb,
            level: level.clamp(0.0, 1.0),
            until: now + cfg.stream_timeout_ms,
        });
        self.dirty = true;
    }

    pub(crate) fn power(&mut self, on: bool, now: Ms) {
        if on && !self.look.on {
            if let Some(r) = self.look.anim.as_mut() {
                r.t0 = now; // an animation restarts when the lamp is switched back on
                r.n = 0;
            }
        }
        self.look.on = on;
        self.dirty = true;
    }

    pub(crate) fn queue_raw(&mut self, command: &str, args: &[(&str, f64)]) -> Result<(), String> {
        let e = self.def.encode(command, args).map_err(|e| e.to_string())?;
        self.raw.push_back(Frame {
            wire: e.wire,
            delta: Default::default(),
        });
        Ok(())
    }

    // ---- rendering ----------------------------------------------------------------------------------------------

    fn calibrate(rgb: Rgb, cfg: &Config) -> Rgb {
        let cal = Calibration {
            gamma: cfg.color_gamma,
        };
        [
            cal.apply(rgb[0] / 255.0) * 255.0,
            cal.apply(rgb[1] / 255.0) * 255.0,
            cal.apply(rgb[2] / 255.0) * 255.0,
        ]
    }

    fn curve(level: f64, cfg: &Config) -> f64 {
        match cfg.level_curve {
            LevelCurve::Linear => level.clamp(0.0, 1.0),
            LevelCurve::Perceptual => lightness_to_luminance(level),
        }
    }

    /// Quantise a colour and level (`0..=1`) to what this lamp can express.
    fn quantise_color(&mut self, rgb: Rgb, level: f64, dithering: bool, cfg: &Config) -> Out {
        let rgb = Self::calibrate(rgb, cfg);
        let level = Self::curve(level, cfg);
        let color = self.caps.color.as_ref();
        let range = color
            .and_then(|c| c.brightness)
            .or_else(|| self.caps.brightness.as_ref().map(|b| b.range));
        let (quad, max) = match range {
            Some(r) => (
                [rgb[0], rgb[1], rgb[2], level_units(r, level)],
                [255, 255, 255, r.max.clamp(0.0, 255.0) as u8],
            ),
            // no brightness control: scale the colour itself
            None => (
                [rgb[0] * level, rgb[1] * level, rgb[2] * level, 0.0],
                [255, 255, 255, 0],
            ),
        };
        let q = if dithering && cfg.dither {
            self.dither.quantize(quad, max)
        } else {
            self.dither.reset();
            let r = |v: f64, m: u8| v.round().clamp(0.0, f64::from(m)) as u8;
            [
                r(quad[0], max[0]),
                r(quad[1], max[1]),
                r(quad[2], max[2]),
                r(quad[3], max[3]),
            ]
        };
        Out::Color {
            rgb: [q[0], q[1], q[2]],
            level: range.map(|_| q[3]),
        }
    }

    fn effect_out(&self, mode: u8, speed_pct: f64, cfg: &Config) -> Out {
        let fraction = speed_pct / 100.0;
        let speed_range = self
            .caps
            .effect
            .as_ref()
            .and_then(|e| e.speed)
            .or_else(|| self.caps.effect_speed.as_ref().map(|s| s.range));
        let level_range = self
            .caps
            .effect
            .as_ref()
            .and_then(|e| e.brightness)
            .or_else(|| self.caps.brightness.as_ref().map(|b| b.range));
        let unit = |r: Range, f: f64| level_units(r, f).round().clamp(0.0, 255.0) as u8;
        Out::Effect {
            mode,
            speed: speed_range.map(|r| unit(r, fraction)),
            level: level_range.map(|r| unit(r, Self::curve(self.look.brightness / 100.0, cfg))),
        }
    }

    /// Decide what the lamp should show now, lower it to frames, and replace the pending queue with them.
    pub(crate) fn rerender(&mut self, now: Ms, cfg: &Config) {
        self.dirty = false;
        if !self.touched {
            return;
        }
        let master = self.look.brightness / 100.0;
        let out = if !self.look.on {
            self.smoother.reset();
            self.last_smooth = None;
            Out::Off
        } else if let Some(s) = self.stream.as_ref().filter(|s| now < s.until) {
            let (rgb, level) = (s.rgb, s.level);
            let dt = self.last_smooth.map_or(0, |t| now.saturating_sub(t)) as f64;
            self.last_smooth = Some(now);
            let [r, g, b, l] = self
                .smoother
                .step([rgb[0], rgb[1], rgb[2], level * 255.0], dt);
            self.quantise_color([r, g, b], l / 255.0 * master, true, cfg)
        } else if let Some(run) = &self.look.anim {
            let s = run.anim.sample(now.saturating_sub(run.t0) as f64 / 1000.0);
            self.quantise_color(s.rgb, s.level * master, true, cfg)
        } else if let Some((mode, speed)) = self.look.effect {
            self.effect_out(mode, speed, cfg)
        } else {
            let rgb = [
                f64::from(self.look.rgb[0]),
                f64::from(self.look.rgb[1]),
                f64::from(self.look.rgb[2]),
            ];
            self.quantise_color(rgb, master, false, cfg)
        };
        match lower(&self.def, &self.caps, &self.sent, &out) {
            Ok(frames) => {
                self.queue = frames.into();
                if self.lower_err {
                    self.lower_err = false;
                    self.last_error = None;
                }
            }
            Err(e) => {
                self.lower_err = true;
                self.last_error = Some(e);
            }
        }
    }

    // ---- link ---------------------------------------------------------------------------------------------------

    pub(crate) fn link_up(&mut self, now: Ms) {
        self.link = Link::Ready;
        self.sent = Sent::default(); // the lamp's state is only trusted after we have written it ourselves
        self.last_activity = now;
        self.next_write_at = now;
        self.backoff_ms = 0;
        self.last_error = None;
        self.lower_err = false;
        self.dirty = true;
    }

    pub(crate) fn link_down(&mut self, now: Ms, reason: Option<String>, retry: bool) {
        self.backoff_ms = if self.backoff_ms == 0 {
            1_000
        } else {
            (self.backoff_ms * 2).min(15_000)
        };
        self.link = if retry {
            Link::Backoff {
                until: now + self.backoff_ms,
            }
        } else {
            Link::Down
        };
        self.sent = Sent::default();
        self.queue.clear();
        if reason.is_some() {
            self.last_error = reason;
            self.lower_err = false;
        }
        self.dirty = true;
    }

    /// Advance the lamp: re-render if needed and emit at most one write or connect. Returns true if the lamp needs
    /// a scan before it can connect.
    pub(crate) fn pump(&mut self, now: Ms, cfg: &Config, actions: &mut VecDeque<Action>) -> bool {
        if self.dirty {
            self.rerender(now, cfg);
        }
        let wants = self.wants_link(now);
        match self.link {
            Link::Ready => {
                if now >= self.next_write_at {
                    let next = self.raw.pop_front().or_else(|| self.queue.pop_front());
                    if let (Some(frame), Some(id)) = (next, self.id.clone()) {
                        self.sent.apply(&frame.delta);
                        self.next_write_at = now + cfg.min_write_interval_ms;
                        self.last_activity = now;
                        actions.push_back(Action::Write {
                            device: id,
                            service: self.def.gatt.service.clone(),
                            characteristic: self.def.gatt.write.clone(),
                            mode: self.def.gatt.mode,
                            data: frame.wire,
                        });
                    }
                }
                false
            }
            Link::Down | Link::Backoff { .. } if wants => {
                if let Link::Backoff { until } = self.link {
                    if now < until {
                        return false;
                    }
                }
                match self.id.clone() {
                    Some(device) => {
                        actions.push_back(Action::Connect { device });
                        self.link = Link::Connecting { since: now };
                        false
                    }
                    None => true,
                }
            }
            _ => false,
        }
    }

    // ---- time ---------------------------------------------------------------------------------------------------

    fn tick_due(&self) -> Option<Ms> {
        let run = self.look.anim.as_ref().filter(|_| self.look.on)?;
        Some(run.t0 + (run.n as f64 * 1000.0 / run.anim.fps).round() as Ms)
    }

    fn idle_deadline(&self, cfg: &Config) -> Option<Ms> {
        (self.link == Link::Ready
            && cfg.idle_ms > 0
            && self.nothing_queued()
            && !self.animating()
            && self.stream.is_none())
        .then(|| self.last_activity + cfg.idle_ms)
    }

    pub(crate) fn next_deadline(&self, cfg: &Config, now: Ms) -> Option<Ms> {
        let write = (self.link == Link::Ready && !self.nothing_queued())
            .then(|| self.next_write_at.max(now));
        let link = match self.link {
            Link::Backoff { until } if self.wants_link(now) => Some(until),
            Link::Connecting { since } => Some(since + cfg.connect_timeout_ms),
            _ => None,
        };
        [
            self.tick_due(),
            self.stream.as_ref().map(|s| s.until),
            write,
            link,
            self.idle_deadline(cfg),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Handle whatever is due at `now`. Returns true if externally visible state changed.
    pub(crate) fn on_timer(
        &mut self,
        now: Ms,
        cfg: &Config,
        actions: &mut VecDeque<Action>,
    ) -> bool {
        let mut changed = false;
        if self.stream.as_ref().is_some_and(|s| now >= s.until) {
            self.stream = None;
            self.smoother.reset();
            self.last_smooth = None;
            self.dirty = true;
            changed = true;
        }
        if let Some(due) = self.tick_due() {
            if now >= due {
                changed |= self.tick(now);
            }
        }
        if let Link::Connecting { since } = self.link {
            if now >= since + cfg.connect_timeout_ms {
                actions.push_back(Action::Disconnect {
                    device: self.id.clone().unwrap_or_default(),
                });
                self.link_down(now, Some("connect timed out".into()), true);
                changed = true;
            }
        }
        if self.idle_deadline(cfg).is_some_and(|d| now >= d) {
            if let Some(device) = self.id.clone() {
                actions.push_back(Action::Disconnect { device });
            }
            self.link = Link::Down;
            self.sent = Sent::default();
            changed = true;
        }
        changed
    }

    /// An animation tick is due. Returns true if the animation ended.
    fn tick(&mut self, now: Ms) -> bool {
        let Some(run) = self.look.anim.as_mut() else {
            return false;
        };
        let t = now.saturating_sub(run.t0) as f64 / 1000.0;
        let finished = run.anim.finished(t);
        let then_off = run.anim.then_off;
        let end = run.anim.sample(t);
        // skip any ticks we were too late for instead of bursting to catch up
        run.n = (run.n + 1).max((t * run.anim.fps).floor() as u64 + 1);
        self.dirty = true;
        if finished {
            self.look.anim = None;
            self.look.active = None;
            self.look.rgb = [
                end.rgb[0].round() as u8,
                end.rgb[1].round() as u8,
                end.rgb[2].round() as u8,
            ];
            // holding the last frame means holding its intensity too, not snapping back to full master brightness
            self.look.brightness = (self.look.brightness * end.level).clamp(0.0, 100.0);
            if then_off {
                self.look.on = false;
            }
        }
        finished
    }
}
