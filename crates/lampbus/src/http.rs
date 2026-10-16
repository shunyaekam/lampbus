//! HTTP + WebSocket API. JSON in and out, bearer-token auth (header or `?token=`).
//!
//! `POST /api/control` is the whole command language (see `control.rs`); the rest is reading state and managing
//! the library. Live sources (screen, music) connect to `/api/stream` and push colours. The transport is the small
//! server in `web.rs`; this file is routing.

use crate::control::{control, Ctx};
use crate::library::Kind;
use crate::web::{self, Msg, Reply, Request, Response, Ws};
use lampbus_core::{Command, Target};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

struct App {
    ctx: Ctx,
    token: String,
    definitions: Value,
}

fn json_reply(status: u16, v: &Value) -> Reply {
    Reply::Http(Response {
        status,
        content_type: "application/json",
        body: v.to_string().into_bytes(),
    })
}

fn fail(status: u16, msg: impl ToString) -> Reply {
    json_reply(status, &json!({"error": msg.to_string()}))
}

fn bad(msg: impl ToString) -> Reply {
    fail(400, msg)
}

fn token_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

fn authorised(app: &App, req: &Request) -> bool {
    let given = req
        .header("authorization")
        .map(|v| v.strip_prefix("Bearer ").unwrap_or(v))
        .or_else(|| req.query("token"))
        .unwrap_or("");
    token_eq(given, &app.token)
}

fn json_body(b: &[u8]) -> Result<Value, Reply> {
    if b.iter().all(u8::is_ascii_whitespace) {
        return Ok(json!({}));
    }
    serde_json::from_slice(b).map_err(|e| bad(format!("invalid JSON: {e}")))
}

fn kind_of(name: &str) -> Kind {
    if name == "animations" {
        Kind::Animations
    } else {
        Kind::Presets
    }
}

pub async fn serve(ctx: Ctx, definitions: Value, addr: &str, token: String) -> std::io::Result<()> {
    let app = Arc::new(App {
        ctx,
        token,
        definitions,
    });
    let listener = tokio::net::TcpListener::bind(addr).await?;
    web::serve(listener, app, route).await
}

async fn route(app: Arc<App>, req: Request) -> Reply {
    let path = req.path.clone();
    if path.starts_with("/api/") && !authorised(&app, &req) {
        return fail(401, "unauthorized");
    }
    match (req.method.as_str(), path.as_str()) {
        ("GET", "/") => Reply::Http(Response {
            status: 200,
            content_type: "text/html; charset=utf-8",
            body: include_str!("ui.html").as_bytes().to_vec(),
        }),
        ("GET", "/healthz") => json_reply(200, &json!({"ok": true})),
        ("GET", "/api/state") => {
            app.ctx.ensure_lamps(Duration::from_secs(1)).await;
            state_reply(&app, req.query("wait").and_then(|w| w.parse().ok())).await
        }
        ("POST", "/api/control") => match json_body(&req.body) {
            Ok(v) => match control(&app.ctx, &v).await {
                Ok(state) => json_reply(200, &state),
                Err(e) => bad(e),
            },
            Err(r) => r,
        },
        ("GET", "/api/direct") => json_reply(200, &app.ctx.forward.status()),
        ("POST", "/api/direct") => match json_body(&req.body) {
            Ok(v) => match v.get("on").and_then(Value::as_bool) {
                Some(on) => {
                    app.ctx.forward.set_direct(on);
                    json_reply(200, &app.ctx.forward.status())
                }
                None => bad("give {\"on\": true} to control the lamp directly, or {\"on\": false} to hand it to the controller"),
            },
            Err(r) => r,
        },
        ("GET", "/api/library") => json_reply(
            200,
            &json!({"animations": app.ctx.library.all(Kind::Animations), "presets": app.ctx.library.all(Kind::Presets)}),
        ),
        ("GET", "/api/definitions") => json_reply(200, &app.definitions),
        ("POST", "/api/raw") => raw(&app, &req.body).await,
        ("GET", "/api/stream") => {
            let target = req
                .query("device")
                .map_or(Target::All, |n| Target::Named(n.to_string()));
            Reply::Upgrade(Box::new(move |ws| Box::pin(stream_socket(ws, app, target))))
        }
        ("GET", "/api/events") => {
            Reply::Upgrade(Box::new(move |ws| Box::pin(events_socket(ws, app))))
        }
        (method, p) => library_item(&app, method, p, &req.body),
    }
}

/// `PUT` and `DELETE /api/{animations|presets}/{name}`.
fn library_item(app: &App, method: &str, path: &str, body: &[u8]) -> Reply {
    let mut parts = path.strip_prefix("/api/").unwrap_or("").split('/');
    let (Some(kind), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
        return fail(404, "not found");
    };
    if (kind != "animations" && kind != "presets") || name.is_empty() {
        return fail(404, "not found");
    }
    match method {
        "PUT" => match json_body(body) {
            Ok(spec) => match app.ctx.library.save(kind_of(kind), name, spec) {
                Ok(v) => json_reply(200, &v),
                Err(e) => bad(e),
            },
            Err(r) => r,
        },
        "DELETE" => match app.ctx.library.delete(kind_of(kind), name) {
            Ok(()) => json_reply(200, &json!({"deleted": name})),
            Err(e) => fail(404, e),
        },
        _ => fail(405, "method not allowed"),
    }
}

async fn raw(app: &App, body: &[u8]) -> Reply {
    let d = match json_body(body) {
        Ok(d) => d,
        Err(r) => return r,
    };
    let Some(command) = d.get("command").and_then(Value::as_str) else {
        return bad("missing \"command\"");
    };
    let args = d
        .get("args")
        .and_then(Value::as_object)
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| v.as_f64().map(|n| (k.clone(), n)))
                .collect()
        })
        .unwrap_or_default();
    let target = d
        .get("device")
        .and_then(Value::as_str)
        .map_or(Target::All, |n| Target::Named(n.into()));
    match app
        .ctx
        .handle
        .command(
            target,
            Command::Raw {
                command: command.to_string(),
                args,
            },
        )
        .await
    {
        Ok(_) => json_reply(200, &json!({"queued": true})),
        Err(e) => bad(e),
    }
}

/// Live sources push frames: binary `r g b [brightness]` or text `{"rgb":[r,g,b],"brightness":n}`.
async fn stream_socket(mut ws: Ws, app: Arc<App>, target: Target) {
    while let Some(msg) = ws.recv().await {
        match msg {
            Msg::Binary(b) => {
                // r g b [brightness]
                if let [r, g, bl, rest @ ..] = b.as_slice() {
                    let brightness = rest.first().map_or(100.0, |v| f64::from(*v));
                    app.ctx.handle.stream(
                        target.clone(),
                        [f64::from(*r), f64::from(*g), f64::from(*bl)],
                        brightness,
                    );
                }
            }
            Msg::Text(t) => {
                let parsed = serde_json::from_str::<Value>(&t).ok().and_then(|d| {
                    let c = d.get("rgb")?.as_array()?;
                    Some((
                        [
                            c.first()?.as_f64()?,
                            c.get(1)?.as_f64()?,
                            c.get(2)?.as_f64()?,
                        ],
                        d.get("brightness").and_then(Value::as_f64).unwrap_or(100.0),
                    ))
                });
                match parsed {
                    Some((rgb, bri)) => app.ctx.handle.stream(target.clone(), rgb, bri),
                    None => {
                        let _ = ws
                            .send_text(
                                r#"{"error":"expected {\"rgb\":[r,g,b],\"brightness\":0-100}"}"#,
                            )
                            .await;
                    }
                }
            }
            Msg::Ping(p) => {
                let _ = ws.pong(&p).await;
            }
        }
    }
}

/// Pushes the state snapshot whenever it changes.
async fn events_socket(mut ws: Ws, app: Arc<App>) {
    let mut rx = app.ctx.handle.subscribe();
    loop {
        let snapshot = serde_json::to_string(&*rx.borrow_and_update()).unwrap_or_default();
        if ws.send_text(&snapshot).await.is_err() {
            return;
        }
        tokio::select! {
            r = rx.changed() => if r.is_err() { return },
            m = ws.recv() => match m {
                Some(Msg::Ping(p)) => { let _ = ws.pong(&p).await; }
                Some(_) => {}
                None => return,
            },
        }
    }
}

/// FNV-1a over the response body: how a long-polling client says which state it has already seen.
pub fn body_hash(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf2_9ce4_8422_2325, |h, &x| {
        (h ^ u64::from(x)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The state snapshot. With `wait=<hash of the last body the caller saw>` it holds the request until the state
/// differs from that (or 50 s pass), so a client can follow the lamp without polling.
async fn state_reply(a: &App, wait: Option<u64>) -> Reply {
    let mut rx = a.ctx.handle.subscribe();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(50);
    loop {
        let body = match serde_json::to_value(&*rx.borrow_and_update()) {
            Ok(v) => v.to_string(),
            Err(e) => return bad(e),
        };
        let changed = wait != Some(body_hash(body.as_bytes()));
        if changed
            || tokio::time::timeout_at(deadline, rx.changed())
                .await
                .is_err()
        {
            return Reply::Http(Response {
                status: 200,
                content_type: "application/json",
                body: body.into_bytes(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_wait_hash_matches_the_matter_adapter() {
        // FNV-1a 64 of "a"; apps/matter/src/forward.rs pins the same value
        assert_eq!(super::body_hash(b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
