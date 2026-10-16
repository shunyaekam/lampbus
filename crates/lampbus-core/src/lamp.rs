//! The per-lamp state machine: what the lamp should show, what the core believes it shows, and the link to it.

use crate::lower::{lower, Frame, Out, Sent};
use crate::{Action, Config, LevelCurve, Ms};
use lampbus_defs::{Caps, Definition, Range};
use lampbus_render::anim::Animation;
use lampbus_render::color::{mix, Rgb, Space};
use lampbus_render::pipeline::{lightness_to_luminance, Calibration, ColourSmoother, Dither};
use std::collections::VecDeque;
use std::sync::Arc;

/// How often a fade renders a new step (about 30 per second; the write pacing keeps it under the lamp's limit).
const FADE_STEP_MS: Ms = 33;

/// Express `units` of a brightness range (`0..=max`) as a brightness byte rounded up plus a scale on the colour, so the
/// product is exact to the colour channels' resolution instead of the byte's. Returns the scaled colour and the byte.
fn split_brightness(rgb: Rgb, units: f64, max: f64) -> (Rgb, f64) {
    if units <= 0.0 || max <= 0.0 {
        return (rgb, 0.0);
    }
    let byte = units.ceil().min(max);
    let s = (units / byte).clamp(0.0, 1.0);
    ([rgb[0] * s, rgb[1] * s, rgb[2] * s], byte)
}

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

/// A fade of what is shown, from one colour and level to another.
struct Fade {
    from: (Rgb, f64),
    to: (Rgb, f64),
    t0: Ms,
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
    smoother: ColourSmoother,
    last_smooth: Option<Ms>,
    dither: Dither,
    /// Whether the last error came from lowering (so a later successful lowering may clear it).
    lower_err: bool,
    /// Whether any state command has been given; an untouched lamp is left alone when it connects.
    pub touched: bool,
    /// Streams are ignored until this time (a person took over).
    pub yield_until: Ms,
    /// What is being shown, before quantising: colour and level (`0..=1`). `None` when unknown (a firmware effect).
    shown: Option<(Rgb, f64)>,
    fade: Option<Fade>,
    fade_tick: Ms,
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
            smoother: ColourSmoother::new(cfg.stream_attack_ms, cfg.stream_release_ms),
            last_smooth: None,
            dither: Dither::new(),
            lower_err: false,
            touched: false,
            yield_until: 0,
            shown: None,
            fade: None,
            fade_tick: 0,
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

    pub(crate) fn fading(&self) -> bool {
        self.fade.is_some()
    }

    pub(crate) fn pending(&self) -> usize {
        self.queue.len() + self.raw.len()
    }

    fn nothing_queued(&self) -> bool {
        self.queue.is_empty() && self.raw.is_empty()
    }

    /// Whether the lamp needs a live connection right now.
    pub(crate) fn wants_link(&self, now: Ms) -> bool {
        !self.nothing_queued()
            || self.animating()
            || self.fade.is_some()
            || (self.look.on && self.streaming(now))
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

    /// Stop an animation and keep what it is showing right now (colour and intensity) as the static look.
    pub(crate) fn freeze_animation(&mut self, now: Ms) {
        if let Some(run) = self.look.anim.take() {
            let s = run.anim.sample(now.saturating_sub(run.t0) as f64 / 1000.0);
            self.look.rgb = [
                s.rgb[0].round() as u8,
                s.rgb[1].round() as u8,
                s.rgb[2].round() as u8,
            ];
            self.look.brightness = (self.look.brightness * s.level).clamp(0.0, 100.0);
        }
        self.dirty = true;
    }

    pub(crate) fn set_stream(&mut self, rgb: Rgb, level: f64, now: Ms, cfg: &Config) {
        if !self.look.on || now < self.yield_until {
            return;
        }
        self.stream = Some(Stream {
            rgb,
            level: level.clamp(0.0, 1.0),
            until: now + cfg.stream_timeout_ms,
        });
        self.dirty = true;
    }

    /// A person gave an explicit colour, effect or animation: a live stream pauses (if configured) so they win.
    pub(crate) fn yield_to_person(&mut self, now: Ms, cfg: &Config) {
        if cfg.stream_yield_ms > 0 {
            self.yield_until = now + cfg.stream_yield_ms;
            self.stream = None;
        }
    }

    /// See [`Sent::forget_power`]: explicit commands are always sent, whatever the core believes.
    pub(crate) fn forget_for(&mut self, cmd: &crate::Command) {
        use crate::Command::{Brightness, Color, Effect, Hs, Power};
        match cmd {
            Power(_) => self.sent.forget_power(),
            Color { .. } | Hs { .. } => {
                self.sent.forget_power();
                self.sent.forget_color();
            }
            Brightness(_) => self.sent.forget_level(),
            Effect { .. } => {
                self.sent.forget_power();
                self.sent.forget_effect();
            }
            _ => {}
        }
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
        let g = cfg.white_balance;
        [
            cal.apply(rgb[0] / 255.0 * g[0]) * 255.0,
            cal.apply(rgb[1] / 255.0 * g[1]) * 255.0,
            cal.apply(rgb[2] / 255.0 * g[2]) * 255.0,
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
            Some(r) => {
                let units = level_units(r, level);
                let combined = color.is_some_and(|c| c.brightness.is_some());
                let (rgb, units) = if cfg.split_brightness && combined {
                    split_brightness(rgb, units, r.max)
                } else {
                    (rgb, units)
                };
                (
                    [rgb[0], rgb[1], rgb[2], units],
                    [255, 255, 255, r.max.clamp(0.0, 255.0) as u8],
                )
            }
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

    /// Move what is shown toward `target` over `cfg.transition_ms`, eased in and out and mixed in linear light. A new
    /// target mid-fade starts from wherever the fade has got to, so a stream of changes (a slider drag) stays smooth.
    /// With no transition configured, or nothing known to fade from, the change is instant.
    fn transition(&mut self, now: Ms, target: (Rgb, f64), cfg: &Config) -> (Rgb, f64) {
        let dur = cfg.transition_ms;
        let Some(cur) = self.shown.filter(|_| dur > 0) else {
            self.fade = None;
            self.shown = Some(target);
            return target;
        };
        if self.fade.as_ref().map_or(cur != target, |f| f.to != target) {
            self.fade = Some(Fade {
                from: cur,
                to: target,
                t0: now,
            });
        }
        let Some(f) = &self.fade else {
            return target;
        };
        let p = (now.saturating_sub(f.t0) as f64 / dur as f64).clamp(0.0, 1.0);
        if p >= 1.0 {
            self.fade = None;
            self.shown = Some(target);
            return target;
        }
        let e = p * p * (3.0 - 2.0 * p);
        let v = (
            mix(f.from.0, f.to.0, e, Space::Linear),
            f.from.1 + (f.to.1 - f.from.1) * e,
        );
        self.shown = Some(v);
        v
    }

    /// Decide what the lamp should show now, lower it to frames, and replace the pending queue with them.
    pub(crate) fn rerender(&mut self, now: Ms, cfg: &Config) {
        self.dirty = false;
        if !self.touched {
            return;
        }
        let master = self.look.brightness / 100.0;
        let static_rgb = [
            f64::from(self.look.rgb[0]),
            f64::from(self.look.rgb[1]),
            f64::from(self.look.rgb[2]),
        ];
        let out = if !self.look.on {
            self.smoother.reset();
            self.last_smooth = None;
            // fade out rather than cut: dim the colour that is showing, then switch off
            let rgb = self.shown.map_or(static_rgb, |s| s.0);
            let (rgb, level) = self.transition(now, (rgb, 0.0), cfg);
            if level > 0.0 {
                self.quantise_color(rgb, level, false, cfg)
            } else {
                Out::Off
            }
        } else if let Some(s) = self.stream.as_ref().filter(|s| now < s.until) {
            let (rgb, level) = (s.rgb, s.level);
            let dt = self.last_smooth.map_or(0, |t| now.saturating_sub(t)) as f64;
            self.last_smooth = Some(now);
            let [r, g, b, l] = self
                .smoother
                .step([rgb[0], rgb[1], rgb[2], level * 255.0], dt);
            // a stream is already continuous: follow it, so a later change starts from where it was
            self.fade = None;
            self.shown = Some(([r, g, b], l / 255.0 * master));
            self.quantise_color([r, g, b], l / 255.0 * master, true, cfg)
        } else if let Some(run) = &self.look.anim {
            let s = run.anim.sample(now.saturating_sub(run.t0) as f64 / 1000.0);
            self.fade = None;
            self.shown = Some((s.rgb, s.level * master));
            self.quantise_color(s.rgb, s.level * master, true, cfg)
        } else if let Some((mode, speed)) = self.look.effect {
            // a firmware effect cannot be faded, and what it shows is unknown
            self.fade = None;
            self.shown = None;
            self.effect_out(mode, speed, cfg)
        } else {
            let (rgb, level) = self.transition(now, (static_rgb, master), cfg);
            self.quantise_color(rgb, level, false, cfg)
        };
        if self.fade.is_some() {
            self.fade_tick = now + FADE_STEP_MS;
        }
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
            && self.fade.is_none()
            && self.stream.is_none())
        .then(|| self.last_activity + cfg.idle_ms)
    }

    pub(crate) fn next_deadline(&self, cfg: &Config, now: Ms) -> Option<Ms> {
        let write = (self.link == Link::Ready && !self.nothing_queued())
            .then(|| self.next_write_at.max(now));
        let link = match self.link {
            // once the back-off has elapsed there is nothing to wake for: the lamp waits for a scan result, not for time
            Link::Backoff { until } if until > now && self.wants_link(now) => Some(until),
            Link::Connecting { since } => Some(since + cfg.connect_timeout_ms),
            _ => None,
        };
        [
            self.tick_due(),
            self.fade.as_ref().map(|_| self.fade_tick),
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
        if self.fade.is_some() && now >= self.fade_tick {
            self.dirty = true; // the next step of a fade
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_split_is_exact_and_never_brightens_the_colour() {
        let rgb = [255.0, 120.0, 10.0];
        let mut units = 0.0;
        while units <= 100.0 {
            let (c, byte) = split_brightness(rgb, units, 100.0);
            assert!(
                byte >= units && byte <= 100.0 && byte.fract() == 0.0,
                "{units}: byte {byte}"
            );
            assert!(
                c.iter().zip(rgb).all(|(a, b)| *a <= b + 1e-9),
                "{units}: the colour is only ever scaled down"
            );
            if units > 0.0 {
                let product = byte * c[0] / rgb[0];
                assert!(
                    (product - units).abs() < 1e-9,
                    "{units}: byte and colour multiply back to it, got {product}"
                );
            }
            units += 0.173;
        }
    }

    #[test]
    fn rounded_to_the_colour_channels_the_error_is_far_below_one_brightness_step() {
        // brightness byte alone: up to 0.5 units of error. Split: bounded by half a colour step at the scaled value.
        let rgb = [255.0, 0.0, 0.0];
        let mut worst: f64 = 0.0;
        let mut units = 1.0;
        while units < 100.0 {
            let (c, byte) = split_brightness(rgb, units, 100.0);
            let shown = byte * c[0].round() / 255.0;
            worst = worst.max((shown - units).abs());
            units += 0.0731;
        }
        assert!(
            worst < 0.2,
            "worst error {worst} brightness units (the byte alone would be up to 0.5)"
        );
    }

    #[test]
    fn nothing_to_split_when_off() {
        assert_eq!(
            split_brightness([1.0, 2.0, 3.0], 0.0, 100.0),
            ([1.0, 2.0, 3.0], 0.0)
        );
    }
}
