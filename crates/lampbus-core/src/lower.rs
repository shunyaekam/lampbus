//! Lowering: turn "what the lamp should show" into the protocol frames that get it there.
//!
//! The core tracks what it believes the lamp is showing ([`Sent`]) and emits only the frames needed to reach the
//! target, so a lamp with a separate brightness command is not re-sent its colour when only the brightness changed.
//! All commands are absolute (never relative), so a lost or repeated frame is harmless and a reconnect simply
//! forgets [`Sent`] and resends everything.

use lampbus_defs::{Caps, Definition};

/// What a lamp is currently displaying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Showing {
    Color,
    Effect,
}

/// The target of a lowering, after calibration, scaling and quantisation to device units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Out {
    Off,
    Color {
        rgb: [u8; 3],
        level: Option<u8>,
    },
    Effect {
        mode: u8,
        speed: Option<u8>,
        level: Option<u8>,
    },
}

/// Facts a frame establishes once written.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Delta {
    power: Option<bool>,
    color: Option<[u8; 3]>,
    level: Option<u8>,
    speed: Option<u8>,
    effect: Option<u8>,
    showing: Option<Showing>,
}

/// What the core believes the lamp shows. `None` means unknown (so it will be sent).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Sent {
    power: Option<bool>,
    color: Option<[u8; 3]>,
    level: Option<u8>,
    speed: Option<u8>,
    effect: Option<u8>,
    showing: Option<Showing>,
}

impl Sent {
    pub(crate) fn apply(&mut self, d: &Delta) {
        macro_rules! take {
            ($($f:ident),*) => { $( if d.$f.is_some() { self.$f = d.$f; } )* };
        }
        take!(power, color, level, speed, effect, showing);
    }
}

/// One wire-ready frame plus what it establishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Frame {
    pub wire: Vec<u8>,
    pub delta: Delta,
}

fn enc(def: &Definition, cmd: &str, args: &[(&str, f64)], delta: Delta) -> Result<Frame, String> {
    let e = def.encode(cmd, args).map_err(|e| e.to_string())?;
    Ok(Frame {
        wire: e.wire,
        delta,
    })
}

fn off_frames(def: &Definition, caps: &Caps, sent: &Sent) -> Result<Vec<Frame>, String> {
    if sent.power == Some(false) {
        return Ok(Vec::new());
    }
    let off = Delta {
        power: Some(false),
        ..Delta::default()
    };
    if let Some(c) = &caps.off {
        return Ok(vec![enc(def, c, &[], off)?]);
    }
    // no off command: black is off
    let Some(color) = &caps.color else {
        return Err("this lamp has no way to turn off".into());
    };
    let mut args: Vec<(&str, f64)> = vec![("r", 0.0), ("g", 0.0), ("b", 0.0)];
    if let Some(r) = color.brightness {
        args.push(("brightness", r.min));
    }
    Ok(vec![enc(
        def,
        &color.command,
        &args,
        Delta {
            color: Some([0, 0, 0]),
            showing: Some(Showing::Color),
            ..off
        },
    )?])
}

/// Frames that bring a lamp believed to show `sent` to `out`.
pub(crate) fn lower(
    def: &Definition,
    caps: &Caps,
    sent: &Sent,
    out: &Out,
) -> Result<Vec<Frame>, String> {
    match *out {
        Out::Off => off_frames(def, caps, sent),
        Out::Color { rgb, level } => lower_color(def, caps, sent, rgb, level),
        Out::Effect { mode, speed, level } => lower_effect(def, caps, sent, mode, speed, level),
    }
}

fn power_on(
    def: &Definition,
    caps: &Caps,
    sent: &Sent,
    frames: &mut Vec<Frame>,
) -> Result<(), String> {
    if sent.power != Some(true) {
        if let Some(on) = &caps.on {
            frames.push(enc(
                def,
                on,
                &[],
                Delta {
                    power: Some(true),
                    ..Delta::default()
                },
            )?);
        }
    }
    Ok(())
}

fn lower_color(
    def: &Definition,
    caps: &Caps,
    sent: &Sent,
    rgb: [u8; 3],
    level: Option<u8>,
) -> Result<Vec<Frame>, String> {
    let color = caps.color.as_ref().ok_or("this lamp cannot show colours")?;
    let mut frames = Vec::new();
    power_on(def, caps, sent, &mut frames)?;
    let inline = color.brightness.is_some();
    // separate brightness command
    if let (false, Some(b), Some(l)) = (inline, &caps.brightness, level) {
        if sent.level != Some(l) {
            frames.push(enc(
                def,
                &b.command,
                &[("level", f64::from(l))],
                Delta {
                    level: Some(l),
                    ..Delta::default()
                },
            )?);
        }
    }
    // a lamp with no `on` command is powered on by the colour frame itself
    let needs_color = sent.color != Some(rgb)
        || sent.showing != Some(Showing::Color)
        || (inline && sent.level != level)
        || (caps.on.is_none() && sent.power != Some(true));
    if needs_color {
        let mut args: Vec<(&str, f64)> = vec![
            ("r", f64::from(rgb[0])),
            ("g", f64::from(rgb[1])),
            ("b", f64::from(rgb[2])),
        ];
        let mut delta = Delta {
            power: Some(true),
            color: Some(rgb),
            showing: Some(Showing::Color),
            ..Delta::default()
        };
        if let (true, Some(l)) = (inline, level) {
            args.push(("brightness", f64::from(l)));
            delta.level = Some(l);
        }
        frames.push(enc(def, &color.command, &args, delta)?);
    }
    Ok(frames)
}

fn lower_effect(
    def: &Definition,
    caps: &Caps,
    sent: &Sent,
    mode: u8,
    speed: Option<u8>,
    level: Option<u8>,
) -> Result<Vec<Frame>, String> {
    let effect = caps
        .effect
        .as_ref()
        .ok_or("this lamp has no built-in effects")?;
    let mut frames = Vec::new();
    power_on(def, caps, sent, &mut frames)?;
    let inline_level = effect.brightness.is_some();
    let inline_speed = effect.speed.is_some();
    if let (false, Some(b), Some(l)) = (inline_level, &caps.brightness, level) {
        if sent.level != Some(l) {
            frames.push(enc(
                def,
                &b.command,
                &[("level", f64::from(l))],
                Delta {
                    level: Some(l),
                    ..Delta::default()
                },
            )?);
        }
    }
    if let (false, Some(s), Some(v)) = (inline_speed, &caps.effect_speed, speed) {
        if sent.speed != Some(v) {
            frames.push(enc(
                def,
                &s.command,
                &[("value", f64::from(v))],
                Delta {
                    speed: Some(v),
                    ..Delta::default()
                },
            )?);
        }
    }
    let needs = sent.effect != Some(mode)
        || sent.showing != Some(Showing::Effect)
        || (inline_speed && sent.speed != speed)
        || (inline_level && sent.level != level);
    if needs {
        let mut args: Vec<(&str, f64)> = vec![("mode", f64::from(mode))];
        let mut delta = Delta {
            power: Some(true),
            effect: Some(mode),
            showing: Some(Showing::Effect),
            ..Delta::default()
        };
        if let (true, Some(v)) = (inline_speed, speed) {
            args.push(("speed", f64::from(v)));
            delta.speed = Some(v);
        }
        if let (true, Some(l)) = (inline_level, level) {
            args.push(("brightness", f64::from(l)));
            delta.level = Some(l);
        }
        frames.push(enc(def, &effect.command, &args, delta)?);
    }
    Ok(frames)
}
