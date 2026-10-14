//! HTTP + WebSocket API. JSON in/out, Bearer-token auth (header or ?token=).
//! Same routes and payloads as the Python build, so lampctl and any client work with either.

use crate::engine::{Cmd, Error, Handle};
use crate::library::Kind;
use crate::protocol;
use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone)]
struct App {
    engine: Handle,
    token: Arc<str>,
}

struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        match e {
            Error::Bad(m) => ApiError(StatusCode::BAD_REQUEST, m),
            Error::NotFound(m) => ApiError(StatusCode::NOT_FOUND, m),
        }
    }
}

impl From<crate::library::LibError> for ApiError {
    fn from(e: crate::library::LibError) -> Self {
        Error::from(e).into()
    }
}

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}

type Res = Result<Json<Value>, ApiError>;

/// Constant-time comparison so token checks don't leak length-of-match timing.
fn token_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn guard(State(app): State<App>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>, req: Request, next: Next) -> Response {
    if req.uri().path().starts_with("/api/") {
        let given = headers.get("authorization").and_then(|v| v.to_str().ok())
            .map(|v| v.strip_prefix("Bearer ").unwrap_or(v)).or_else(|| q.get("token").map(String::as_str)).unwrap_or("");
        if !token_eq(given, &app.token) {
            return ApiError(StatusCode::UNAUTHORIZED, "unauthorized".into()).into_response();
        }
    }
    next.run(req).await
}

/// JSON object body; an empty body counts as {}.
fn body(bytes: &Bytes) -> Result<Map<String, Value>, ApiError> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(Map::new());
    }
    match serde_json::from_slice(bytes) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err(bad("body must be a JSON object")),
        Err(e) => Err(bad(format!("invalid JSON: {e}"))),
    }
}

fn need<'a>(d: &'a Map<String, Value>, k: &str) -> Result<&'a Value, ApiError> {
    d.get(k).ok_or_else(|| bad(format!("missing field {k:?}")))
}

fn opt_f64(d: &Map<String, Value>, k: &str) -> Result<Option<f64>, ApiError> {
    match d.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_f64().map(Some).ok_or_else(|| bad(format!("{k} must be a number"))),
    }
}

async fn call(app: &App, cmd: Cmd) -> Res {
    Ok(Json(app.engine.call(cmd).await?))
}

pub fn router(engine: Handle, token: String) -> Router {
    let app = App { engine, token: token.into() };
    Router::new()
        .route("/healthz", get(|| async { Json(json!({"ok": true})) }))
        .route("/api/state", get(|State(a): State<App>| async move { call(&a, Cmd::Snapshot).await }))
        .route("/api/power", post(|State(a): State<App>, b: Bytes| async move {
            let d = body(&b)?;
            let on = match d.get("on") {
                None | Some(Value::Null) => None,
                Some(v) => Some(v.as_bool().ok_or_else(|| bad("on must be true or false"))?),
            };
            call(&a, Cmd::Power(on)).await
        }))
        .route("/api/color", post(|State(a): State<App>, b: Bytes| async move {
            let d = body(&b)?;
            let brightness = opt_f64(&d, "brightness")?;
            if !d.contains_key("color") && d.contains_key("hue") {
                let hue = opt_f64(&d, "hue")?.unwrap_or(0.0);
                let sat = opt_f64(&d, "saturation")?.unwrap_or(100.0);
                return call(&a, Cmd::Hs { hue, sat, brightness }).await;
            }
            call(&a, Cmd::Color { color: need(&d, "color")?.clone(), brightness }).await
        }))
        .route("/api/brightness", post(|State(a): State<App>, b: Bytes| async move {
            let v = need(&body(&b)?, "value")?.as_f64().ok_or_else(|| bad("value must be a number"))?;
            call(&a, Cmd::Brightness(v)).await
        }))
        .route("/api/effect", post(|State(a): State<App>, b: Bytes| async move {
            let d = body(&b)?;
            call(&a, Cmd::Effect { effect: need(&d, "effect")?.clone(), speed: opt_f64(&d, "speed")?, brightness: opt_f64(&d, "brightness")? }).await
        }))
        .route("/api/effects", get(|| async {
            let effects: Map<String, Value> = protocol::EFFECTS.iter().map(|(k, v)| (k.to_string(), json!(v))).collect();
            let aliases: Map<String, Value> = protocol::EFFECT_ALIASES.iter().map(|(k, v)| (k.to_string(), json!(v))).collect();
            Json(json!({"effects": effects, "aliases": aliases}))
        }))
        // animations
        .route("/api/animations", get(|State(a): State<App>| async move { Json(Value::Object(a.engine.library.all(Kind::Animations))) }))
        .route("/api/animation", post(|State(a): State<App>, b: Bytes| async move {
            let d = body(&b)?;
            let brightness = opt_f64(&d, "brightness")?;
            if let Some(spec) = d.get("spec") {
                return call(&a, Cmd::Play { name: None, spec: Some(spec.clone()), brightness }).await;
            }
            let name = need(&d, "name")?.as_str().ok_or_else(|| bad("name must be a string"))?.to_string();
            call(&a, Cmd::Play { name: Some(name), spec: None, brightness }).await
        }).delete(|State(a): State<App>| async move { call(&a, Cmd::StopAnim).await }))
        .route("/api/animations/{name}", put(|State(a): State<App>, Path(n): Path<String>, b: Bytes| async move {
            let spec = Value::Object(body(&b)?);
            Ok::<_, ApiError>(Json(a.engine.library.save(Kind::Animations, &n, spec)?))
        }).merge(delete(|State(a): State<App>, Path(n): Path<String>| async move {
            a.engine.library.delete(Kind::Animations, &n)?;
            Ok::<_, ApiError>(Json(json!({"deleted": n})))
        })))
        // presets
        .route("/api/presets", get(|State(a): State<App>| async move { Json(Value::Object(a.engine.library.all(Kind::Presets))) }))
        .route("/api/presets/{name}", put(|State(a): State<App>, Path(n): Path<String>, b: Bytes| async move {
            let spec = Value::Object(body(&b)?);
            Ok::<_, ApiError>(Json(a.engine.library.save(Kind::Presets, &n, spec)?))
        }).merge(delete(|State(a): State<App>, Path(n): Path<String>| async move {
            a.engine.library.delete(Kind::Presets, &n)?;
            Ok::<_, ApiError>(Json(json!({"deleted": n})))
        })))
        .route("/api/presets/{name}/apply", post(|State(a): State<App>, Path(n): Path<String>| async move { call(&a, Cmd::ApplyPreset(n)).await }))
        .route("/api/presets/{name}/save-current", post(|State(a): State<App>, Path(n): Path<String>| async move { call(&a, Cmd::SaveCurrent(n)).await }))
        // raw / manual mode
        .route("/api/ops", get(|| async { Json(protocol::ops_json()) }))
        .route("/api/raw", post(|State(a): State<App>, b: Bytes| async move {
            let d = body(&b)?;
            let frame = if let Some(h) = d.get("hex") {
                protocol::raw(h.as_str().ok_or_else(|| bad("hex must be a string"))?).map_err(bad)?
            } else {
                let op = need(&d, "op")?.as_str().ok_or_else(|| bad("op must be a string"))?;
                let args = d.get("args").and_then(Value::as_object).cloned().unwrap_or_default();
                protocol::build(op, &args).map_err(bad)?
            };
            call(&a, Cmd::Raw(frame)).await
        }))
        // live streams
        .route("/api/stream", get(|State(a): State<App>, ws: WebSocketUpgrade| async move {
            ws.on_upgrade(move |s| stream_socket(s, a.engine))
        }))
        .route("/api/events", get(|State(a): State<App>, ws: WebSocketUpgrade| async move {
            ws.on_upgrade(move |s| events_socket(s, a.engine))
        }))
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app)
}

/// Screen/music clients push frames: binary `r g b [brightness]` or text `{"rgb":[r,g,b],"brightness":n}`.
async fn stream_socket(mut ws: WebSocket, engine: Handle) {
    while let Some(Ok(msg)) = ws.recv().await {
        match msg {
            Message::Binary(b) if b.len() >= 3 => {
                engine.stream([b[0] as f64, b[1] as f64, b[2] as f64], b.get(3).map_or(100.0, |v| *v as f64));
            }
            Message::Text(t) => {
                let parsed = serde_json::from_str::<Value>(&t).ok().and_then(|d| {
                    let rgb = d.get("rgb")?.as_array()?;
                    Some(([rgb.first()?.as_f64()?, rgb.get(1)?.as_f64()?, rgb.get(2)?.as_f64()?],
                          d.get("brightness").and_then(Value::as_f64).unwrap_or(100.0)))
                });
                match parsed {
                    Some((rgb, bri)) => engine.stream(rgb, bri),
                    None => { let _ = ws.send(Message::Text(r#"{"error":"expected {\"rgb\":[r,g,b],\"brightness\":0-100}"}"#.into())).await; }
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
}

/// Pushes the state snapshot whenever it changes.
async fn events_socket(mut ws: WebSocket, engine: Handle) {
    let mut rx = engine.events.clone();
    loop {
        let snapshot = rx.borrow_and_update().to_string();
        if ws.send(Message::Text(snapshot.into())).await.is_err() {
            return;
        }
        tokio::select! {
            r = rx.changed() => if r.is_err() { return },
            m = ws.recv() => if !matches!(m, Some(Ok(_))) { return },
        }
    }
}
