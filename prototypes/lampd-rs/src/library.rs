//! Presets and animations as plain JSON files (git-friendly, hand- or agent-editable).
//! Same on-disk layout as the Python build: <root>/animations/*.json and <root>/presets/*.json.

use crate::anim;
use serde_json::{Map, Value};
use std::fs;
use std::path::PathBuf;

#[derive(Clone, Copy)]
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

pub enum LibError {
    NotFound(String),
    Invalid(String),
}

pub struct Library {
    root: PathBuf,
}

impl Library {
    pub fn new(root: PathBuf) -> std::io::Result<Self> {
        for k in [Kind::Animations, Kind::Presets] {
            fs::create_dir_all(root.join(k.dir()))?;
        }
        Ok(Self { root })
    }

    fn path(&self, kind: Kind, name: &str) -> Result<PathBuf, LibError> {
        if !anim::valid_name(name) {
            return Err(LibError::Invalid("name must be lowercase letters, digits, '-' or '_' (max 40)".into()));
        }
        Ok(self.root.join(kind.dir()).join(format!("{name}.json")))
    }

    pub fn names(&self, kind: Kind) -> Vec<String> {
        let mut out: Vec<String> = fs::read_dir(self.root.join(kind.dir())).into_iter().flatten().flatten()
            .filter_map(|e| e.file_name().to_str()?.strip_suffix(".json").map(String::from)).collect();
        out.sort();
        out
    }

    pub fn get(&self, kind: Kind, name: &str) -> Result<Value, LibError> {
        let text = fs::read_to_string(self.path(kind, name)?)
            .map_err(|_| LibError::NotFound(format!("no {} named {name:?}", kind.singular())))?;
        let mut spec: Value = serde_json::from_str(&text).map_err(|e| LibError::Invalid(format!("{name}: {e}")))?;
        if let Some(o) = spec.as_object_mut() {
            o.entry("name").or_insert_with(|| Value::String(name.into()));
        }
        Ok(spec)
    }

    /// Every readable entry; one bad file must not hide the rest.
    pub fn all(&self, kind: Kind) -> Map<String, Value> {
        self.names(kind).into_iter().filter_map(|n| self.get(kind, &n).ok().map(|v| (n, v))).collect()
    }

    pub fn save(&self, kind: Kind, name: &str, spec: Value) -> Result<Value, LibError> {
        let mut spec = match spec {
            Value::Object(o) => o,
            _ => return Err(LibError::Invalid("body must be a JSON object".into())),
        };
        spec.insert("name".into(), Value::String(name.into()));
        let spec = Value::Object(spec);
        match kind {
            Kind::Animations => drop(anim::load(&spec).map_err(LibError::Invalid)?),
            Kind::Presets => validate_preset(&spec).map_err(LibError::Invalid)?,
        }
        let path = self.path(kind, name)?;
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, serde_json::to_string_pretty(&spec).unwrap()).map_err(|e| LibError::Invalid(e.to_string()))?;
        fs::rename(&tmp, &path).map_err(|e| LibError::Invalid(e.to_string()))?;
        Ok(spec)
    }

    pub fn delete(&self, kind: Kind, name: &str) -> Result<(), LibError> {
        fs::remove_file(self.path(kind, name)?).map_err(|_| LibError::NotFound(format!("no {} named {name:?}", kind.singular())))
    }
}

/// A preset is any combination of: power, color, brightness, effect{mode,speed}, animation.
fn validate_preset(spec: &Value) -> Result<(), String> {
    const ALLOWED: [&str; 9] = ["name", "description", "title", "power", "color", "brightness", "effect", "animation", "homekit"];
    let o = spec.as_object().ok_or("preset must be an object")?;
    let extra: Vec<&String> = o.keys().filter(|k| !ALLOWED.contains(&k.as_str())).collect();
    if !extra.is_empty() {
        return Err(format!("unknown preset fields: {extra:?}"));
    }
    if let Some(c) = o.get("color") {
        anim::parse_color(c)?;
    }
    if let Some(b) = o.get("brightness") {
        if !b.as_f64().is_some_and(|b| (1.0..=100.0).contains(&b)) {
            return Err("brightness must be 1-100".into());
        }
    }
    if ["color", "effect", "animation"].iter().filter(|k| o.contains_key(**k)).count() > 1 {
        return Err("a preset can set only one of color / effect / animation".into());
    }
    Ok(())
}
