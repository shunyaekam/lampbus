//! A small HTTP/1.1 and WebSocket server: only what the lampbus API needs, in a few hundred lines, so the daemon does
//! not carry a general-purpose web framework.
//!
//! It knows nothing about lamps. `serve` accepts connections, parses one request per connection (every reply closes
//! the connection, as the API's clients expect), and hands it to a routing function that returns a [`Reply`]: an
//! HTTP response, or a WebSocket upgrade. Limits keep a hostile peer cheap: bounded header and body sizes, a read
//! deadline, a connection cap, no chunked bodies, no fragmented WebSocket messages.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;

/// Largest request head (request line and headers).
pub const MAX_HEAD: usize = 16 * 1024;
/// Largest request body.
pub const MAX_BODY: usize = 1024 * 1024;
/// Largest WebSocket message.
pub const MAX_FRAME: usize = 64 * 1024;
const MAX_CONNECTIONS: usize = 64;
const READ_DEADLINE: Duration = Duration::from_secs(10);

/// A parsed request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    /// The path, percent-decoded.
    pub path: String,
    /// The query string as decoded pairs.
    pub query: Vec<(String, String)>,
    /// Header names are lower-cased.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn query(&self, name: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// What a route answers with.
pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
}

type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// A route's answer: a response, or an upgrade to a WebSocket that the closure then drives.
pub enum Reply {
    Http(Response),
    Upgrade(Box<dyn FnOnce(Ws) -> BoxFuture + Send>),
}

/// A request that cannot be served, with the status to answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpError(pub u16, pub &'static str);

/// The parsed request line and headers.
#[derive(Debug, PartialEq, Eq)]
pub struct Head {
    pub method: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub content_length: usize,
}

/// Parse a request head (everything before the blank line).
pub fn parse_head(head: &[u8]) -> Result<Head, HttpError> {
    let text =
        std::str::from_utf8(head).map_err(|_| HttpError(400, "request is not valid text"))?;
    let mut lines = text.split("\r\n");
    let line = lines.next().unwrap_or("");
    let mut parts = line.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(HttpError(400, "bad request line"));
    };
    if method.is_empty() || method.len() > 8 || !method.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(HttpError(400, "bad method"));
    }
    if !target.starts_with('/') {
        return Err(HttpError(400, "bad target"));
    }
    if version != "HTTP/1.1" && version != "HTTP/1.0" {
        return Err(HttpError(400, "unsupported HTTP version"));
    }
    let mut headers = Vec::new();
    let mut content_length: Option<usize> = None;
    for l in lines {
        if l.is_empty() {
            continue;
        }
        let (name, value) = l.split_once(':').ok_or(HttpError(400, "bad header"))?;
        if name.is_empty() || name.bytes().any(|b| b <= b' ' || b == 0x7f) {
            return Err(HttpError(400, "bad header name"));
        }
        let name = name.to_ascii_lowercase();
        let value = value.trim().to_string();
        if name == "transfer-encoding" {
            return Err(HttpError(
                501,
                "chunked bodies are not supported; send Content-Length",
            ));
        }
        if name == "content-length" {
            let n: usize = value
                .parse()
                .map_err(|_| HttpError(400, "bad Content-Length"))?;
            if content_length.is_some_and(|c| c != n) {
                return Err(HttpError(400, "conflicting Content-Length"));
            }
            content_length = Some(n);
        }
        headers.push((name, value));
    }
    let content_length = content_length.unwrap_or(0);
    if content_length > MAX_BODY {
        return Err(HttpError(413, "body too large"));
    }
    Ok(Head {
        method: method.to_string(),
        target: target.to_string(),
        headers,
        content_length,
    })
}

/// Percent-decode; `plus` also turns `+` into a space (query strings).
pub fn percent_decode(s: &str, plus: bool) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while let Some(&c) = b.get(i) {
        let hex = |k: usize| b.get(k).and_then(|x| (*x as char).to_digit(16));
        match c {
            b'%' => match (hex(i + 1), hex(i + 2)) {
                (Some(h), Some(l)) => {
                    out.push((h * 16 + l) as u8);
                    i += 3;
                    continue;
                }
                _ => out.push(c),
            },
            b'+' if plus => out.push(b' '),
            _ => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn split_target(target: &str) -> (String, Vec<(String, String)>) {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let pairs = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (percent_decode(k, true), percent_decode(v, true))
        })
        .collect();
    (percent_decode(path, false), pairs)
}

// ---- SHA-1 and base64, for the WebSocket handshake only ------------------------------------------------------

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xefcd_ab89,
        0x98ba_dcfe,
        0x1032_5476,
        0xc3d2_e1f0,
    ];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 80];
        for (slot, word) in w.iter_mut().zip(chunk.chunks_exact(4)) {
            *slot = u32::from_be_bytes([
                word.first().copied().unwrap_or(0),
                word.get(1).copied().unwrap_or(0),
                word.get(2).copied().unwrap_or(0),
                word.get(3).copied().unwrap_or(0),
            ]);
        }
        for i in 16..80 {
            let v = w.get(i - 3).copied().unwrap_or(0)
                ^ w.get(i - 8).copied().unwrap_or(0)
                ^ w.get(i - 14).copied().unwrap_or(0)
                ^ w.get(i - 16).copied().unwrap_or(0);
            if let Some(slot) = w.get_mut(i) {
                *slot = v.rotate_left(1);
            }
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5a82_7999),
                20..=39 => (b ^ c ^ d, 0x6ed9_eba1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1b_bcdc),
                _ => (b ^ c ^ d, 0xca62_c1d6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        h = [
            h[0].wrapping_add(a),
            h[1].wrapping_add(b),
            h[2].wrapping_add(c),
            h[3].wrapping_add(d),
            h[4].wrapping_add(e),
        ];
    }
    let mut out = [0u8; 20];
    for (slot, word) in out.chunks_exact_mut(4).zip(h) {
        slot.copy_from_slice(&word.to_be_bytes());
    }
    out
}

fn base64(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in data.chunks(3) {
        let n = c
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for k in 0..4 {
            if k <= c.len() {
                s.push(
                    A.get(((n >> (18 - 6 * k)) & 63) as usize)
                        .copied()
                        .map_or('=', char::from),
                );
            } else {
                s.push('=');
            }
        }
    }
    s
}

/// The `Sec-WebSocket-Accept` value for a client's `Sec-WebSocket-Key`.
pub fn accept_key(key: &str) -> String {
    base64(&sha1(
        format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes(),
    ))
}

// ---- WebSocket frames ----------------------------------------------------------------------------------------

/// A message from the peer.
#[derive(Debug, PartialEq, Eq)]
pub enum Msg {
    Text(String),
    Binary(Vec<u8>),
    Ping(Vec<u8>),
}

/// Try to take one frame from the front of `buf`. `Ok(None)`: not complete yet. The error is a close code.
pub fn parse_frame(buf: &[u8]) -> Result<Option<(u8, Vec<u8>, usize)>, u16> {
    let (Some(&b0), Some(&b1)) = (buf.first(), buf.get(1)) else {
        return Ok(None);
    };
    if b0 & 0x70 != 0 || b0 & 0x80 == 0 || b0 & 0x0f == 0 {
        return Err(1002); // reserved bits, or a fragmented message
    }
    if b1 & 0x80 == 0 {
        return Err(1002); // clients must mask
    }
    let (len, mut at) = match b1 & 0x7f {
        126 => match buf.get(2..4) {
            Some(l) => (
                usize::from(u16::from_be_bytes([
                    l.first().copied().unwrap_or(0),
                    l.get(1).copied().unwrap_or(0),
                ])),
                4,
            ),
            None => return Ok(None),
        },
        127 => match buf.get(2..10) {
            Some(l) => {
                let mut a = [0u8; 8];
                a.copy_from_slice(l);
                (
                    usize::try_from(u64::from_be_bytes(a)).unwrap_or(usize::MAX),
                    10,
                )
            }
            None => return Ok(None),
        },
        n => (usize::from(n), 2),
    };
    if len > MAX_FRAME {
        return Err(1009);
    }
    let Some(mask) = buf.get(at..at + 4) else {
        return Ok(None);
    };
    at += 4;
    let Some(payload) = buf.get(at..at + len) else {
        return Ok(None);
    };
    let unmasked = payload
        .iter()
        .enumerate()
        .map(|(i, b)| b ^ mask.get(i % 4).copied().unwrap_or(0))
        .collect();
    Ok(Some((b0 & 0x0f, unmasked, at + len)))
}

/// A server frame (never masked).
pub fn encode_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut f = vec![0x80 | opcode];
    match payload.len() {
        n if n < 126 => f.push(n as u8),
        n if n < 65536 => {
            f.push(126);
            f.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            f.push(127);
            f.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    f.extend_from_slice(payload);
    f
}

/// One side of an upgraded connection.
pub struct Ws {
    rd: ReadHalf<TcpStream>,
    wr: WriteHalf<TcpStream>,
    buf: Vec<u8>,
}

impl Ws {
    /// The next message, or `None` when the peer closed or broke the protocol. Safe to use inside `select!`.
    pub async fn recv(&mut self) -> Option<Msg> {
        loop {
            match parse_frame(&self.buf) {
                Err(code) => {
                    let _ = self
                        .wr
                        .write_all(&encode_frame(8, &code.to_be_bytes()))
                        .await;
                    return None;
                }
                Ok(Some((op, payload, used))) => {
                    self.buf.drain(..used);
                    match op {
                        1 => return String::from_utf8(payload).ok().map(Msg::Text),
                        2 => return Some(Msg::Binary(payload)),
                        9 => return Some(Msg::Ping(payload)),
                        8 => {
                            let _ = self.wr.write_all(&encode_frame(8, &payload)).await;
                            return None;
                        }
                        _ => continue, // a pong, or an opcode we do not use
                    }
                }
                Ok(None) => {
                    let mut tmp = [0u8; 4096];
                    match self.rd.read(&mut tmp).await {
                        Ok(0) | Err(_) => return None,
                        Ok(n) => self.buf.extend_from_slice(tmp.get(..n)?),
                    }
                }
            }
        }
    }

    pub async fn send_text(&mut self, text: &str) -> std::io::Result<()> {
        self.wr.write_all(&encode_frame(1, text.as_bytes())).await
    }

    pub async fn pong(&mut self, payload: &[u8]) -> std::io::Result<()> {
        self.wr.write_all(&encode_frame(10, payload)).await
    }
}

// ---- connections ---------------------------------------------------------------------------------------------

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        501 => "Not Implemented",
        _ => "Error",
    }
}

async fn write_response(s: &mut TcpStream, r: &Response) {
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        r.status,
        reason(r.status),
        r.content_type,
        r.body.len()
    );
    let _ = s.write_all(head.as_bytes()).await;
    let _ = s.write_all(&r.body).await;
    let _ = s.shutdown().await;
}

fn error_response(e: HttpError) -> Response {
    Response {
        status: e.0,
        content_type: "application/json",
        body: format!("{{\"error\":{:?}}}", e.1).into_bytes(),
    }
}

async fn read_request(s: &mut TcpStream) -> Result<(Request, Vec<u8>), HttpError> {
    let mut buf: Vec<u8> = Vec::new();
    let head_end = loop {
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p;
        }
        if buf.len() > MAX_HEAD {
            return Err(HttpError(400, "request head too large"));
        }
        let mut tmp = [0u8; 2048];
        match s.read(&mut tmp).await {
            Ok(0) | Err(_) => return Err(HttpError(400, "connection closed")),
            Ok(n) => buf.extend_from_slice(tmp.get(..n).unwrap_or(&[])),
        }
    };
    let head = parse_head(buf.get(..head_end).unwrap_or(&[]))?;
    let mut body = buf.get(head_end + 4..).unwrap_or(&[]).to_vec();
    while body.len() < head.content_length {
        let mut tmp = [0u8; 4096];
        match s.read(&mut tmp).await {
            Ok(0) | Err(_) => return Err(HttpError(400, "body ended early")),
            Ok(n) => body.extend_from_slice(tmp.get(..n).unwrap_or(&[])),
        }
    }
    body.truncate(head.content_length);
    let (path, query) = split_target(&head.target);
    Ok((
        Request {
            method: head.method,
            path,
            query,
            headers: head.headers,
            body,
        },
        Vec::new(),
    ))
}

async fn connection<S, F, Fut>(mut s: TcpStream, state: Arc<S>, route: Arc<F>)
where
    S: Send + Sync + 'static,
    F: Fn(Arc<S>, Request) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Reply> + Send + 'static,
{
    let read = tokio::time::timeout(READ_DEADLINE, read_request(&mut s)).await;
    let req = match read {
        Ok(Ok((req, _))) => req,
        Ok(Err(e)) => return write_response(&mut s, &error_response(e)).await,
        Err(_) => return,
    };
    let key = req.header("sec-websocket-key").map(str::to_string);
    let wants_ws = req
        .header("upgrade")
        .is_some_and(|u| u.eq_ignore_ascii_case("websocket"));
    match route(state, req).await {
        Reply::Http(r) => write_response(&mut s, &r).await,
        Reply::Upgrade(run) => {
            let Some(key) = key.filter(|_| wants_ws) else {
                return write_response(
                    &mut s,
                    &error_response(HttpError(400, "expected a WebSocket upgrade")),
                )
                .await;
            };
            let accept = accept_key(&key);
            let head = format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
            );
            if s.write_all(head.as_bytes()).await.is_err() {
                return;
            }
            let (rd, wr) = tokio::io::split(s);
            run(Ws {
                rd,
                wr,
                buf: Vec::new(),
            })
            .await;
        }
    }
}

/// Accept connections until Ctrl-C. `route` answers each parsed request.
pub async fn serve<S, F, Fut>(listener: TcpListener, state: Arc<S>, route: F) -> std::io::Result<()>
where
    S: Send + Sync + 'static,
    F: Fn(Arc<S>, Request) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Reply> + Send + 'static,
{
    let route = Arc::new(route);
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let stop = tokio::signal::ctrl_c();
    tokio::pin!(stop);
    loop {
        tokio::select! {
            _ = &mut stop => return Ok(()),
            accepted = listener.accept() => {
                let (s, _) = accepted?;
                let Ok(permit) = Arc::clone(&slots).try_acquire_owned() else { continue };
                let (state, route) = (Arc::clone(&state), Arc::clone(&route));
                tokio::spawn(async move {
                    connection(s, state, route).await;
                    drop(permit);
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn sha1_and_the_websocket_accept_key_match_the_rfcs() {
        assert_eq!(
            hex(&sha1(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(hex(&sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        // two blocks
        assert_eq!(
            hex(&sha1(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
        // RFC 6455, section 1.3
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn base64_pads_correctly() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn heads_parse_and_bad_ones_are_refused() {
        let h = parse_head(b"POST /api/control?x=1 HTTP/1.1\r\nHost: a\r\nContent-Length: 12\r\nAuthorization: Bearer t").unwrap();
        assert_eq!(
            (h.method.as_str(), h.target.as_str(), h.content_length),
            ("POST", "/api/control?x=1", 12)
        );
        assert!(h
            .headers
            .contains(&("authorization".into(), "Bearer t".into())));
        let bad = |s: &str| parse_head(s.as_bytes()).unwrap_err().0;
        assert_eq!(bad("GET / HTTP/2\r\n"), 400);
        assert_eq!(bad("get / HTTP/1.1\r\n"), 400);
        assert_eq!(bad("GET nope HTTP/1.1\r\n"), 400);
        assert_eq!(bad("GET / HTTP/1.1 extra\r\n"), 400);
        assert_eq!(bad("GET / HTTP/1.1\r\nno colon\r\n"), 400);
        assert_eq!(
            bad("GET / HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 2\r\n"),
            400
        );
        assert_eq!(bad("GET / HTTP/1.1\r\nContent-Length: nine\r\n"), 400);
        assert_eq!(
            bad(&format!(
                "POST / HTTP/1.1\r\nContent-Length: {}\r\n",
                MAX_BODY + 1
            )),
            413
        );
        assert_eq!(
            bad("POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n"),
            501
        );
        assert!(parse_head(&[0xff, 0xfe]).is_err());
        assert!(parse_head(b"").is_err());
    }

    #[test]
    fn targets_decode_path_and_query() {
        assert_eq!(percent_decode("a%20b%2Fc", false), "a b/c");
        assert_eq!(percent_decode("a+b", true), "a b");
        assert_eq!(percent_decode("a+b", false), "a+b");
        assert_eq!(
            percent_decode("100%", false),
            "100%",
            "a stray percent stays"
        );
        assert_eq!(percent_decode("%zz%4", false), "%zz%4");
        let (p, q) = split_target("/api/stream?device=SSL%20lamp&flag&token=abc");
        assert_eq!(p, "/api/stream");
        assert_eq!(
            q,
            vec![
                ("device".into(), "SSL lamp".into()),
                ("flag".into(), String::new()),
                ("token".into(), "abc".into())
            ]
        );
    }

    fn masked(opcode: u8, payload: &[u8]) -> Vec<u8> {
        let key = [0x37, 0xfa, 0x21, 0x3d];
        let mut f = vec![0x80 | opcode];
        match payload.len() {
            n if n < 126 => f.push(0x80 | n as u8),
            n if n < 65536 => {
                f.push(0x80 | 126);
                f.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                f.push(0x80 | 127);
                f.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        f.extend_from_slice(&key);
        f.extend(payload.iter().enumerate().map(|(i, b)| b ^ key[i % 4]));
        f
    }

    #[test]
    fn frames_round_trip_at_every_length_boundary() {
        for n in [0usize, 1, 125, 126, 127, 65535] {
            let payload: Vec<u8> = (0..n).map(|i| (i % 251) as u8).collect();
            if n <= MAX_FRAME {
                let f = masked(2, &payload);
                let (op, got, used) = parse_frame(&f).unwrap().unwrap();
                assert_eq!((op, used, got), (2, f.len(), payload.clone()), "{n}");
                // a cut-off frame is "not yet", never an error or a panic
                for cut in [0, 1, 2, f.len() / 2] {
                    assert_eq!(
                        parse_frame(f.get(..cut.min(f.len().saturating_sub(1))).unwrap()),
                        Ok(None)
                    );
                }
            }
            // server frames are the same layout without a mask
            let e = encode_frame(1, &payload);
            assert_eq!(e.first(), Some(&0x81));
            assert!(e.ends_with(&payload));
        }
    }

    #[test]
    fn protocol_breaches_become_close_codes() {
        assert_eq!(
            parse_frame(&[0x01, 0x80, 0, 0, 0, 0]),
            Err(1002),
            "fragmented"
        );
        assert_eq!(
            parse_frame(&[0xc1, 0x80, 0, 0, 0, 0]),
            Err(1002),
            "reserved bit"
        );
        assert_eq!(parse_frame(&[0x81, 0x01, b'x']), Err(1002), "unmasked");
        assert_eq!(
            parse_frame(&[0x82, 0xff, 0, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0, 0]),
            Err(1009),
            "too big"
        );
    }

    async fn roundtrip(request: &[u8]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(serve(
            listener,
            Arc::new(()),
            |_, req: Request| async move {
                match req.path.as_str() {
                    "/echo" => Reply::Http(Response {
                        status: 200,
                        content_type: "application/json",
                        body: format!(
                            "{} {} {:?} {}",
                            req.method,
                            req.path,
                            req.query("q"),
                            req.body.len()
                        )
                        .into_bytes(),
                    }),
                    "/ws" => Reply::Upgrade(Box::new(|mut ws: Ws| {
                        Box::pin(async move {
                            while let Some(m) = ws.recv().await {
                                if let Msg::Text(t) = m {
                                    let _ = ws.send_text(&format!("echo {t}")).await;
                                }
                            }
                        })
                    })),
                    _ => Reply::Http(error_response(HttpError(404, "not found"))),
                }
            },
        ));
        let mut c = TcpStream::connect(addr).await.unwrap();
        c.write_all(request).await.unwrap();
        let mut out = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(2), c.read_to_end(&mut out)).await;
        String::from_utf8_lossy(&out).into_owned()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_post_with_a_body_and_a_query_is_routed() {
        let r =
            roundtrip(b"POST /echo?q=a%20b HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\n\r\nhello")
                .await;
        assert!(r.starts_with("HTTP/1.1 200 OK\r\n"), "{r}");
        assert!(r.contains("Connection: close"));
        assert!(r.ends_with("POST /echo Some(\"a b\") 5"), "{r}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn bad_requests_get_an_error_not_a_hang() {
        assert!(roundtrip(b"nonsense\r\n\r\n")
            .await
            .starts_with("HTTP/1.1 400"));
        assert!(roundtrip(b"GET /missing HTTP/1.1\r\n\r\n")
            .await
            .starts_with("HTTP/1.1 404"));
        assert!(
            roundtrip(b"POST /echo HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n")
                .await
                .starts_with("HTTP/1.1 501")
        );
        // an upgrade route asked without upgrade headers
        assert!(roundtrip(b"GET /ws HTTP/1.1\r\n\r\n")
            .await
            .starts_with("HTTP/1.1 400"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_websocket_handshakes_and_echoes() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(serve(
            listener,
            Arc::new(()),
            |_, _req: Request| async move {
                Reply::Upgrade(Box::new(|mut ws: Ws| {
                    Box::pin(async move {
                        while let Some(m) = ws.recv().await {
                            if let Msg::Text(t) = m {
                                let _ = ws.send_text(&format!("echo {t}")).await;
                            }
                        }
                    })
                }))
            },
        ));
        let mut c = TcpStream::connect(addr).await.unwrap();
        c.write_all(b"GET /ws HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n").await.unwrap();
        let mut buf = [0u8; 512];
        let n = c.read(&mut buf).await.unwrap();
        let head = String::from_utf8_lossy(buf.get(..n).unwrap()).into_owned();
        assert!(head.starts_with("HTTP/1.1 101"), "{head}");
        assert!(head.contains("s3pPLMBiTxaQ9kYGzzhZRbK+xOo="), "{head}");
        c.write_all(&masked(1, b"hi")).await.unwrap();
        let n = c.read(&mut buf).await.unwrap();
        assert_eq!(
            buf.get(..n).unwrap(),
            encode_frame(1, b"echo hi").as_slice()
        );
        // a close is answered, and the connection ends
        c.write_all(&masked(8, &[0x03, 0xe8])).await.unwrap();
        let n = c.read(&mut buf).await.unwrap();
        assert_eq!(buf.first(), Some(&0x88));
        assert!(n >= 2);
    }
}
