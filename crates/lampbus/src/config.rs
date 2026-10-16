//! Configuration: a small JSON file, optional. With no file, `lampbus` adopts the first lamp it finds.

use lampbus_core::{Config, DeviceConfig, LevelCurve};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileDevice {
    alias: String,
    name: Option<String>,
    id: Option<String>,
    definition: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
struct File {
    devices: Vec<FileDevice>,
    auto_adopt: Option<bool>,
    idle_seconds: Option<f64>,
    stream_yield_seconds: Option<f64>,
    transition_ms: Option<f64>,
    split_brightness: Option<bool>,
    color_gamma: Option<f64>,
    white_balance: Option<[f64; 3]>,
    level_curve: Option<String>,
    dither: Option<bool>,
    definitions_dir: Option<String>,
    library_dir: Option<String>,
    bind: Option<String>,
    udp_stream: Option<String>,
    ddp: Option<String>,
    stream_device: Option<String>,
    controller: Option<String>,
    controller_direct: Option<bool>,
    schedules: Vec<FileSchedule>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileSchedule {
    name: Option<String>,
    at: String,
    days: Option<Value>,
    control: Value,
}

/// Everything the binary needs besides the core's own [`Config`].
pub struct Settings {
    pub core: Config,
    pub definitions_dir: Option<PathBuf>,
    pub library_dir: PathBuf,
    pub bind: String,
    pub config_dir: PathBuf,
    /// UDP address for simple `r g b [brightness]` frames (off unless set).
    pub udp_stream: Option<String>,
    /// UDP address for DDP packets (off unless set).
    pub ddp: Option<String>,
    /// Lamp that live input drives (default: all).
    pub stream_device: Option<String>,
    /// UDP address of an external controller that owns the lamp (see `forward.rs`).
    pub controller: Option<String>,
    /// Start with the controller bypassed: lampbus executes commands itself.
    pub controller_direct: bool,
    pub schedules: Vec<crate::schedule::Entry>,
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from)
}

fn expand(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None => PathBuf::from(p),
    }
}

pub fn default_dir() -> PathBuf {
    std::env::var_os("LAMPBUS_HOME")
        .map_or_else(|| home().join(".config").join("lampbus"), PathBuf::from)
}

/// Load settings from `path`, or from the default location, or fall back to zero-config defaults.
pub fn load(path: Option<&Path>) -> Result<Settings, String> {
    let config_dir = default_dir();
    let path = path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| config_dir.join("config.json"));
    let file: File = match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => File::default(),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let mut core = Config::default();
    core.devices = file
        .devices
        .into_iter()
        .map(|d| DeviceConfig {
            alias: d.alias,
            name: d.name,
            id: d.id,
            definition: d.definition,
        })
        .collect();
    // with no lamps configured, find whatever is nearby
    core.auto_adopt = file.auto_adopt.unwrap_or(core.devices.is_empty());
    if let Some(s) = file.idle_seconds {
        core.idle_ms = (s.max(0.0) * 1000.0) as u64;
    }
    core.split_brightness = file.split_brightness.unwrap_or(true);
    if let Some(s) = file.stream_yield_seconds {
        core.stream_yield_ms = (s.max(0.0) * 1000.0) as u64;
    }
    // colour and brightness changes glide by default (0 turns it off): analog, not stepped
    core.transition_ms = file
        .transition_ms
        .map_or(300, |ms| ms.clamp(0.0, 5_000.0) as u64);
    if let Some(g) = file.color_gamma {
        if !(0.1..=5.0).contains(&g) {
            return Err("color_gamma must be between 0.1 and 5".into());
        }
        core.color_gamma = g;
    }
    if let Some(w) = file.white_balance {
        if w.iter().any(|g| !(0.0..=1.0).contains(g)) {
            return Err(
                "white_balance must be three gains between 0 and 1, e.g. [0.85, 1, 0.75]".into(),
            );
        }
        core.white_balance = w;
    }
    core.level_curve = match file.level_curve.as_deref() {
        None | Some("linear") => LevelCurve::Linear,
        Some("perceptual") => LevelCurve::Perceptual,
        Some(other) => {
            return Err(format!(
                "level_curve must be 'linear' or 'perceptual', not {other:?}"
            ))
        }
    };
    if let Some(d) = file.dither {
        core.dither = d;
    }
    let mut schedules = Vec::new();
    for (i, sc) in file.schedules.into_iter().enumerate() {
        schedules.push(crate::schedule::Entry {
            name: sc.name.unwrap_or_else(|| format!("schedule {}", i + 1)),
            minute: crate::schedule::parse_time(&sc.at)?,
            days: crate::schedule::parse_days(sc.days.as_ref())?,
            control: sc.control,
        });
    }
    Ok(Settings {
        core,
        definitions_dir: file.definitions_dir.as_deref().map(expand),
        library_dir: file
            .library_dir
            .as_deref()
            .map_or_else(|| config_dir.join("library"), expand),
        bind: file.bind.unwrap_or_else(|| "127.0.0.1:8787".into()),
        config_dir,
        udp_stream: file.udp_stream,
        ddp: file.ddp,
        stream_device: file.stream_device,
        controller: file.controller,
        controller_direct: file.controller_direct.unwrap_or(false)
            || std::env::var("LAMPBUS_DIRECT").is_ok_and(|v| !v.is_empty() && v != "0"),
        schedules,
    })
}

/// Load the built-in definitions plus any in `extra`, with later (user) ones replacing earlier ones by id.
pub fn definitions(extra: Option<&Path>) -> Result<Vec<lampbus_defs::Definition>, String> {
    let mut defs: Vec<lampbus_defs::Definition> = Vec::new();
    let mut add = |d: lampbus_defs::Definition| {
        defs.retain(|x| x.id != d.id);
        defs.push(d);
    };
    for text in crate::assets::DEFINITIONS {
        add(lampbus_defs::Definition::from_json(text)
            .map_err(|e| format!("built-in definition: {e}"))?);
    }
    if let Some(dir) = extra {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Ok(defs);
        };
        let mut files: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
        files.sort();
        for p in files {
            let name = p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string();
            if name.ends_with(".json") && !name.ends_with(".vectors.json") {
                let text =
                    std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
                add(lampbus_defs::Definition::from_json(&text)
                    .map_err(|e| format!("{}: {e}", p.display()))?);
            }
        }
    }
    Ok(defs)
}

/// The API token: from `LAMPBUS_TOKEN`, else a file in the config directory (created on first use, mode 0600).
pub fn token(dir: &Path) -> Result<String, String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if let Ok(t) = std::env::var("LAMPBUS_TOKEN") {
        return Ok(t);
    }
    let path = dir.join("token");
    if !path.exists() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let mut raw = [0u8; 24];
        getrandom::fill(&mut raw).map_err(|e| format!("no system randomness: {e}"))?;
        const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut text = String::new();
        for chunk in raw.chunks(3) {
            let n = chunk
                .iter()
                .enumerate()
                .fold(0u32, |a, (i, b)| a | u32::from(*b) << (16 - 8 * i));
            for i in 0..=chunk.len() {
                text.extend(
                    A.get(((n >> (18 - 6 * i)) & 63) as usize)
                        .map(|c| *c as char),
                );
            }
        }
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| e.to_string())?;
        f.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    }
    Ok(std::fs::read_to_string(&path)
        .map_err(|e| e.to_string())?
        .trim()
        .to_string())
}

/// Summaries of the loaded lamp definitions, for `/api/definitions`.
pub fn summarise(defs: &[lampbus_defs::Definition]) -> Value {
    Value::Array(
        defs.iter()
            .map(|d| {
                let commands: serde_json::Map<String, Value> = d
                    .commands
                    .iter()
                    .map(|(n, c)| {
                        let params: serde_json::Map<String, Value> =
                            c.params.iter().map(|(p, s)| (p.clone(), json!({"min": s.min, "max": s.max, "default": s.default}))).collect();
                        (n.clone(), Value::Object(params))
                    })
                    .collect();
                json!({"id": d.id, "name": d.name, "match": {"name_prefix": d.matching.name_prefix, "service_uuids": d.matching.service_uuids},
                       "roles": d.roles, "effects": d.effects.iter().map(|e| json!({"id": e.id, "name": e.name, "aliases": e.aliases})).collect::<Vec<_>>(),
                       "commands": commands})
            })
            .collect(),
    )
}
