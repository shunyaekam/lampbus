//! MCP server over stdio (newline-delimited JSON-RPC 2.0), so an agent can drive the lamps with no daemon: the
//! agent's host starts `lampbus mcp`, and the process talks to Bluetooth directly for as long as the session lives.

use crate::control::{control, Ctx};
use crate::library::Kind;
use lampbus_render::anim;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const COLORS_HELP: &str = "A colour is a name (red, orange, amber, yellow, green, cyan, blue, purple, magenta, pink, white, warm, candle, cool), \"#rrggbb\", [r,g,b] (0-255) or {\"h\":0-360,\"s\":0-100,\"v\":0-100}.";

fn tools() -> Value {
    json!([
        {
            "name": "lamp_status",
            "description": "Current state of every lamp: power, colour, brightness, active effect or animation, connection, and what it supports (colours, brightness, built-in effects).",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "lamp_control",
            "description": format!("Change one or more lamps. Give ONE of color/hue, effect, animation or preset, plus optional brightness/speed/power. {COLORS_HELP} Brightness and speed are 0-100. Animations are library names (see lamp_library) or inline specs (see lamp_validate_animation) and keep running while this session is open. power may be true, false or \"toggle\"; it is applied last. Omit device to address every lamp."),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "device": {"type": "string", "description": "Lamp alias or id; omit for all lamps"},
                    "preset": {"type": "string", "description": "Name from lamp_library"},
                    "color": {"description": "Colour as name, hex, [r,g,b] or {h,s,v}", "oneOf": [{"type": "string"}, {"type": "array", "items": {"type": "number"}, "minItems": 3, "maxItems": 3}, {"type": "object"}]},
                    "hue": {"type": "number", "description": "0-360 (alternative to color)"},
                    "saturation": {"type": "number", "description": "0-100, used with hue"},
                    "effect": {"description": "Built-in effect name, alias (e.g. rainbow) or id", "oneOf": [{"type": "string"}, {"type": "number"}]},
                    "animation": {"description": "Library animation name or an inline animation spec object", "oneOf": [{"type": "string"}, {"type": "object"}]},
                    "brightness": {"type": "number", "description": "0-100"},
                    "speed": {"type": "number", "description": "0-100, for effects"},
                    "stop_animation": {"type": "boolean"},
                    "power": {"description": "true, false or \"toggle\"", "oneOf": [{"type": "boolean"}, {"type": "string", "enum": ["toggle"]}]}
                }
            }
        },
        {
            "name": "lamp_library",
            "description": "The available animations and presets with their descriptions, and the lamp families and built-in effects known to this installation.",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "lamp_validate_animation",
            "description": "Check an animation spec without playing it. Format: {\"type\":\"keyframes\",\"duration\":seconds,\"loop\":bool,\"space\":\"linear\"|\"srgb\"|\"hsv\",\"then\":\"hold\"|\"off\",\"fps\":0.1-40,\"keys\":[{\"t\":seconds,\"color\":..,\"level\":0-100,\"ease\":\"linear\"|\"smooth\"|\"hold\"}]} or {\"type\":\"gen\",\"gen\":\"rainbow\"|\"breathe\"|\"candle\"|\"aurora\",\"params\":{...}}. Returns what it will do, or why it is invalid.",
            "inputSchema": {"type": "object", "properties": {"spec": {"type": "object"}}, "required": ["spec"]}
        },
        {
            "name": "lamp_save_animation",
            "description": "Validate and save an animation to the user's library so it can be played by name.",
            "inputSchema": {"type": "object", "properties": {"name": {"type": "string", "description": "lowercase letters, digits, - or _"}, "spec": {"type": "object"}}, "required": ["name", "spec"]}
        }
    ])
}

fn describe(snapshot: &Value) -> String {
    let lamps = snapshot
        .get("lamps")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if lamps.is_empty() {
        return "no lamps known".into();
    }
    lamps
        .iter()
        .map(|l| {
            let s = |k: &str| l.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            let on = l.get("on").and_then(Value::as_bool).unwrap_or(false);
            let rgb = l.get("rgb").map_or_else(String::new, ToString::to_string);
            format!(
                "{} ({}): {} source={} rgb={rgb} brightness={}% link={}{}",
                s("alias"),
                s("definition"),
                if on { "on" } else { "off" },
                s("source"),
                l.get("brightness").and_then(Value::as_u64).unwrap_or(0),
                s("link"),
                l.get("last_error")
                    .and_then(Value::as_str)
                    .map_or_else(String::new, |e| format!(" error={e}")),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn call(ctx: &Ctx, definitions: &Value, name: &str, args: &Value) -> Result<String, String> {
    match name {
        "lamp_status" => {
            ctx.ensure_lamps(Duration::from_secs(10)).await;
            let snap = serde_json::to_value(ctx.handle.snapshot()).map_err(|e| e.to_string())?;
            Ok(format!("{}\n\n{snap}", describe(&snap)))
        }
        "lamp_control" => {
            control(ctx, args).await?;
            let aliases: Vec<String> = args
                .get("device")
                .and_then(Value::as_str)
                .map(|d| vec![d.to_string()])
                .unwrap_or_default();
            let settled = ctx.handle.settled(&aliases, Duration::from_secs(25)).await;
            let snap = serde_json::to_value(ctx.handle.snapshot()).map_err(|e| e.to_string())?;
            match settled {
                Ok(_) => Ok(format!("done\n{}", describe(&snap))),
                Err(why) => Err(format!(
                    "the command was accepted but the lamp did not confirm in time: {why}"
                )),
            }
        }
        "lamp_library" => {
            let describe_all = |kind: Kind| -> Value {
                ctx.library
                    .all(kind)
                    .into_iter()
                    .map(|(n, v)| (n, v.get("description").cloned().unwrap_or(Value::Null)))
                    .collect::<serde_json::Map<_, _>>()
                    .into()
            };
            let snap = serde_json::to_value(ctx.handle.snapshot()).map_err(|e| e.to_string())?;
            Ok(json!({"animations": describe_all(Kind::Animations), "presets": describe_all(Kind::Presets),
                      "lamps": snap.get("lamps").cloned().unwrap_or(Value::Null), "families": definitions}).to_string())
        }
        "lamp_validate_animation" => {
            let spec = args.get("spec").ok_or("missing spec")?;
            let a = anim::load(spec).map_err(|e| e.to_string())?;
            Ok(format!(
                "valid: {} fps, {}, {}",
                a.fps,
                a.duration
                    .map_or_else(|| "runs forever".to_string(), |d| format!("{d} s")),
                if a.looping {
                    "loops"
                } else if a.then_off {
                    "ends by switching the lamp off"
                } else {
                    "holds its last frame"
                }
            ))
        }
        "lamp_save_animation" => {
            let n = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or("missing name")?;
            let spec = args.get("spec").cloned().ok_or("missing spec")?;
            ctx.library.save(Kind::Animations, n, spec)?;
            Ok(format!(
                "saved animation {n:?}; play it with lamp_control {{\"animation\": \"{n}\"}}"
            ))
        }
        other => Err(format!("unknown tool {other:?}")),
    }
}

async fn handle(ctx: &Ctx, definitions: &Value, req: &Value) -> Option<Value> {
    let id = req.get("id").cloned();
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => json!({
            "protocolVersion": params.get("protocolVersion").and_then(Value::as_str).unwrap_or("2024-11-05"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "lampbus", "version": env!("CARGO_PKG_VERSION")},
            "instructions": "Control Bluetooth lamps. Start with lamp_status, then lamp_control."
        }),
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools()}),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let (text, is_error) = match call(ctx, definitions, name, &args).await {
                Ok(t) => (t, false),
                Err(e) => (e, true),
            };
            json!({"content": [{"type": "text", "text": text}], "isError": is_error})
        }
        _ if id.is_none() => return None, // notifications need no reply
        _ => {
            return Some(
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("method not found: {method}")}}),
            )
        }
    };
    id.map(|id| json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

pub async fn run(ctx: Ctx, definitions: Value) -> Result<(), String> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut out = tokio::io::stdout();
    while let Some(line) = lines.next_line().await.map_err(|e| e.to_string())? {
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(req) => handle(&ctx, &definitions, &req).await,
            Err(e) => Some(
                json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("parse error: {e}")}}),
            ),
        };
        if let Some(r) = reply {
            out.write_all(format!("{r}\n").as_bytes())
                .await
                .map_err(|e| e.to_string())?;
            out.flush().await.map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}
