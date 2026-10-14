//! HTTP + WebSocket API. JSON in and out, bearer-token auth (header or `?token=`).
//!
//! `POST /api/control` is the whole command language (see `control.rs`); the rest is reading state and managing
//! the library. Live sources (screen, music) connect to `/api/stream` and push colours.

use crate::control::{control, Ctx};
use crate::library::Kind;
use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use lampbus_core::{Command, Target};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

struct App {
    ctx: Ctx,
    token: String,
    definitions: Value,
}

type Shared = State<Arc<App>>;
fn fail(code: StatusCode, msg: impl ToString) -> (StatusCode, Json<Value>) {
    (code, Json(json!({"error": msg.to_string()})))
}

fn bad(msg: impl ToString) -> (StatusCode, Json<Value>) {
    fail(StatusCode::BAD_REQUEST, msg)
}

fn token_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

async fn guard(
    State(app): Shared,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
    req: Request,
    next: Next,
) -> Response {
    if req.uri().path().starts_with("/api/") {
        let given = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.strip_prefix("Bearer ").unwrap_or(v))
            .or_else(|| q.get("token").map(String::as_str))
            .unwrap_or("");
        if !token_eq(given, &app.token) {
            return fail(StatusCode::UNAUTHORIZED, "unauthorized").into_response();
        }
    }
    next.run(req).await
}

fn json_body(b: &Bytes) -> Result<Value, (StatusCode, Json<Value>)> {
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
    let router = Router::new()
        .route("/", get(|| async { Html(include_str!("ui.html")) }))
        .route("/healthz", get(|| async { Json(json!({"ok": true})) }))
        .route("/api/state", get(|State(a): Shared| async move {
            a.ctx.ensure_lamps(std::time::Duration::from_secs(1)).await;
            serde_json::to_value(a.ctx.handle.snapshot()).map(Json).map_err(bad)
        }))
        .route("/api/control", post(|State(a): Shared, b: Bytes| async move {
            control(&a.ctx, &json_body(&b)?).await.map(Json).map_err(bad)
        }))
        .route("/api/library", get(|State(a): Shared| async move {
            Json(json!({"animations": a.ctx.library.all(Kind::Animations), "presets": a.ctx.library.all(Kind::Presets)}))
        }))
        .route("/api/definitions", get(|State(a): Shared| async move { Json(a.definitions.clone()) }))
        .route("/api/{kind}/{name}", put(|State(a): Shared, Path((kind, name)): Path<(String, String)>, b: Bytes| async move {
            if kind != "animations" && kind != "presets" {
                return Err(fail(StatusCode::NOT_FOUND, "not found"));
            }
            a.ctx.library.save(kind_of(&kind), &name, json_body(&b)?).map(Json).map_err(bad)
        }).delete(|State(a): Shared, Path((kind, name)): Path<(String, String)>| async move {
            if kind != "animations" && kind != "presets" {
                return Err(fail(StatusCode::NOT_FOUND, "not found"));
            }
            a.ctx.library.delete(kind_of(&kind), &name).map(|()| Json(json!({"deleted": name}))).map_err(|e| fail(StatusCode::NOT_FOUND, e))
        }))
        .route("/api/raw", post(|State(a): Shared, b: Bytes| async move {
            let d = json_body(&b)?;
            let command = d.get("command").and_then(Value::as_str).ok_or_else(|| bad("missing \"command\""))?.to_string();
            let args = d
                .get("args")
                .and_then(Value::as_object)
                .map(|o| o.iter().filter_map(|(k, v)| v.as_f64().map(|n| (k.clone(), n))).collect())
                .unwrap_or_default();
            let target = d.get("device").and_then(Value::as_str).map_or(Target::All, |n| Target::Named(n.into()));
            a.ctx.handle.command(target, Command::Raw { command, args }).await.map_err(bad)?;
            Ok::<_, (StatusCode, Json<Value>)>(Json(json!({"queued": true})))
        }))
        .route("/api/stream", get(|State(a): Shared, Query(q): Query<HashMap<String, String>>, ws: WebSocketUpgrade| async move {
            let target = q.get("device").map_or(Target::All, |n| Target::Named(n.clone()));
            ws.on_upgrade(move |s| stream_socket(s, a, target))
        }))
        .route("/api/events", get(|State(a): Shared, ws: WebSocketUpgrade| async move { ws.on_upgrade(move |s| events_socket(s, a)) }))
        .layer(middleware::from_fn_with_state(Arc::clone(&app), guard))
        .with_state(Arc::clone(&app));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
}

/// Live sources push frames: binary `r g b [brightness]` or text `{"rgb":[r,g,b],"brightness":n}`.
async fn stream_socket(mut ws: WebSocket, app: Arc<App>, target: Target) {
    while let Some(Ok(msg)) = ws.recv().await {
        match msg {
            Message::Binary(b) => {
                // r g b [brightness]
                if let [r, g, bl, rest @ ..] = b.as_ref() {
                    let brightness = rest.first().map_or(100.0, |v| f64::from(*v));
                    app.ctx.handle.stream(
                        target.clone(),
                        [f64::from(*r), f64::from(*g), f64::from(*bl)],
                        brightness,
                    );
                }
            }
            Message::Text(t) => {
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
                            .send(Message::Text(
                                r#"{"error":"expected {\"rgb\":[r,g,b],\"brightness\":0-100}"}"#
                                    .into(),
                            ))
                            .await;
                    }
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
}

/// Pushes the state snapshot whenever it changes.
async fn events_socket(mut ws: WebSocket, app: Arc<App>) {
    let mut rx = app.ctx.handle.subscribe();
    loop {
        let snapshot = serde_json::to_string(&*rx.borrow_and_update()).unwrap_or_default();
        if ws.send(Message::Text(snapshot.into())).await.is_err() {
            return;
        }
        tokio::select! {
            r = rx.changed() => if r.is_err() { return },
            m = ws.recv() => if !matches!(m, Some(Ok(_))) { return },
        }
    }
}
