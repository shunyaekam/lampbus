//! The one control language. The CLI, the HTTP API and the MCP server all speak it, so a lamp behaves the same
//! whichever way it is driven.
//!
//! ```text
//! {"device": "desk",                       // optional; default: every lamp
//!  "preset": "reading",                    // or any ONE of:
//!  "color": "#ff8800" | "warm" | [r,g,b] | {"h":0-360,"s":0-100,"v":0-100},
//!  "hue": 0-360, "saturation": 0-100,      //   (alternative to color)
//!  "effect": "rainbow" | 1,                //   built-in effect by name, alias or id
//!  "animation": "candle" | {inline spec},
//!  "brightness": 0-100, "speed": 0-100,    // modifiers (brightness alone just changes brightness)
//!  "stop_animation": true,
//!  "power": true | false | "toggle"}       // applied last
//! ```

use crate::library::{preset_commands, Kind, Library};
use lampbus_core::{Command, Target};
use lampbus_render::{anim, color};
use lampbus_shell::Handle;
use serde_json::Value;
use std::sync::Arc;

const KEYS: [&str; 12] = [
    "device",
    "preset",
    "color",
    "hue",
    "saturation",
    "effect",
    "animation",
    "brightness",
    "speed",
    "stop_animation",
    "power",
    "label",
];

#[derive(Clone)]
pub struct Ctx {
    pub handle: Handle,
    pub library: Library,
    /// With no lamps configured: scan for the first matching lamp before acting.
    pub auto_discover: bool,
}

impl Ctx {
    /// Make sure at least one lamp is known, scanning for up to `limit` if configured to.
    pub async fn ensure_lamps(&self, limit: std::time::Duration) {
        if !self.auto_discover || !self.handle.snapshot().lamps.is_empty() {
            return;
        }
        self.handle.discover().await;
        let mut rx = self.handle.subscribe();
        let _ = tokio::time::timeout(limit, async {
            while rx.borrow().lamps.is_empty() {
                if rx.changed().await.is_err() {
                    break;
                }
            }
        })
        .await;
    }
}

fn number(v: &Value, key: &str) -> Result<Option<f64>, String> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(x) => x
            .as_f64()
            .filter(|n| n.is_finite())
            .map(Some)
            .ok_or_else(|| format!("{key} must be a number")),
    }
}

/// Turn a control object into commands, in the order they must be applied.
pub fn commands(library: &Library, args: &Value) -> Result<(Target, Vec<Command>), String> {
    let obj = args.as_object().ok_or("control must be a JSON object")?;
    if let Some(k) = obj.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(format!("unknown field {k:?}; known: {}", KEYS.join(", ")));
    }
    let target = match obj.get("device").and_then(Value::as_str) {
        Some(d) => Target::Named(d.to_string()),
        None => Target::All,
    };
    let (brightness, speed) = (number(args, "brightness")?, number(args, "speed")?);
    let modes = ["color", "hue", "effect", "animation", "preset"]
        .iter()
        .filter(|k| obj.contains_key(**k))
        .count();
    if modes > 1 {
        return Err("give only one of color/hue, effect, animation, preset".into());
    }
    let mut cmds = Vec::new();
    if let Some(name) = obj.get("preset").and_then(Value::as_str) {
        let spec = library.get(Kind::Presets, name)?;
        cmds.extend(preset_commands(library, name, &spec)?);
    } else if let Some(a) = obj.get("animation") {
        let (name, spec) = match a {
            Value::String(n) => (Some(n.clone()), library.get(Kind::Animations, n)?),
            Value::Object(_) => (None, a.clone()),
            _ => return Err("animation must be a library name or an inline spec object".into()),
        };
        let animation = Arc::new(anim::load(&spec).map_err(|e| e.to_string())?);
        cmds.push(Command::Play {
            animation,
            name,
            brightness,
        });
    } else if let Some(e) = obj.get("effect") {
        let effect = match e {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            _ => return Err("effect must be a name or an id".into()),
        };
        cmds.push(Command::Effect {
            effect,
            speed,
            brightness,
        });
    } else if let Some(c) = obj.get("color") {
        cmds.push(Command::Color {
            rgb: color::parse(c).map_err(|e| e.to_string())?,
            brightness,
        });
    } else if let Some(hue) = number(args, "hue")? {
        cmds.push(Command::Hs {
            hue,
            sat: number(args, "saturation")?.unwrap_or(100.0),
            brightness,
        });
    } else if let Some(b) = brightness {
        cmds.push(Command::Brightness(b));
    }
    if obj.get("stop_animation").and_then(Value::as_bool) == Some(true) {
        cmds.push(Command::StopAnimation);
    }
    match obj.get("power") {
        None | Some(Value::Null) => {}
        Some(Value::Bool(b)) => cmds.push(Command::Power(Some(*b))),
        Some(Value::String(s)) if s == "toggle" => cmds.push(Command::Power(None)),
        Some(_) => return Err("power must be true, false or \"toggle\"".into()),
    }
    if let Some(l) = obj.get("label").and_then(Value::as_str) {
        cmds.push(Command::Label(Some(l.to_string())));
    }
    if cmds.is_empty() {
        return Err("nothing to do: give at least one of preset, color, hue, effect, animation, brightness, power, stop_animation".into());
    }
    Ok((target, cmds))
}

/// Apply a control object and return the resulting state.
pub async fn control(ctx: &Ctx, args: &Value) -> Result<Value, String> {
    let (target, cmds) = commands(&ctx.library, args)?;
    ctx.ensure_lamps(std::time::Duration::from_secs(12)).await;
    for c in cmds {
        ctx.handle
            .command(target.clone(), c)
            .await
            .map_err(|e| e.to_string())?;
    }
    serde_json::to_value(ctx.handle.snapshot()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lib() -> Library {
        Library::new(std::env::temp_dir().join("lampbus-test-nonexistent"))
    }

    #[test]
    fn maps_fields_to_commands_in_order() {
        let (t, c) = commands(
            &lib(),
            &json!({"device": "desk", "color": "red", "brightness": 40, "power": true}),
        )
        .unwrap();
        assert_eq!(t, Target::Named("desk".into()));
        assert!(
            matches!(c.as_slice(), [Command::Color { brightness: Some(b), .. }, Command::Power(Some(true))] if (*b - 40.0).abs() < 1e-9)
        );
        let (t, c) = commands(&lib(), &json!({"brightness": 10})).unwrap();
        assert_eq!(t, Target::All);
        assert!(matches!(c.as_slice(), [Command::Brightness(_)]));
        let (_, c) = commands(&lib(), &json!({"effect": 5, "speed": 30})).unwrap();
        assert!(
            matches!(c.as_slice(), [Command::Effect { effect, speed: Some(_), .. }] if effect == "5")
        );
        let (_, c) = commands(&lib(), &json!({"animation": "candle"})).unwrap();
        assert!(matches!(c.as_slice(), [Command::Play { name: Some(n), .. }] if n == "candle"));
        let (_, c) = commands(&lib(), &json!({"preset": "reading"})).unwrap();
        assert!(matches!(c.last(), Some(Command::Label(Some(l))) if l == "preset:reading"));
        let (_, c) = commands(&lib(), &json!({"hue": 120})).unwrap();
        assert!(matches!(c.as_slice(), [Command::Hs { sat, .. }] if (*sat - 100.0).abs() < 1e-9));
        let (_, c) = commands(&lib(), &json!({"power": "toggle"})).unwrap();
        assert!(matches!(c.as_slice(), [Command::Power(None)]));
    }

    #[test]
    fn rejects_mistakes_instead_of_guessing() {
        for bad in [
            json!({}),
            json!({"colour": "red"}),
            json!({"color": "red", "effect": "rainbow"}),
            json!({"color": "nope"}),
            json!({"brightness": "bright"}),
            json!({"power": "maybe"}),
            json!({"animation": 5}),
            json!({"animation": "no-such-animation"}),
            json!({"preset": "no-such-preset"}),
            json!([1, 2]),
        ] {
            assert!(commands(&lib(), &bad).is_err(), "accepted {bad}");
        }
    }

    #[test]
    fn inline_animations_are_validated() {
        let ok = json!({"animation": {"type": "gen", "gen": "rainbow"}});
        assert!(commands(&lib(), &ok).is_ok());
        let bad = json!({"animation": {"type": "gen", "gen": "os.system"}});
        assert!(commands(&lib(), &bad).is_err());
    }
}
