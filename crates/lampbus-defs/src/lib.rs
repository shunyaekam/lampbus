//! Data-driven lamp definitions.
//!
//! A [`Definition`] describes how to recognise a Bluetooth lamp, which GATT characteristic to write to, and how
//! each command is laid out as bytes. Definitions are plain JSON (see `spec/definition-format.md`); this crate parses
//! and validates them and encodes commands to the exact bytes to write. It performs no I/O and runs no code from the
//! definition: the format is a small, total, deterministic template language.

mod encode;

use serde::Deserialize;
use std::collections::BTreeMap;
use std::fmt;

pub use encode::Encoded;

/// The schema identifier this crate understands.
pub const SCHEMA: &str = "lampbus.device/0";
/// Longest frame (after padding and checksum) a definition may produce.
pub const MAX_FRAME: usize = 64;

/// Everything that can go wrong parsing, validating or encoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The JSON could not be parsed into the expected shape.
    Parse(String),
    /// The definition parsed but breaks a rule of the format.
    Invalid(String),
    /// No command with that name.
    UnknownCommand(String),
    /// An argument was given that the command does not take.
    UnknownParam {
        /// Command being encoded.
        command: String,
        /// The unexpected argument name.
        param: String,
    },
    /// A required argument was not given.
    MissingParam {
        /// Command being encoded.
        command: String,
        /// The missing argument name.
        param: String,
    },
    /// An argument was NaN or infinite.
    BadValue {
        /// Command being encoded.
        command: String,
        /// The offending argument name.
        param: String,
    },
    /// The definition needs a feature this build does not include.
    Unsupported(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Parse(m) => write!(f, "parse error: {m}"),
            Error::Invalid(m) => write!(f, "invalid definition: {m}"),
            Error::UnknownCommand(c) => write!(f, "unknown command {c:?}"),
            Error::UnknownParam { command, param } => {
                write!(f, "command {command:?} has no parameter {param:?}")
            }
            Error::MissingParam { command, param } => {
                write!(f, "command {command:?} needs parameter {param:?}")
            }
            Error::BadValue { command, param } => write!(
                f,
                "parameter {param:?} of {command:?} is not a finite number"
            ),
            Error::Unsupported(m) => write!(f, "unsupported: {m}"),
        }
    }
}

impl std::error::Error for Error {}

fn invalid<T>(msg: impl Into<String>) -> Result<T, Error> {
    Err(Error::Invalid(msg.into()))
}

/// How much we trust a definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    /// Run against a real lamp by the maintainers.
    HardwareVerified,
    /// Frames cross-checked against independent open implementations, not run on hardware by us.
    SourceVerified,
    /// Neither of the above.
    #[default]
    Unverified,
}

/// How to recognise the lamp in a scan.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Match {
    /// Advertised local name starts with any of these.
    pub name_prefix: Vec<String>,
    /// Advertised service UUIDs (lowercase, 128-bit form).
    pub service_uuids: Vec<String>,
}

/// GATT write mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WriteMode {
    /// ATT write request (acknowledged).
    WithResponse,
    /// ATT write command (unacknowledged, faster).
    WithoutResponse,
}

/// Where commands are written.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gatt {
    /// Service UUID.
    pub service: String,
    /// Characteristic UUID that takes commands.
    pub write: String,
    /// Write mode.
    pub mode: WriteMode,
}

/// A built-in effect the lamp can run by itself.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Effect {
    /// Value to pass to the effect command.
    pub id: u8,
    /// Human-readable name.
    pub name: String,
    /// Other names hosts and users may use for it (for example `rainbow`).
    #[serde(default)]
    pub aliases: Vec<String>,
}

impl Effect {
    /// Case-insensitive match against the name or any alias.
    pub fn matches(&self, query: &str) -> bool {
        self.name.eq_ignore_ascii_case(query)
            || self.aliases.iter().any(|a| a.eq_ignore_ascii_case(query))
    }
}

/// A command parameter: its allowed range and optional default. Undeclared parameters used in a frame are
/// required and span 0..=255.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Param {
    /// Smallest accepted value (values below are clamped up).
    pub min: f64,
    /// Largest accepted value (values above are clamped down).
    pub max: f64,
    /// Value used when the caller gives none; without it the parameter is required.
    pub default: Option<f64>,
}

impl Default for Param {
    fn default() -> Self {
        Param {
            min: 0.0,
            max: 255.0,
            default: None,
        }
    }
}

/// Linear remap of a parameter into a byte.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scale {
    /// Parameter to read.
    pub param: String,
    /// Input range `[low, high]`.
    pub from: [f64; 2],
    /// Output range `[low, high]`, within 0..=255.
    pub to: [f64; 2],
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawElem {
    Num(u8),
    Text(String),
    Scale { scale: Scale },
}

/// One byte of a frame template.
#[derive(Debug, Clone, Deserialize)]
#[serde(try_from = "RawElem")]
pub enum Elem {
    /// A literal byte (`0..255`, or a `"0x.."` string).
    Byte(u8),
    /// A parameter, written `"$name"`.
    Param(String),
    /// A scaled parameter, written `{"scale": {...}}`.
    Scale(Scale),
}

impl TryFrom<RawElem> for Elem {
    type Error = String;

    fn try_from(raw: RawElem) -> Result<Self, String> {
        match raw {
            RawElem::Num(n) => Ok(Elem::Byte(n)),
            RawElem::Scale { scale } => Ok(Elem::Scale(scale)),
            RawElem::Text(s) => {
                if let Some(name) = s.strip_prefix('$') {
                    Ok(Elem::Param(name.to_string()))
                } else if let Some(hex) = s.strip_prefix("0x") {
                    u8::from_str_radix(hex, 16)
                        .map(Elem::Byte)
                        .map_err(|_| format!("bad hex byte {s:?}"))
                } else {
                    Err(format!(
                        "frame element {s:?} must be a number, \"0x..\", or \"$param\""
                    ))
                }
            }
        }
    }
}

/// Checksum appended after padding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChecksumKind {
    /// XOR of all bytes.
    Xor,
    /// Sum of all bytes modulo 256.
    Sum8,
}

/// Checksum configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checksum {
    /// Algorithm.
    pub kind: ChecksumKind,
}

/// A command: parameters, a frame template, optional zero padding and checksum.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Command {
    /// Declared parameters.
    #[serde(default)]
    pub params: BTreeMap<String, Param>,
    /// Frame template, one element per byte.
    pub frame: Vec<Elem>,
    /// Zero-pad the frame to this length before the checksum.
    #[serde(default)]
    pub pad_to: Option<usize>,
    /// Append a checksum byte.
    #[serde(default)]
    pub checksum: Option<Checksum>,
}

impl Command {
    /// Length of every frame this command produces (the template is fixed-size).
    pub fn frame_len(&self) -> usize {
        self.frame.len().max(self.pad_to.unwrap_or(0)) + usize::from(self.checksum.is_some())
    }
}

/// A transform applied to each frame just before it is written.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", deny_unknown_fields)]
pub enum WireOp {
    /// AES-128 in ECB mode over whole 16-byte blocks.
    #[serde(rename = "aes-128-ecb")]
    Aes128Ecb {
        /// 16-byte key as 32 hex characters.
        key: String,
    },
}

/// A complete lamp definition.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    /// Must equal [`SCHEMA`].
    pub schema: String,
    /// Short stable identifier, e.g. `elk-bledom`.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Verification level.
    #[serde(default)]
    pub status: Status,
    /// How to recognise the lamp.
    #[serde(rename = "match")]
    pub matching: Match,
    /// Where to write.
    pub gatt: Gatt,
    /// Maps standard roles (`on`, `off`, `color_rgb`, `brightness`, `effect`, ...) to command names.
    #[serde(default)]
    pub roles: BTreeMap<String, String>,
    /// Built-in effects.
    #[serde(default)]
    pub effects: Vec<Effect>,
    /// Commands by name.
    pub commands: BTreeMap<String, Command>,
    /// Frame transforms applied in order.
    #[serde(default)]
    pub wire: Vec<WireOp>,
    /// Where the protocol knowledge came from.
    #[serde(default)]
    pub sources: Vec<String>,
}

/// An inclusive numeric range of a command parameter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Range {
    /// Smallest accepted value.
    pub min: f64,
    /// Largest accepted value.
    pub max: f64,
}

/// How a lamp takes a colour.
#[derive(Debug, Clone, PartialEq)]
pub struct ColorCaps {
    /// Command taking `r`, `g`, `b`.
    pub command: String,
    /// Range of the command's inline `brightness` parameter, if it has one.
    pub brightness: Option<Range>,
}

/// A command with one numeric parameter (`level` for brightness, `value` for effect speed).
#[derive(Debug, Clone, PartialEq)]
pub struct ScalarCaps {
    /// The command.
    pub command: String,
    /// Range of its parameter.
    pub range: Range,
}

/// How a lamp runs its built-in effects.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectCaps {
    /// Command taking `mode`.
    pub command: String,
    /// Range of `mode`.
    pub mode: Range,
    /// Range of the inline `speed` parameter, if any.
    pub speed: Option<Range>,
    /// Range of the inline `brightness` parameter, if any.
    pub brightness: Option<Range>,
}

/// What a lamp can do, derived from its `roles`. See the "Roles" section of the format spec.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Caps {
    /// Command that powers the lamp on (no arguments).
    pub on: Option<String>,
    /// Command that powers the lamp off (no arguments).
    pub off: Option<String>,
    /// Colour control.
    pub color: Option<ColorCaps>,
    /// Separate brightness control (parameter `level`).
    pub brightness: Option<ScalarCaps>,
    /// Built-in effects.
    pub effect: Option<EffectCaps>,
    /// Separate effect-speed control (parameter `value`).
    pub effect_speed: Option<ScalarCaps>,
}

/// A conformance vector: the exact bytes a command must produce for given arguments.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Vector {
    /// Command name.
    pub command: String,
    /// Arguments.
    #[serde(default)]
    pub args: BTreeMap<String, f64>,
    /// Expected frame before wire transforms, hex.
    pub plain: String,
    /// Expected bytes on the wire, hex; omitted when identical to `plain`.
    #[serde(default)]
    pub wire: Option<String>,
}

/// A vector that did not reproduce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VectorFailure {
    /// Index in the vector list.
    pub index: usize,
    /// Command under test.
    pub command: String,
    /// What differed.
    pub reason: String,
}

/// Parse a vector file (a JSON array of [`Vector`]).
///
/// # Errors
/// Returns [`Error::Parse`] if the JSON is malformed.
pub fn parse_vectors(json: &str) -> Result<Vec<Vector>, Error> {
    serde_json::from_str(json).map_err(|e| Error::Parse(e.to_string()))
}

/// Lowercase hex of a byte slice.
pub fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

pub(crate) fn from_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 || !s.is_ascii() {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.char_indices().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                c == '-'
            } else {
                c.is_ascii_hexdigit() && !c.is_ascii_uppercase()
            }
        })
}

fn is_ident(s: &str, allow: char) -> bool {
    !s.is_empty()
        && s.len() <= 40
        && s.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == allow)
}

impl Definition {
    /// Parse and validate a definition from JSON.
    ///
    /// # Errors
    /// [`Error::Parse`] for malformed JSON or unknown fields, [`Error::Invalid`] when a rule of the format is broken.
    pub fn from_json(json: &str) -> Result<Definition, Error> {
        let def: Definition =
            serde_json::from_str(json).map_err(|e| Error::Parse(e.to_string()))?;
        def.validate()?;
        Ok(def)
    }

    /// Check every rule of the format that can be checked without a lamp.
    ///
    /// # Errors
    /// [`Error::Invalid`] naming the first rule broken.
    pub fn validate(&self) -> Result<(), Error> {
        if self.schema != SCHEMA {
            return invalid(format!("schema must be {SCHEMA:?}, got {:?}", self.schema));
        }
        if !is_ident(&self.id, '-') {
            return invalid(format!(
                "id {:?} must be lowercase letters, digits or '-' (max 40)",
                self.id
            ));
        }
        if self.matching.name_prefix.is_empty() && self.matching.service_uuids.is_empty() {
            return invalid("match needs at least one name_prefix or service_uuids entry");
        }
        for u in self
            .matching
            .service_uuids
            .iter()
            .chain([&self.gatt.service, &self.gatt.write])
        {
            if !is_uuid(u) {
                return invalid(format!("{u:?} is not a lowercase 128-bit UUID"));
            }
        }
        if self.commands.is_empty() {
            return invalid("at least one command is required");
        }
        for (name, cmd) in &self.commands {
            self.validate_command(name, cmd)?;
        }
        for (role, cmd) in &self.roles {
            let Some(c) = self.commands.get(cmd) else {
                return invalid(format!("role {role:?} points at unknown command {cmd:?}"));
            };
            let needs: &[&str] = match role.as_str() {
                "color_rgb" => &["r", "g", "b"],
                "brightness" => &["level"],
                "effect" => &["mode"],
                "effect_speed" => &["value"],
                _ => &[],
            };
            for p in needs {
                if !encode::uses_param(c, p) {
                    return invalid(format!(
                        "role {role:?}: command {cmd:?} must use parameter {p:?}"
                    ));
                }
            }
            if matches!(role.as_str(), "on" | "off") {
                let all_defaulted = c.params.values().all(|p| p.default.is_some());
                let undeclared = c
                    .frame
                    .iter()
                    .any(|e| matches!(e, Elem::Param(n) if !c.params.contains_key(n)));
                if !all_defaulted || undeclared {
                    return invalid(format!("role {role:?}: command {cmd:?} must work with no arguments (give every parameter a default)"));
                }
            }
        }
        let mut ids: Vec<u8> = self.effects.iter().map(|e| e.id).collect();
        ids.sort_unstable();
        if ids.windows(2).any(|w| w.first() == w.get(1)) {
            return invalid("effect ids must be unique");
        }
        for op in &self.wire {
            let WireOp::Aes128Ecb { key } = op;
            if from_hex(key).map(|k| k.len()) != Some(16) {
                return invalid("aes-128-ecb key must be 32 hex characters");
            }
            for (name, cmd) in &self.commands {
                if cmd.frame_len() % 16 != 0 {
                    return invalid(format!("command {name:?} produces {} bytes; aes-128-ecb needs a multiple of 16 (use pad_to)", cmd.frame_len()));
                }
            }
        }
        Ok(())
    }

    fn validate_command(&self, name: &str, cmd: &Command) -> Result<(), Error> {
        if !is_ident(name, '_') {
            return invalid(format!(
                "command name {name:?} must be lowercase letters, digits or '_'"
            ));
        }
        if cmd.frame.is_empty() {
            return invalid(format!("command {name:?} has an empty frame"));
        }
        for (p, spec) in &cmd.params {
            let ok = spec.min.is_finite()
                && spec.max.is_finite()
                && 0.0 <= spec.min
                && spec.min <= spec.max
                && spec.max <= 255.0;
            if !ok {
                return invalid(format!(
                    "parameter {p:?} of {name:?} needs 0 <= min <= max <= 255"
                ));
            }
            if let Some(d) = spec.default {
                if !(spec.min..=spec.max).contains(&d) {
                    return invalid(format!("default of {p:?} in {name:?} is outside min..max"));
                }
            }
        }
        for el in &cmd.frame {
            match el {
                Elem::Byte(_) => {}
                Elem::Param(p) => {
                    if !is_ident(p, '_') {
                        return invalid(format!(
                            "parameter name {p:?} in {name:?} is not a valid identifier"
                        ));
                    }
                }
                Elem::Scale(s) => {
                    let finite = s.from.iter().chain(&s.to).all(|v| v.is_finite());
                    if !finite
                        || s.from[0] == s.from[1]
                        || s.to.iter().any(|v| !(0.0..=255.0).contains(v))
                    {
                        return invalid(format!("scale of {:?} in {name:?} needs a non-empty finite input range and an output within 0..=255", s.param));
                    }
                }
            }
        }
        if let Some(p) = cmd.pad_to {
            if p < cmd.frame.len() {
                return invalid(format!(
                    "pad_to {p} of {name:?} is shorter than its {}-byte frame",
                    cmd.frame.len()
                ));
            }
        }
        if cmd.frame_len() > MAX_FRAME {
            return invalid(format!(
                "command {name:?} produces {} bytes, over the {MAX_FRAME}-byte limit",
                cmd.frame_len()
            ));
        }
        Ok(())
    }

    /// The command that implements a standard role, if the definition declares one.
    pub fn role(&self, role: &str) -> Option<&str> {
        self.roles.get(role).map(String::as_str)
    }

    fn range_of(&self, command: &str, param: &str) -> Option<Range> {
        let c = self.commands.get(command)?;
        if !encode::uses_param(c, param) {
            return None;
        }
        let p = c.params.get(param).cloned().unwrap_or_default();
        Some(Range {
            min: p.min,
            max: p.max,
        })
    }

    /// Summarise what this lamp can do. Call on a validated definition.
    pub fn caps(&self) -> Caps {
        let role = |r: &str| self.role(r).map(str::to_string);
        let scalar = |r: &str, param: &str| {
            let command = self.role(r)?;
            Some(ScalarCaps {
                command: command.to_string(),
                range: self.range_of(command, param)?,
            })
        };
        Caps {
            on: role("on"),
            off: role("off"),
            color: self.role("color_rgb").map(|c| ColorCaps {
                command: c.to_string(),
                brightness: self.range_of(c, "brightness"),
            }),
            brightness: scalar("brightness", "level"),
            effect: self.role("effect").and_then(|c| {
                Some(EffectCaps {
                    command: c.to_string(),
                    mode: self.range_of(c, "mode")?,
                    speed: self.range_of(c, "speed"),
                    brightness: self.range_of(c, "brightness"),
                })
            }),
            effect_speed: scalar("effect_speed", "value"),
        }
    }

    /// Run conformance vectors and report every mismatch (empty means all passed).
    pub fn verify_vectors(&self, vectors: &[Vector]) -> Vec<VectorFailure> {
        let mut failures = Vec::new();
        for (index, v) in vectors.iter().enumerate() {
            let mut fail = |reason: String| {
                failures.push(VectorFailure {
                    index,
                    command: v.command.clone(),
                    reason,
                })
            };
            let args: Vec<(&str, f64)> = v.args.iter().map(|(k, x)| (k.as_str(), *x)).collect();
            match self.encode(&v.command, &args) {
                Err(e) => fail(format!("encode failed: {e}")),
                Ok(out) => {
                    if to_hex(&out.plain) != v.plain {
                        fail(format!(
                            "plain {} != expected {}",
                            to_hex(&out.plain),
                            v.plain
                        ));
                    }
                    let want_wire = v.wire.as_deref().unwrap_or(&v.plain);
                    if to_hex(&out.wire) != want_wire {
                        fail(format!(
                            "wire {} != expected {want_wire}",
                            to_hex(&out.wire)
                        ));
                    }
                }
            }
        }
        failures
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = r#"{
        "schema": "lampbus.device/0", "id": "t", "name": "T",
        "match": {"name_prefix": ["T-"]},
        "gatt": {"service": "0000fff0-0000-1000-8000-00805f9b34fb", "write": "0000fff3-0000-1000-8000-00805f9b34fb", "mode": "without-response"},
        "commands": {"c": {"params": {"v": {"min": 0, "max": 100}}, "frame": [1, "$v", {"scale": {"param": "v", "from": [0, 100], "to": [0, 255]}}, "0xff"]}}
    }"#;

    fn with(edit: impl Fn(&mut serde_json::Value)) -> Result<Definition, Error> {
        let mut v: serde_json::Value = serde_json::from_str(BASE).unwrap();
        edit(&mut v);
        Definition::from_json(&v.to_string())
    }

    #[test]
    fn minimal_definition_encodes_with_clamp_scale_and_rounding() {
        let d = Definition::from_json(BASE).unwrap();
        assert_eq!(
            d.encode("c", &[("v", 50.0)]).unwrap().plain,
            vec![1, 50, 128, 0xff]
        ); // 127.5 rounds up
        assert_eq!(
            d.encode("c", &[("v", 500.0)]).unwrap().plain,
            vec![1, 100, 255, 0xff]
        ); // clamped
        assert_eq!(
            d.encode("c", &[("v", -5.0)]).unwrap().plain,
            vec![1, 0, 0, 0xff]
        );
    }

    #[test]
    fn encode_errors_are_specific() {
        let d = Definition::from_json(BASE).unwrap();
        assert!(matches!(
            d.encode("nope", &[]),
            Err(Error::UnknownCommand(_))
        ));
        assert!(matches!(
            d.encode("c", &[]),
            Err(Error::MissingParam { .. })
        ));
        assert!(matches!(
            d.encode("c", &[("v", 1.0), ("typo", 2.0)]),
            Err(Error::UnknownParam { .. })
        ));
        assert!(matches!(
            d.encode("c", &[("v", f64::NAN)]),
            Err(Error::BadValue { .. })
        ));
        assert!(matches!(
            d.encode("c", &[("v", f64::INFINITY)]),
            Err(Error::BadValue { .. })
        ));
    }

    #[test]
    fn rejects_malformed_definitions() {
        assert!(matches!(
            with(|v| v["schema"] = "x".into()),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            with(|v| v["id"] = "Bad Id".into()),
            Err(Error::Invalid(_))
        ));
        assert!(
            matches!(with(|v| v["extra"] = 1.into()), Err(Error::Parse(_))),
            "unknown fields are errors"
        );
        assert!(matches!(
            with(|v| v["match"] = serde_json::json!({})),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            with(|v| v["gatt"]["write"] = "NOT-A-UUID".into()),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            with(|v| v["commands"] = serde_json::json!({})),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            with(|v| v["roles"] = serde_json::json!({"on": "ghost"})),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            with(|v| v["commands"]["c"]["frame"] = serde_json::json!([256])),
            Err(Error::Parse(_))
        ));
        assert!(matches!(
            with(|v| v["commands"]["c"]["frame"] = serde_json::json!(["bogus"])),
            Err(Error::Parse(_))
        ));
        assert!(
            matches!(
                with(|v| v["commands"]["c"]["pad_to"] = 2.into()),
                Err(Error::Invalid(_))
            ),
            "pad shorter than frame"
        );
        assert!(
            matches!(
                with(|v| v["commands"]["c"]["pad_to"] = 65.into()),
                Err(Error::Invalid(_))
            ),
            "over the frame limit"
        );
        assert!(matches!(
            with(|v| v["commands"]["c"]["params"]["v"]["max"] = 300.into()),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            with(|v| v["commands"]["c"]["params"]["v"]["default"] = 500.into()),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            with(|v| v["effects"] =
                serde_json::json!([{"id": 1, "name": "a"}, {"id": 1, "name": "b"}])),
            Err(Error::Invalid(_))
        ));
    }

    fn set_aes(v: &mut serde_json::Value, key: &str) {
        v["wire"] = serde_json::json!([{"op": "aes-128-ecb", "key": key}]);
    }

    #[test]
    fn aes_rules_are_enforced() {
        let good = "00112233445566778899aabbccddeeff";
        // frame is 4 bytes: not a multiple of 16 without padding
        assert!(matches!(with(|v| set_aes(v, good)), Err(Error::Invalid(_))));
        assert!(with(|v| {
            set_aes(v, good);
            v["commands"]["c"]["pad_to"] = 16.into();
        })
        .is_ok());
        assert!(matches!(
            with(|v| {
                set_aes(v, "abcd");
                v["commands"]["c"]["pad_to"] = 16.into();
            }),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn role_conventions_are_enforced() {
        let with_role = |role: &'static str| {
            move |v: &mut serde_json::Value| v["roles"] = serde_json::json!({ role: "c" })
        };
        // BASE command "c" has only parameter "v": it is not a colour, brightness, effect or speed command
        assert!(matches!(
            with(with_role("color_rgb")),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            with(with_role("brightness")),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(with(with_role("effect")), Err(Error::Invalid(_))));
        assert!(matches!(
            with(with_role("effect_speed")),
            Err(Error::Invalid(_))
        ));
        // "on"/"off" must work with no arguments; v has no default
        assert!(matches!(with(with_role("on")), Err(Error::Invalid(_))));
        assert!(with(|v| {
            v["roles"] = serde_json::json!({"on": "c"});
            v["commands"]["c"]["params"]["v"]["default"] = 7.into();
        })
        .is_ok());
        // unknown roles are allowed and ignored
        assert!(with(|v| v["roles"] = serde_json::json!({"future_role": "c"})).is_ok());
    }

    #[test]
    fn checksums_and_padding() {
        let d = with(|v| {
            v["commands"]["c"]["pad_to"] = 6.into();
            v["commands"]["c"]["checksum"] = serde_json::json!({"kind": "sum8"});
        })
        .unwrap();
        // [1, 10, 26 (10 of 100 -> 25.5 rounds to 26), 255, 0, 0] sum = 292 -> 36
        let out = d.encode("c", &[("v", 10.0)]).unwrap().plain;
        assert_eq!(out, vec![1, 10, 26, 255, 0, 0, 36]);
    }
}
