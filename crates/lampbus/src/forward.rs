//! Handing the lamp to an external controller.
//!
//! With `"controller": "host:port"` set, lampbus no longer decides what a lamp shows. Every control command that reaches
//! a door (the CLI, the HTTP API, MCP, Matter, schedules) is sent to the controller as a few lines of text, and the lamp
//! shows what the controller streams back over the UDP input. The controller answers each message with `ok` or
//! `err <why>`, so a controller that is not running is an error the caller sees, not a command that silently vanishes.
//!
//! The controller's own calls carry `"motor": true` and are executed here, which is how it turns the lamp on and off and
//! plays an effect it was asked for. `lampbus direct on` (or `LAMPBUS_DIRECT=1`) is a switch for a person: commands are
//! executed here again and the controller is bypassed.
//!
//! Messages, one per line: `person look R G B [PERCENT]`, `person brightness PERCENT`, `person power on|off|toggle`, and
//! `person act JSON` for anything that is not a plain light (an effect, an animation, a raw command), carrying the whole
//! control object.

use lampbus_core::Command;
use lampbus_render::color::hsv_to_rgb;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::time::timeout;

struct Inner {
    addr: Option<String>,
    direct: AtomicBool,
}

/// Whether, and where, commands are forwarded.
#[derive(Clone)]
pub struct Forward(Arc<Inner>);

impl Forward {
    /// `addr` is the controller's UDP address, if any; `direct` starts with the controller bypassed.
    pub fn new(addr: Option<String>, direct: bool) -> Forward {
        Forward(Arc::new(Inner {
            addr,
            direct: AtomicBool::new(direct),
        }))
    }

    /// Whether commands are being forwarded right now.
    pub fn active(&self) -> bool {
        self.0.addr.is_some() && !self.0.direct.load(Ordering::Relaxed)
    }

    /// Bypass the controller (`true`) or hand control back to it (`false`).
    pub fn set_direct(&self, on: bool) {
        self.0.direct.store(on, Ordering::Relaxed);
    }

    /// For `GET /api/direct` and `lampbus direct`.
    pub fn status(&self) -> Value {
        json!({
            "controller": self.0.addr,
            "direct": self.0.direct.load(Ordering::Relaxed),
            "forwarding": self.active(),
        })
    }

    /// Send `lines` to the controller and wait for its answer.
    pub async fn send(&self, lines: &[String]) -> Result<(), String> {
        let addr = self.0.addr.clone().ok_or("no controller is configured")?;
        let unreachable = |why: &str| {
            format!(
                "the controller at {addr} {why}; it owns the lamp, so nothing was changed. Start it, or run `lampbus direct on` to control the lamp yourself"
            )
        };
        let sock = UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(|e| e.to_string())?;
        sock.connect(&addr)
            .await
            .map_err(|e| format!("controller address {addr}: {e}"))?;
        sock.send(lines.join("\n").as_bytes())
            .await
            .map_err(|_| unreachable("could not be reached"))?;
        let mut buf = [0u8; 512];
        match timeout(Duration::from_millis(800), sock.recv(&mut buf)).await {
            Ok(Ok(n)) => {
                let reply = String::from_utf8_lossy(buf.get(..n).unwrap_or_default())
                    .trim()
                    .to_string();
                match reply.strip_prefix("err") {
                    _ if reply == "ok" => Ok(()),
                    Some(why) => Err(why.trim().to_string()),
                    None => Err(format!(
                        "the controller at {addr} sent an unexpected reply: {reply:?}"
                    )),
                }
            }
            Ok(Err(_)) => Err(unreachable("refused the message")),
            Err(_) => Err(unreachable("did not answer")),
        }
    }
}

/// The messages for a parsed control object. A plain light becomes one line per command; anything else (an effect, an
/// animation, a raw command) is handed over whole as `person act`, for the controller to relay to the motor layer.
pub fn lines(args: &Value, cmds: &[Command]) -> Vec<String> {
    let plain = cmds.iter().all(|c| {
        matches!(
            c,
            Command::Color { .. }
                | Command::Hs { .. }
                | Command::Brightness(_)
                | Command::Power(_)
                | Command::Label(_)
        )
    });
    if !plain {
        let mut a = args.clone();
        if let Some(o) = a.as_object_mut() {
            o.remove("motor");
        }
        return vec![format!("person act {a}")];
    }
    let level = |b: &Option<f64>| b.map_or(String::new(), |v| format!(" {v:.1}"));
    cmds.iter()
        .filter_map(|c| match c {
            Command::Color { rgb, brightness } => Some(format!(
                "person look {:.0} {:.0} {:.0}{}",
                rgb[0],
                rgb[1],
                rgb[2],
                level(brightness)
            )),
            Command::Hs {
                hue,
                sat,
                brightness,
            } => {
                let [r, g, b] = hsv_to_rgb(hue.rem_euclid(360.0) / 360.0, sat / 100.0, 1.0);
                Some(format!(
                    "person look {:.0} {:.0} {:.0}{}",
                    r * 255.0,
                    g * 255.0,
                    b * 255.0,
                    level(brightness)
                ))
            }
            Command::Brightness(b) => Some(format!("person brightness {b:.1}")),
            Command::Power(Some(true)) => Some("person power on".into()),
            Command::Power(Some(false)) => Some("person power off".into()),
            Command::Power(None) => Some("person power toggle".into()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_lights_become_one_line_each() {
        let cmds = [
            Command::Color {
                rgb: [255.0, 136.0, 0.0],
                brightness: Some(40.0),
            },
            Command::Power(Some(true)),
        ];
        assert_eq!(
            lines(&json!({}), &cmds),
            vec!["person look 255 136 0 40.0", "person power on"]
        );
        assert_eq!(
            lines(&json!({}), &[Command::Brightness(55.0)]),
            vec!["person brightness 55.0"]
        );
        assert_eq!(
            lines(&json!({}), &[Command::Power(None)]),
            vec!["person power toggle"]
        );
        let hs = lines(
            &json!({}),
            &[Command::Hs {
                hue: 120.0,
                sat: 100.0,
                brightness: None,
            }],
        );
        assert_eq!(hs, vec!["person look 0 255 0"]);
    }

    #[test]
    fn anything_else_is_handed_over_whole() {
        let args = json!({"effect": "rainbow", "speed": 50, "motor": false});
        let cmds = [Command::Effect {
            effect: "rainbow".into(),
            speed: Some(50.0),
            brightness: None,
        }];
        let l = lines(&args, &cmds);
        assert_eq!(l.len(), 1);
        assert!(
            l[0].starts_with("person act {") && l[0].contains("rainbow") && !l[0].contains("motor")
        );
    }

    async fn controller(reply: &'static str) -> String {
        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = sock.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let mut buf = [0u8; 512];
            if let Ok((_, from)) = sock.recv_from(&mut buf).await {
                let _ = sock.send_to(reply.as_bytes(), from).await;
            }
        });
        addr
    }

    #[tokio::test]
    async fn an_answering_controller_is_ok_and_a_complaining_one_is_an_error() {
        let ok = Forward::new(Some(controller("ok").await), false);
        assert!(ok.send(&["person power on".into()]).await.is_ok());
        let err = Forward::new(Some(controller("err asleep").await), false);
        assert_eq!(
            err.send(&["person power on".into()]).await,
            Err("asleep".to_string())
        );
    }

    #[tokio::test]
    async fn a_silent_controller_is_a_clear_error_that_names_the_way_out() {
        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = sock.local_addr().unwrap().to_string();
        let f = Forward::new(Some(addr), false);
        let e = f.send(&["person power on".into()]).await.unwrap_err();
        assert!(
            e.contains("did not answer") && e.contains("lampbus direct on"),
            "{e}"
        );
    }

    #[test]
    fn the_switch_is_for_a_person() {
        let f = Forward::new(Some("127.0.0.1:1".into()), false);
        assert!(f.active());
        f.set_direct(true);
        assert!(!f.active());
        assert_eq!(f.status()["direct"], json!(true));
        assert!(
            !Forward::new(None, false).active(),
            "no controller, nothing to forward to"
        );
    }
}
