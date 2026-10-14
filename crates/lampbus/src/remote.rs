//! A tiny HTTP/1.1 client for talking to a running `lampbus serve` (plain HTTP, `Connection: close`).

use serde_json::Value;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

#[derive(Clone)]
pub struct Remote {
    pub addr: String,
    pub token: String,
}

impl Remote {
    /// Call the API and return the parsed JSON body, or the server's error message.
    pub async fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, String> {
        let payload = body.map(Value::to_string).unwrap_or_default();
        let req = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
            self.addr,
            self.token,
            payload.len()
        );
        let mut stream = timeout(Duration::from_secs(3), TcpStream::connect(&self.addr))
            .await
            .map_err(|_| format!("timed out connecting to {}", self.addr))?
            .map_err(|e| format!("cannot reach lampbus at {}: {e}", self.addr))?;
        stream
            .write_all(req.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        let mut raw = Vec::new();
        timeout(Duration::from_secs(60), stream.read_to_end(&mut raw))
            .await
            .map_err(|_| "request timed out".to_string())?
            .map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&raw);
        let (head, body) = text.split_once("\r\n\r\n").ok_or("malformed response")?;
        let status: u16 = head
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .ok_or("malformed status line")?;
        let value: Value = serde_json::from_str(body.trim()).unwrap_or(Value::Null);
        if (200..300).contains(&status) {
            Ok(value)
        } else {
            Err(value
                .get("error")
                .and_then(Value::as_str)
                .map_or_else(|| format!("server returned {status}"), str::to_string))
        }
    }

    /// Whether a daemon answers at this address.
    pub async fn alive(addr: &str) -> bool {
        let r = Remote {
            addr: addr.to_string(),
            token: String::new(),
        };
        timeout(Duration::from_millis(250), r.call("GET", "/healthz", None))
            .await
            .is_ok_and(|x| x.is_ok())
    }
}
