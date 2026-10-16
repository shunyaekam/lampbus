//! `lampbus`: control Bluetooth lamps from the command line, an HTTP API, or an MCP server (for agents).

mod air;
mod assets;
mod cli;
mod config;
mod control;
#[cfg(feature = "http")]
mod http;
mod ingest;
mod library;
#[cfg(feature = "mcp")]
mod mcp;
mod remote;
mod schedule;
#[cfg(feature = "http")]
mod web;

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
        timers: Default::default(),
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
        Cmd::Check {
            definition,
            vectors,
        } => return check(&definition, vectors.as_deref()),
        Cmd::Air { what } => {
            let settings = config::load(g.config.as_deref())?;
            return air::run(
                &what,
                g.group.unwrap_or(1),
                g.secs.unwrap_or(2),
                g.print,
                &settings.config_dir,
            );
        }
        Cmd::Gatt { name, seconds } => {
            let report =
                lampbus_shell::inspect::gatt(name.as_deref(), Duration::from_secs(seconds)).await?;
            print!("{report}");
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
    schedule::check(&ctx, &settings.schedules)?;
    tokio::spawn(schedule::run(
        std::sync::Arc::new(ctx.clone()),
        settings.schedules.clone(),
    ));
    let target = settings
        .stream_device
        .clone()
        .map_or(lampbus_core::Target::All, lampbus_core::Target::Named);
    if let Some(addr) = settings.udp_stream.clone() {
        eprintln!("lampbus: UDP stream input on {addr}");
        let (h, t) = (ctx.handle.clone(), target.clone());
        tokio::spawn(async move {
            eprintln!(
                "lampbus: udp_stream stopped: {:?}",
                ingest::run_simple(addr, h, t).await
            )
        });
    }
    if let Some(addr) = settings.ddp.clone() {
        eprintln!("lampbus: DDP input on {addr}");
        let h = ctx.handle.clone();
        tokio::spawn(async move {
            eprintln!(
                "lampbus: ddp stopped: {:?}",
                ingest::run_ddp(addr, h, target).await
            )
        });
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
        Cmd::Calibrate(what) => {
            let (question, steps) = cli::calibration_steps(&what)?;
            for st in steps {
                println!("-> {}", st.say);
                control::control(&ctx, &st.control).await?;
                ctx.handle.settled(&[], Duration::from_secs(30)).await?;
                tokio::time::sleep(Duration::from_secs(st.seconds)).await;
            }
            println!("\n{question}");
        }
        Cmd::Help
        | Cmd::Version
        | Cmd::Definitions
        | Cmd::Serve
        | Cmd::Mcp
        | Cmd::Check { .. }
        | Cmd::Gatt { .. }
        | Cmd::Air { .. } => {}
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

/// After sending commands to a running daemon, wait until the lamp has really received them: nothing pending and no
/// error. A link stuck in back-off is reported as a failure, never as success.
async fn confirmed(r: &Remote, limit: Duration) -> Result<(), String> {
    let deadline = std::time::Instant::now() + limit;
    loop {
        let state = r.call("GET", "/api/state", None).await?;
        let lamps = state
            .get("lamps")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let problem = lamps.iter().find_map(|l| {
            let pending = l.get("pending").and_then(Value::as_u64).unwrap_or(0);
            let link = l.get("link").and_then(Value::as_str).unwrap_or("");
            let err = l.get("last_error").and_then(Value::as_str);
            // an animation or a stream always has a frame in flight; it is delivered when the link is up and healthy
            let playing = matches!(
                l.get("source").and_then(Value::as_str),
                Some("animation" | "stream")
            );
            let settled = err.is_none()
                && if playing {
                    link == "ready"
                } else {
                    pending == 0 && (link == "ready" || link == "down")
                };
            (!settled).then(|| {
                format!(
                    "{} is {link} ({})",
                    l.get("alias").and_then(Value::as_str).unwrap_or("lamp"),
                    err.unwrap_or("still sending")
                )
            })
        });
        match problem {
            None => return Ok(()),
            Some(p) if std::time::Instant::now() >= deadline => {
                return Err(format!("the lamp did not confirm: {p}"))
            }
            Some(_) => tokio::time::sleep(Duration::from_millis(250)).await,
        }
    }
}

async fn via_remote(g: &Global, r: &Remote, cmd: Cmd) -> Result<(), String> {
    match cmd {
        Cmd::Status | Cmd::Scan => show(&r.call("GET", "/api/state", None).await?, g.json),
        Cmd::Control(args) => {
            let state = r.call("POST", "/api/control", Some(&args)).await?;
            confirmed(r, Duration::from_secs(30)).await?;
            show(&state, g.json);
        }
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
        Cmd::Calibrate(what) => {
            let (question, steps) = cli::calibration_steps(&what)?;
            for st in steps {
                println!("-> {}", st.say);
                r.call("POST", "/api/control", Some(&st.control)).await?;
                confirmed(r, Duration::from_secs(30)).await?;
                println!("   (the lamp confirmed; look now for {} s)", st.seconds);
                tokio::time::sleep(Duration::from_secs(st.seconds)).await;
            }
            println!("\n{question}");
        }
        Cmd::Help
        | Cmd::Version
        | Cmd::Definitions
        | Cmd::Serve
        | Cmd::Mcp
        | Cmd::Check { .. }
        | Cmd::Gatt { .. }
        | Cmd::Air { .. } => {}
    }
    Ok(())
}

/// `lampbus check`: parse a lamp definition and reproduce its test vectors byte for byte, so a new lamp can be
/// validated without Bluetooth, a lamp, or a build environment.
fn check(definition: &std::path::Path, vectors: Option<&std::path::Path>) -> Result<(), String> {
    let text = std::fs::read_to_string(definition)
        .map_err(|e| format!("{}: {e}", definition.display()))?;
    let def = lampbus_defs::Definition::from_json(&text)
        .map_err(|e| format!("{}: {e}", definition.display()))?;
    println!(
        "{}: valid. {} ({}), {} commands, {} effects, status {:?}",
        definition.display(),
        def.name,
        def.id,
        def.commands.len(),
        def.effects.len(),
        def.status
    );
    let sibling = definition.with_extension("vectors.json");
    let path = vectors
        .map(std::path::Path::to_path_buf)
        .or_else(|| sibling.exists().then_some(sibling));
    let Some(path) = path else {
        println!("no vectors file given or found next to it: add one so others can trust it");
        return Ok(());
    };
    let vecs = lampbus_defs::parse_vectors(
        &std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?,
    )
    .map_err(|e| format!("{}: {e}", path.display()))?;
    let failures = def.verify_vectors(&vecs);
    for f in &failures {
        println!("  vector {} ({}): {}", f.index, f.command, f.reason);
    }
    let untested: Vec<&String> = def
        .commands
        .keys()
        .filter(|c| !vecs.iter().any(|v| &v.command == *c))
        .collect();
    if !untested.is_empty() {
        println!(
            "  commands with no vector: {}",
            untested
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if failures.is_empty() {
        println!("{} vectors reproduce exactly.", vecs.len());
        Ok(())
    } else {
        Err(format!(
            "{} of {} vectors failed",
            failures.len(),
            vecs.len()
        ))
    }
}
