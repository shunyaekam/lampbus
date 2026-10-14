//! lampd (Rust): lightweight controller service for iStrip BLE lamps.
//!
//! Environment (all optional):
//!   LAMPD_NAME     BLE name of the lamp                  (SSL-E089ED)
//!   LAMPD_BIND     address for the HTTP API              (127.0.0.1; use `tailscale serve` to expose it)
//!   LAMPD_PORT     HTTP port                             (8787)
//!   LAMPD_IDLE     seconds before dropping BLE, 0=never  (120)
//!   LAMPD_LIBRARY  directory with animations/ presets/   (./library)
//!   LAMPD_CONFIG   secrets/state directory               (~/.config/lampd)
//!   LAMPD_TOKEN    API token (default: generated into <config>/token)

mod anim;
mod api;
mod device;
mod engine;
mod library;
mod protocol;

use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

fn env(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn expand_home(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => PathBuf::from(env("HOME", ".")).join(rest),
        None => PathBuf::from(p),
    }
}

/// URL-safe base64 without padding, enough for a token.
fn b64url(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |a, (i, b)| a | (*b as u32) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

fn load_token(dir: &PathBuf) -> std::io::Result<String> {
    if let Ok(t) = std::env::var("LAMPD_TOKEN") {
        return Ok(t);
    }
    let path = dir.join("token");
    if !path.exists() {
        let mut raw = [0u8; 24];
        getrandom::fill(&mut raw).expect("no system randomness");
        let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?;
        f.write_all(b64url(&raw).as_bytes())?;
        eprintln!("lampd: generated API token at {}", path.display());
    }
    Ok(fs::read_to_string(path)?.trim().to_string())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    let config = expand_home(&env("LAMPD_CONFIG", "~/.config/lampd"));
    fs::create_dir_all(&config)?;
    let token = load_token(&config)?;

    let default_library = std::env::current_dir()?.join("library");
    let library = Arc::new(library::Library::new(expand_home(&env("LAMPD_LIBRARY", default_library.to_str().unwrap())))?);

    let idle: f64 = env("LAMPD_IDLE", "120").parse().unwrap_or(120.0);
    let device = device::spawn(device::Config {
        name: env("LAMPD_NAME", "SSL-E089ED"),
        idle: Duration::from_secs_f64(idle.max(0.0)),
        min_interval: Duration::from_millis(25),
    });
    let engine = engine::spawn(device, library, config.join("state.json"));

    let addr = format!("{}:{}", env("LAMPD_BIND", "127.0.0.1"), env("LAMPD_PORT", "8787"));
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    eprintln!("lampd: API on http://{addr} (token: {})", config.join("token").display());
    axum::serve(listener, api::router(engine, token))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
}
