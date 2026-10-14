//! Animations and presets: built-in ones plus JSON files in the user's library directory (which win).

use crate::assets;
use lampbus_core::Command;
use lampbus_render::{anim, color};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Animations,
    Presets,
}

impl Kind {
    fn dir(self) -> &'static str {
        match self {
            Kind::Animations => "animations",
            Kind::Presets => "presets",
        }
    }

    fn singular(self) -> &'static str {
        match self {
            Kind::Animations => "animation",
            Kind::Presets => "preset",
        }
    }
}

#[derive(Clone)]
pub struct Library {
    dir: PathBuf,
}

impl Library {
    pub fn new(dir: PathBuf) -> Library {
        Library { dir }
    }

    fn path(&self, kind: Kind, name: &str) -> Result<PathBuf, String> {
        if !anim::valid_name(name) {
            return Err("name must be lowercase letters, digits, '-' or '_' (max 40)".into());
        }
        Ok(self.dir.join(kind.dir()).join(format!("{name}.json")))
    }

    fn embedded(kind: Kind) -> &'static [(&'static str, &'static str)] {
        match kind {
            Kind::Animations => assets::ANIMATIONS,
            Kind::Presets => assets::PRESETS,
        }
    }

    /// Every entry, built-in first, overridden by the user's files.
    pub fn all(&self, kind: Kind) -> BTreeMap<String, Value> {
        let mut out = BTreeMap::new();
        for (name, text) in Self::embedded(kind) {
            if let Ok(v) = serde_json::from_str::<Value>(text) {
                out.insert((*name).to_string(), v);
            }
        }
        if let Ok(entries) = std::fs::read_dir(self.dir.join(kind.dir())) {
            for e in entries.filter_map(Result::ok) {
                let file = e.file_name().to_string_lossy().to_string();
                if let (Some(name), Ok(text)) = (
                    file.strip_suffix(".json"),
                    std::fs::read_to_string(e.path()),
                ) {
                    if let Ok(v) = serde_json::from_str::<Value>(&text) {
                        out.insert(name.to_string(), v);
                    }
                }
            }
        }
        out
    }

    pub fn get(&self, kind: Kind, name: &str) -> Result<Value, String> {
        self.all(kind).remove(name).ok_or_else(|| {
            let known: Vec<String> = self.all(kind).into_keys().collect();
            format!(
                "no {} named {name:?}; known: {}",
                kind.singular(),
                known.join(", ")
            )
        })
    }

    pub fn save(&self, kind: Kind, name: &str, spec: Value) -> Result<Value, String> {
        let mut spec = spec;
        match kind {
            Kind::Animations => {
                anim::load(&spec).map_err(|e| e.to_string())?;
            }
            Kind::Presets => validate_preset(&spec)?,
        }
        if let Some(o) = spec.as_object_mut() {
            o.insert("name".into(), json!(name));
        }
        let path = self.path(kind, name)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(
            &tmp,
            serde_json::to_string_pretty(&spec).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
        Ok(spec)
    }

    pub fn delete(&self, kind: Kind, name: &str) -> Result<(), String> {
        std::fs::remove_file(self.path(kind, name)?).map_err(|_| {
            format!(
                "no user {} named {name:?} (built-in entries cannot be deleted)",
                kind.singular()
            )
        })
    }
}

/// A preset sets any one of colour, effect or animation, plus optional power and brightness.
pub fn validate_preset(spec: &Value) -> Result<(), String> {
    const ALLOWED: [&str; 9] = [
        "name",
        "description",
        "title",
        "power",
        "color",
        "brightness",
        "effect",
        "animation",
        "homekit",
    ];
    let o = spec.as_object().ok_or("preset must be an object")?;
    if let Some(k) = o.keys().find(|k| !ALLOWED.contains(&k.as_str())) {
        return Err(format!("unknown preset field {k:?}"));
    }
    if let Some(c) = o.get("color") {
        color::parse(c).map_err(|e| e.to_string())?;
    }
    if let Some(b) = o.get("brightness") {
        if !b.as_f64().is_some_and(|b| (0.0..=100.0).contains(&b)) {
            return Err("brightness must be 0-100".into());
        }
    }
    if ["color", "effect", "animation"]
        .iter()
        .filter(|k| o.contains_key(**k))
        .count()
        > 1
    {
        return Err("a preset can set only one of color / effect / animation".into());
    }
    Ok(())
}

/// The commands that apply a preset.
pub fn preset_commands(lib: &Library, name: &str, spec: &Value) -> Result<Vec<Command>, String> {
    let brightness = spec.get("brightness").and_then(Value::as_f64);
    let mut cmds = Vec::new();
    if let Some(a) = spec.get("animation").and_then(Value::as_str) {
        let animation = anim::load(&lib.get(Kind::Animations, a)?).map_err(|e| e.to_string())?;
        cmds.push(Command::Play {
            animation: std::sync::Arc::new(animation),
            name: Some(a.to_string()),
            brightness,
        });
    } else if let Some(e) = spec.get("effect") {
        let mode = e.get("mode").map_or_else(
            || "1".to_string(),
            |m| m.to_string().trim_matches('"').to_string(),
        );
        cmds.push(Command::Effect {
            effect: mode,
            speed: e.get("speed").and_then(Value::as_f64),
            brightness,
        });
    } else if let Some(c) = spec.get("color") {
        let rgb = color::parse(c).map_err(|e| e.to_string())?;
        cmds.push(Command::Color { rgb, brightness });
    } else if let Some(b) = brightness {
        cmds.push(Command::Brightness(b));
    }
    if let Some(p) = spec.get("power").and_then(Value::as_bool) {
        cmds.push(Command::Power(Some(p)));
    }
    cmds.push(Command::Label(Some(format!("preset:{name}"))));
    Ok(cmds)
}
