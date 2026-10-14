//! Encode one command from a definition file.
//!
//! ```text
//! cargo run -p lampbus-defs --example encode -- definitions/elk-bledom.json color r=255 g=0 b=128
//! ```

use lampbus_defs::{to_hex, Definition};
use std::process::ExitCode;

fn run() -> Result<(), String> {
    let mut argv = std::env::args().skip(1);
    let path = argv.next().ok_or("usage: encode <definition.json> <command> [name=value ...]")?;
    let command = argv.next().ok_or("missing command name")?;
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
    let def = Definition::from_json(&text).map_err(|e| e.to_string())?;
    let mut owned: Vec<(String, f64)> = Vec::new();
    for a in argv {
        let (k, v) = a.split_once('=').ok_or_else(|| format!("argument {a:?} must be name=value"))?;
        owned.push((k.to_string(), v.parse().map_err(|_| format!("{v:?} is not a number"))?));
    }
    let args: Vec<(&str, f64)> = owned.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    let out = def.encode(&command, &args).map_err(|e| e.to_string())?;
    println!("plain {}", to_hex(&out.plain));
    println!("wire  {}", to_hex(&out.wire));
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
