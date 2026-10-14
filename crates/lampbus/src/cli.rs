//! Command-line parsing (hand-rolled to keep the binary small) and output formatting.

use serde_json::{json, Map, Value};
use std::path::PathBuf;

pub const HELP: &str = "lampbus: control Bluetooth lamps

USAGE
  lampbus [options] <command> [args]

COMMANDS
  status                         what every lamp is doing
  on | off | toggle
  color <color> [brightness]     e.g.  lampbus color warm 40    lampbus color '#ff8800'    lampbus color 255,0,0
  hue <0-360> [saturation]
  brightness <0-100>
  effect <name|id> [speed]       built-in effect, e.g.  lampbus effect rainbow
  anim <name> [brightness]       play an animation (needs `lampbus serve`, or --hold)
  stop                           stop the animation
  preset <name>                  apply a preset
  library                        list animations and presets
  save-animation <name> <file>   validate and save an animation spec
  raw <command> [name=value...]  send a protocol command from the lamp's definition
  scan                           find lamps nearby
  definitions                    lamp families this build understands
  serve                          run the HTTP API (keeps the Bluetooth link, runs animations)
  mcp                            run an MCP server on stdio (for agents; no daemon needed)

OPTIONS
  -d, --device <name>            address one lamp (alias or id); default: all
      --config <file>            config file (default ~/.config/lampbus/config.json)
      --remote <host:port>       talk to a running `lampbus serve` (also LAMPBUS_URL)
      --hold                     with `anim`: stay running so the animation keeps playing
      --json                     machine-readable output
  -h, --help | --version

With no config file lampbus uses the first lamp it finds. If `lampbus serve` is running on this machine,
commands are sent to it automatically; otherwise they talk to Bluetooth directly.";

#[derive(Default)]
pub struct Global {
    pub config: Option<PathBuf>,
    pub device: Option<String>,
    pub remote: Option<String>,
    pub json: bool,
    pub hold: bool,
}

pub enum Cmd {
    Control(Value),
    Status,
    Library,
    SaveAnimation {
        name: String,
        file: PathBuf,
    },
    Raw {
        command: String,
        args: Map<String, Value>,
    },
    Scan,
    Definitions,
    Serve,
    Mcp,
    Help,
    Version,
}

fn number(s: &str, what: &str) -> Result<Value, String> {
    s.parse::<f64>()
        .map(|n| json!(n))
        .map_err(|_| format!("{what} must be a number, not {s:?}"))
}

fn color_arg(s: &str) -> Value {
    let parts: Vec<&str> = s.split(',').collect();
    if parts.len() == 3 {
        let nums: Vec<f64> = parts.iter().filter_map(|p| p.trim().parse().ok()).collect();
        if nums.len() == 3 {
            return json!(nums);
        }
    }
    json!(s)
}

pub fn parse(argv: Vec<String>) -> Result<(Global, Cmd), String> {
    let mut g = Global::default();
    let mut rest: Vec<String> = Vec::new();
    let mut it = argv.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => return Ok((g, Cmd::Help)),
            "--version" => return Ok((g, Cmd::Version)),
            "--json" => g.json = true,
            "--hold" => g.hold = true,
            "-d" | "--device" => g.device = Some(it.next().ok_or("--device needs a name")?),
            "--config" => g.config = Some(PathBuf::from(it.next().ok_or("--config needs a path")?)),
            "--remote" => g.remote = Some(it.next().ok_or("--remote needs host:port")?),
            s if s.starts_with('-') && s.len() > 1 && s.parse::<f64>().is_err() => {
                return Err(format!("unknown option {s}; try --help"))
            }
            _ => rest.push(a),
        }
    }
    let mut args = rest.into_iter();
    let Some(cmd) = args.next() else {
        return Ok((g, Cmd::Help));
    };
    let a: Vec<String> = args.collect();
    // checked access: a missing argument is a usage error, never a panic
    let arg = |i: usize, usage: &str| {
        a.get(i)
            .map(String::as_str)
            .ok_or_else(|| format!("usage: lampbus {usage}"))
    };
    let opt = |i: usize| a.get(i).map(String::as_str);
    let mut ctl = Map::new();
    let parsed = match cmd.as_str() {
        "status" => Cmd::Status,
        "library" => Cmd::Library,
        "scan" => Cmd::Scan,
        "definitions" => Cmd::Definitions,
        "serve" => Cmd::Serve,
        "mcp" => Cmd::Mcp,
        "help" => Cmd::Help,
        "on" | "off" | "toggle" => {
            ctl.insert(
                "power".into(),
                if cmd == "toggle" {
                    json!("toggle")
                } else {
                    json!(cmd == "on")
                },
            );
            Cmd::Control(Value::Object(ctl))
        }
        "color" | "colour" => {
            ctl.insert(
                "color".into(),
                color_arg(arg(0, "color <color> [brightness]")?),
            );
            if let Some(b) = opt(1) {
                ctl.insert("brightness".into(), number(b, "brightness")?);
            }
            Cmd::Control(Value::Object(ctl))
        }
        "hue" => {
            ctl.insert(
                "hue".into(),
                number(arg(0, "hue <0-360> [saturation]")?, "hue")?,
            );
            if let Some(s) = opt(1) {
                ctl.insert("saturation".into(), number(s, "saturation")?);
            }
            Cmd::Control(Value::Object(ctl))
        }
        "brightness" => {
            ctl.insert(
                "brightness".into(),
                number(arg(0, "brightness <0-100>")?, "brightness")?,
            );
            Cmd::Control(Value::Object(ctl))
        }
        "effect" => {
            let e = arg(0, "effect <name|id> [speed]")?;
            ctl.insert(
                "effect".into(),
                e.parse::<u64>().map_or_else(|_| json!(e), |n| json!(n)),
            );
            if let Some(s) = opt(1) {
                ctl.insert("speed".into(), number(s, "speed")?);
            }
            Cmd::Control(Value::Object(ctl))
        }
        "anim" | "animation" => {
            ctl.insert(
                "animation".into(),
                json!(arg(0, "anim <name> [brightness]")?),
            );
            if let Some(b) = opt(1) {
                ctl.insert("brightness".into(), number(b, "brightness")?);
            }
            Cmd::Control(Value::Object(ctl))
        }
        "stop" => {
            ctl.insert("stop_animation".into(), json!(true));
            Cmd::Control(Value::Object(ctl))
        }
        "preset" | "apply" => {
            ctl.insert("preset".into(), json!(arg(0, "preset <name>")?));
            Cmd::Control(Value::Object(ctl))
        }
        "save-animation" => {
            let usage = "save-animation <name> <file.json>";
            Cmd::SaveAnimation {
                name: arg(0, usage)?.to_string(),
                file: PathBuf::from(arg(1, usage)?),
            }
        }
        "raw" => {
            let command = arg(0, "raw <command> [name=value...]")?.to_string();
            let mut args = Map::new();
            for kv in a.iter().skip(1) {
                let (k, v) = kv
                    .split_once('=')
                    .ok_or_else(|| format!("argument {kv:?} must be name=value"))?;
                args.insert(k.to_string(), number(v, k)?);
            }
            Cmd::Raw { command, args }
        }
        other => return Err(format!("unknown command {other:?}; try `lampbus --help`")),
    };
    if let (Cmd::Control(Value::Object(m)), Some(d)) = (&parsed, &g.device) {
        let mut m = m.clone();
        m.insert("device".into(), json!(d));
        return Ok((g, Cmd::Control(Value::Object(m))));
    }
    Ok((g, parsed))
}

/// One readable line per lamp.
pub fn render(snapshot: &Value) -> String {
    let lamps = snapshot
        .get("lamps")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if lamps.is_empty() {
        return "no lamps found".into();
    }
    lamps
        .iter()
        .map(|l| {
            let s = |k: &str| l.get(k).and_then(Value::as_str).unwrap_or("-").to_string();
            let on = l.get("on").and_then(Value::as_bool).unwrap_or(false);
            let rgb: Vec<u64> = l
                .get("rgb")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_u64).collect())
                .unwrap_or_default();
            let mut line = format!(
                "{} [{}] {}",
                s("alias"),
                s("definition"),
                if on {
                    format!(
                        "on  {}%  rgb({})  {}",
                        l.get("brightness").and_then(Value::as_u64).unwrap_or(0),
                        rgb.iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(","),
                        s("source")
                    )
                } else {
                    "off".into()
                }
            );
            line.push_str(&format!("  link={}", s("link")));
            if let Some(a) = l.get("animation").and_then(Value::as_str) {
                line.push_str(&format!("  animation={a}"));
            }
            if let Some(e) = l.get("last_error").and_then(Value::as_str) {
                line.push_str(&format!("  ERROR: {e}"));
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<(Global, Cmd), String> {
        parse(s.split_whitespace().map(String::from).collect())
    }

    fn ctl(s: &str) -> Value {
        match p(s).unwrap().1 {
            Cmd::Control(v) => v,
            _ => panic!("not a control command: {s}"),
        }
    }

    #[test]
    fn commands_become_control_objects() {
        assert_eq!(ctl("on"), json!({"power": true}));
        assert_eq!(ctl("off"), json!({"power": false}));
        assert_eq!(ctl("toggle"), json!({"power": "toggle"}));
        assert_eq!(
            ctl("color warm 40"),
            json!({"color": "warm", "brightness": 40.0})
        );
        assert_eq!(
            ctl("color 255,0,128"),
            json!({"color": [255.0, 0.0, 128.0]})
        );
        assert_eq!(ctl("color #ff8800"), json!({"color": "#ff8800"}));
        assert_eq!(
            ctl("effect rainbow 30"),
            json!({"effect": "rainbow", "speed": 30.0})
        );
        assert_eq!(ctl("effect 5"), json!({"effect": 5}));
        assert_eq!(
            ctl("anim candle 50"),
            json!({"animation": "candle", "brightness": 50.0})
        );
        assert_eq!(
            ctl("-d desk brightness 10"),
            json!({"brightness": 10.0, "device": "desk"})
        );
        assert_eq!(ctl("preset reading"), json!({"preset": "reading"}));
        assert_eq!(ctl("hue 120"), json!({"hue": 120.0}));
    }

    #[test]
    fn mistakes_are_explained() {
        for bad in [
            "color",
            "color red x",
            "brightness",
            "frobnicate",
            "--wat on",
            "raw",
            "raw x y",
            "-d",
        ] {
            assert!(p(bad).is_err(), "{bad}");
        }
        assert!(matches!(p("").unwrap().1, Cmd::Help));
        assert!(matches!(p("--help").unwrap().1, Cmd::Help));
    }

    #[test]
    fn raw_arguments_are_numbers() {
        let Cmd::Raw { command, args } = p("raw color r=1 g=2 b=3").unwrap().1 else {
            panic!()
        };
        assert_eq!((command.as_str(), args.len()), ("color", 3));
    }

    #[test]
    fn rendering_is_readable() {
        let s = json!({"lamps": [{"alias": "desk", "definition": "istrip-ssl", "on": true, "brightness": 40, "rgb": [1, 2, 3], "source": "color", "link": "ready"}]});
        assert_eq!(
            render(&s),
            "desk [istrip-ssl] on  40%  rgb(1,2,3)  color  link=ready"
        );
        assert_eq!(render(&json!({"lamps": []})), "no lamps found");
    }
}
