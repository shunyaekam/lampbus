//! `lampbus`: control Bluetooth lamps from the command line, an HTTP API, or an MCP server (for agents).

mod assets;
mod cli;
mod config;
mod control;
#[cfg(feature = "http")]
mod http;
mod library;
#[cfg(feature = "mcp")]
mod mcp;
mod remote;

use cli::{Cmd, Global};
use config::Settings;
use control::Ctx;
use library::{Kind, Library};
use remote::Remote;
use serde_json::{json, Value};
use std::process::ExitCode;
use std::time::Duration;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("lampbus: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Build the in-process controller: definitions, core, Bluetooth shell and library.
fn direct(settings: &Settings) -> Result<(Ctx, Vec<lampbus_defs::Definition>), String> {
    let defs = config::definitions(settings.definitions_dir.as_deref())?;
    let core =
        lampbus_core::Core::new(defs.clone(), settings.core.clone()).map_err(|e| e.to_string())?;
    let handle = lampbus_shell::start(core);
    let ctx = Ctx {
        handle,
        library: Library::new(settings.library_dir.clone()),
        auto_discover: settings.core.auto_adopt,
    };
    Ok((ctx, defs))
}

/// A running daemon, if one was asked for or is listening on the configured address.
async fn find_remote(g: &Global, settings: &Settings) -> Option<Remote> {
    let addr = g.remote.clone().or_else(|| {
        std::env::var("LAMPBUS_URL").ok().map(|u| {
            u.trim_start_matches("http://")
                .trim_end_matches('/')
                .to_string()
        })
    });
    let forced = addr.is_some();
    let addr = addr.unwrap_or_else(|| settings.bind.clone());
    if !forced && !Remote::alive(&addr).await {
        return None;
    }
    let token = config::token(&settings.config_dir).ok()?;
    Some(Remote { addr, token })
}

fn show(snapshot: &Value, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(snapshot).unwrap_or_default()
        );
    } else {
        println!("{}", cli::render(snapshot));
    }
}

async fn run() -> Result<(), String> {
    let (g, cmd) = cli::parse(std::env::args().skip(1).collect())?;
    match cmd {
        Cmd::Help => {
            println!("{}", cli::HELP);
            return Ok(());
        }
        Cmd::Version => {
            println!("lampbus {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        _ => {}
    }
    let settings = config::load(g.config.as_deref())?;
    match cmd {
        Cmd::Definitions => {
            let defs = config::definitions(settings.definitions_dir.as_deref())?;
            println!(
                "{}",
                serde_json::to_string_pretty(&config::summarise(&defs))
                    .map_err(|e| e.to_string())?
            );
            Ok(())
        }
        Cmd::Serve => serve(&settings).await,
        Cmd::Mcp => mcp(&settings).await,
        other => client(&g, &settings, other).await,
    }
}

#[cfg(feature = "http")]
async fn serve(settings: &Settings) -> Result<(), String> {
    let (ctx, defs) = direct(settings)?;
    let token = config::token(&settings.config_dir)?;
    eprintln!(
        "lampbus: API on http://{}  (token in {})",
        settings.bind,
        settings.config_dir.join("token").display()
    );
    if settings.core.auto_adopt {
        ctx.handle.discover().await;
    }
    http::serve(ctx, config::summarise(&defs), &settings.bind, token)
        .await
        .map_err(|e| format!("cannot serve on {}: {e}", settings.bind))
}

#[cfg(not(feature = "http"))]
async fn serve(_: &Settings) -> Result<(), String> {
    Err("this build has no HTTP support (rebuild with --features http)".into())
}

#[cfg(feature = "mcp")]
async fn mcp(settings: &Settings) -> Result<(), String> {
    let (ctx, defs) = direct(settings)?;
    mcp::run(ctx, config::summarise(&defs)).await
}

#[cfg(not(feature = "mcp"))]
async fn mcp(_: &Settings) -> Result<(), String> {
    Err("this build has no MCP support (rebuild with --features mcp)".into())
}

/// Everything else: talk to a running daemon if there is one, else straight to Bluetooth.
async fn client(g: &Global, settings: &Settings, cmd: Cmd) -> Result<(), String> {
    if let Some(r) = find_remote(g, settings).await {
        return via_remote(g, &r, cmd).await;
    }
    let (ctx, _) = direct(settings)?;
    match cmd {
        Cmd::Status => {
            ctx.ensure_lamps(Duration::from_secs(10)).await;
            show(
                &serde_json::to_value(ctx.handle.snapshot()).map_err(|e| e.to_string())?,
                g.json,
            );
        }
        Cmd::Scan => {
            ctx.handle.discover().await;
            tokio::time::sleep(Duration::from_secs(8)).await;
            show(
                &serde_json::to_value(ctx.handle.snapshot()).map_err(|e| e.to_string())?,
                g.json,
            );
        }
        Cmd::Library => println!("{}", library_text(&ctx.library)),
        Cmd::SaveAnimation { name, file } => {
            let spec = serde_json::from_str(
                &std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?,
            )
            .map_err(|e| e.to_string())?;
            ctx.library.save(Kind::Animations, &name, spec)?;
            println!("saved animation {name:?}");
        }
        Cmd::Control(args) => {
            let playing = args.get("animation").is_some();
            if playing && !g.hold {
                return Err("an animation only plays while lampbus keeps running. Start `lampbus serve` (commands then go to it automatically), or add --hold to stay running here.".into());
            }
            control::control(&ctx, &args).await?;
            let aliases: Vec<String> = args
                .get("device")
                .and_then(Value::as_str)
                .map(|d| vec![d.to_string()])
                .unwrap_or_default();
            let snap = ctx
                .handle
                .settled(&aliases, Duration::from_secs(30))
                .await?;
            show(
                &serde_json::to_value(snap).map_err(|e| e.to_string())?,
                g.json,
            );
            if playing {
                eprintln!("lampbus: playing; press Ctrl-C to stop");
                let _ = tokio::signal::ctrl_c().await;
            }
        }
        Cmd::Raw { command, args } => {
            ctx.ensure_lamps(Duration::from_secs(12)).await;
            let target = g
                .device
                .clone()
                .map_or(lampbus_core::Target::All, lampbus_core::Target::Named);
            let pairs = args
                .iter()
                .filter_map(|(k, v)| v.as_f64().map(|n| (k.clone(), n)))
                .collect();
            ctx.handle
                .command(
                    target,
                    lampbus_core::Command::Raw {
                        command,
                        args: pairs,
                    },
                )
                .await
                .map_err(|e| e.to_string())?;
            ctx.handle.settled(&[], Duration::from_secs(30)).await?;
            println!("sent");
        }
        Cmd::Help | Cmd::Version | Cmd::Definitions | Cmd::Serve | Cmd::Mcp => {}
    }
    Ok(())
}

fn library_text(lib: &Library) -> String {
    let mut out = String::new();
    for (title, kind) in [("animations", Kind::Animations), ("presets", Kind::Presets)] {
        out.push_str(&format!("{title}:\n"));
        for (name, spec) in lib.all(kind) {
            out.push_str(&format!(
                "  {name:14} {}\n",
                spec.get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
            ));
        }
    }
    out.trim_end().to_string()
}

async fn via_remote(g: &Global, r: &Remote, cmd: Cmd) -> Result<(), String> {
    match cmd {
        Cmd::Status | Cmd::Scan => show(&r.call("GET", "/api/state", None).await?, g.json),
        Cmd::Control(args) => show(&r.call("POST", "/api/control", Some(&args)).await?, g.json),
        Cmd::Library => {
            let v = r.call("GET", "/api/library", None).await?;
            let mut out = String::new();
            for title in ["animations", "presets"] {
                out.push_str(&format!("{title}:\n"));
                if let Some(m) = v.get(title).and_then(Value::as_object) {
                    for (n, spec) in m {
                        out.push_str(&format!(
                            "  {n:14} {}\n",
                            spec.get("description")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                        ));
                    }
                }
            }
            println!("{}", out.trim_end());
        }
        Cmd::SaveAnimation { name, file } => {
            let spec: Value = serde_json::from_str(
                &std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?,
            )
            .map_err(|e| e.to_string())?;
            r.call("PUT", &format!("/api/animations/{name}"), Some(&spec))
                .await?;
            println!("saved animation {name:?}");
        }
        Cmd::Raw { command, args } => {
            let body = json!({"command": command, "args": args, "device": g.device});
            r.call("POST", "/api/raw", Some(&body)).await?;
            println!("sent");
        }
        Cmd::Help | Cmd::Version | Cmd::Definitions | Cmd::Serve | Cmd::Mcp => {}
    }
    Ok(())
}
