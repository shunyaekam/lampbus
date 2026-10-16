//! `lampbus-life`: an ambient creature for your lamp.
//!
//! It breathes, drifts through the colours of the day, stirs now and then, and reacts when poked. It drives a running
//! `lampbus serve` over its UDP input (set `"udp_stream": "127.0.0.1:8789"` in the lampbus config). The mathematics is
//! in `life.rs` and is tested without a lamp. An agent, a script or you steer it with small text commands over UDP;
//! the same commands are the `lampbus-life` subcommands.

mod automaton;
mod life;

use life::{poke_colour, Life, Params};
use std::net::UdpSocket;
use std::process::Command;
use std::time::{Duration, Instant};

const HELP: &str = "lampbus-life: an ambient creature for your lamp

RUN
  lampbus-life [--to host:port] [--listen host:port] [--max PERCENT] [--fps N] [--seed N] [--mood NAME]
               [--fine] [--hour H]

STEER (while it runs; works from any shell, script or agent)
  lampbus-life mood <ember|campfire|calm|focus|ocean|night|aurora|forest|dusk|sunset|bloom|conway>   ease into a mood (about 15 s)
  lampbus-life set <name> <number>    breath_period, breath_depth, drift, stir_every, excitability, flicker, sway, automaton, level
  lampbus-life poke [good|info|warn|bad]                      react to something
  lampbus-life sleep | wake                                   stop or resume driving the lamp
  lampbus-life status

  --to host:port      the lampbus UDP input (default 127.0.0.1:8789; enable `udp_stream` in the lampbus config)
  --listen host:port  where it hears commands (default 127.0.0.1:8790)
  --max PERCENT       brightness ceiling (default 40)
  --fps N             frames per second sent to the lamp (default 20, at most 30)
  --fine              send brightness through the colour channels: finer steps, smoother dim light
  --seed N            a different personality
  --no-setup          do not turn the lamp on or set its master brightness (for a service: it takes over when the lamp is on)
  --hour H            pretend it is H o'clock (for example 20.5); default is the real time

It turns the lamp on at the start. `lampbus off` puts it to sleep: while the lamp is off it is ignored, and
turning the lamp on wakes it. Commands are plain text datagrams, for example: echo 'mood campfire' | nc -u -w0 127.0.0.1 8790";

struct Opts {
    to: String,
    listen: String,
    max: f64,
    fps: f64,
    seed: u64,
    hour: Option<f64>,
    fine: bool,
    mood: String,
    setup: bool,
}

fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        to: "127.0.0.1:8789".into(),
        listen: "127.0.0.1:8790".into(),
        max: 40.0,
        fps: 20.0,
        seed: 1,
        hour: None,
        fine: false,
        mood: "ember".into(),
        setup: true,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |what: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{what} needs a value"))
        };
        match a.as_str() {
            "--to" => o.to = value("--to")?,
            "--listen" => o.listen = value("--listen")?,
            "--max" => {
                o.max = value("--max")?
                    .parse()
                    .map_err(|_| "--max needs a number")?
            }
            "--fps" => {
                o.fps = value("--fps")?
                    .parse::<f64>()
                    .map_err(|_| "--fps needs a number")?
                    .clamp(1.0, 30.0)
            }
            "--seed" => {
                o.seed = value("--seed")?
                    .parse()
                    .map_err(|_| "--seed needs a whole number")?
            }
            "--hour" => {
                o.hour = Some(
                    value("--hour")?
                        .parse()
                        .map_err(|_| "--hour needs a number")?,
                )
            }
            "--mood" => o.mood = value("--mood")?,
            "--fine" => o.fine = true,
            "--no-setup" => o.setup = false,
            other => return Err(format!("unknown option {other:?}\n\n{HELP}")),
        }
    }
    if Params::mood(&o.mood).is_none() {
        return Err(format!("unknown mood {:?}", o.mood));
    }
    Ok(o)
}

/// Local time as hours since midnight, from the operating system's `date` (correct across time zones and DST).
fn local_hour() -> Option<f64> {
    let out = Command::new("date").arg("+%H %M %S").output().ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    let mut n = text.split_whitespace().map(|x| x.parse::<f64>().ok());
    Some(n.next()?? + n.next()?? / 60.0 + n.next()?? / 3600.0)
}

/// What the creature is doing, besides the maths.
struct State {
    life: Life,
    mood: String,
    asleep: bool,
}

/// Apply one text command. Returns the reply for `status` and for mistakes.
fn command(state: &mut State, t: f64, line: &str) -> Option<String> {
    let mut w = line.split_whitespace();
    match (w.next()?, w.next(), w.next()) {
        ("poke", name, _) => {
            state.life.poke(t, poke_colour(name.unwrap_or("info")));
            None
        }
        ("mood", Some(name), _) => match Params::mood(name) {
            Some(p) => {
                *state.life.target_mut() = p;
                state.mood = name.to_string();
                None
            }
            None => Some(format!(
                "error: unknown mood {name:?}; try ember campfire calm focus ocean night aurora forest dusk sunset bloom conway"
            )),
        },
        ("set", Some(key), Some(v)) => {
            let Ok(n) = v.parse::<f64>() else {
                return Some(format!("error: {v:?} is not a number"));
            };
            if state.life.target_mut().set(key, n) {
                None
            } else {
                Some(format!("error: unknown setting {key:?}"))
            }
        }
        ("sleep", ..) => {
            state.asleep = true;
            None
        }
        ("wake", ..) => {
            state.asleep = false;
            None
        }
        ("status", ..) => Some(format!("mood={} asleep={}", state.mood, state.asleep)),
        (other, ..) => Some(format!("error: unknown command {other:?}")),
    }
}

/// `lampbus-life <command...>`: send a command to a running creature.
fn send(args: &[String]) -> i32 {
    let (to, rest): (String, Vec<&String>) = match args.iter().position(|a| a == "--listen") {
        Some(i) => (
            args.get(i + 1).cloned().unwrap_or_default(),
            args.iter()
                .enumerate()
                .filter(|(k, _)| *k != i && *k != i + 1)
                .map(|(_, a)| a)
                .collect(),
        ),
        None => ("127.0.0.1:8790".into(), args.iter().collect()),
    };
    let line = rest
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let Ok(sock) = UdpSocket::bind("127.0.0.1:0") else {
        return 1;
    };
    if let Err(e) = sock.send_to(line.as_bytes(), &to) {
        eprintln!("lampbus-life: cannot reach {to}: {e}");
        return 1;
    }
    let _ = sock.set_read_timeout(Some(Duration::from_millis(700)));
    let mut buf = [0u8; 256];
    if let Ok(n) = sock.recv(&mut buf) {
        println!("{}", String::from_utf8_lossy(buf.get(..n).unwrap_or(&[])));
    } else if line.starts_with("status") {
        eprintln!("lampbus-life: no answer (is it running?)");
        return 1;
    }
    0
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{HELP}");
        return;
    }
    if matches!(
        args.first().map(String::as_str),
        Some("poke" | "mood" | "set" | "sleep" | "wake" | "status")
    ) {
        std::process::exit(send(&args));
    }
    let o = match parse(&args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("lampbus-life: {e}");
            std::process::exit(2);
        }
    };
    let Ok(out) = UdpSocket::bind("127.0.0.1:0") else {
        eprintln!("lampbus-life: cannot open a socket");
        std::process::exit(1);
    };
    let hear = UdpSocket::bind(&o.listen).ok().inspect(|s| {
        let _ = s.set_nonblocking(true);
    });
    if hear.is_none() {
        eprintln!(
            "lampbus-life: cannot listen on {} (another one running?); steering is off",
            o.listen
        );
    }
    // the lamp's master brightness is full so the ceiling here is the ceiling, and it is on (streams are ignored while off)
    if o.setup {
        let _ = Command::new("lampbus").args(["brightness", "100"]).status();
        let _ = Command::new("lampbus").arg("on").status();
    }
    println!(
        "lampbus-life: alive, mood {}, ceiling {}%, listening on {}",
        o.mood, o.max, o.listen
    );

    let mut state = State {
        life: Life::new(o.seed),
        mood: o.mood.clone(),
        asleep: false,
    };
    if let Some(p) = Params::mood(&o.mood) {
        *state.life.target_mut() = p;
    }
    let start = Instant::now();
    let mut hour = o.hour.or_else(local_hour).unwrap_or(20.0);
    let mut hour_read = 0.0;
    let frame_time = Duration::from_secs_f64(1.0 / o.fps);
    let mut buf = [0u8; 256];
    loop {
        let t = start.elapsed().as_secs_f64();
        if t - hour_read > 30.0 {
            hour = o.hour.or_else(local_hour).unwrap_or(hour);
            hour_read = t;
        }
        if let Some(s) = &hear {
            while let Ok((n, from)) = s.recv_from(&mut buf) {
                let text = String::from_utf8_lossy(buf.get(..n).unwrap_or(&[])).into_owned();
                if let Some(reply) = command(&mut state, t, text.trim()) {
                    let _ = s.send_to(reply.as_bytes(), from);
                }
            }
        }
        let mut f = state.life.step(t, hour + (t - hour_read) / 3600.0, o.max);
        if o.fine {
            f = f.fine();
        }
        if !state.asleep {
            let _ = out.send_to(&[f.rgb[0], f.rgb[1], f.rgb[2], f.level], &o.to);
        }
        std::thread::sleep(frame_time);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> State {
        State {
            life: Life::new(1),
            mood: "ember".into(),
            asleep: false,
        }
    }

    #[test]
    fn commands_steer_and_mistakes_are_answered() {
        let mut s = state();
        assert_eq!(command(&mut s, 0.0, "mood campfire"), None);
        assert_eq!(s.mood, "campfire");
        assert!(command(&mut s, 0.0, "mood nope")
            .unwrap()
            .starts_with("error"));
        assert_eq!(command(&mut s, 0.0, "set flicker 0.3"), None);
        assert!(command(&mut s, 0.0, "set flicker lots")
            .unwrap()
            .starts_with("error"));
        assert!(command(&mut s, 0.0, "set mystery 1")
            .unwrap()
            .starts_with("error"));
        assert_eq!(command(&mut s, 0.0, "poke good"), None);
        assert_eq!(command(&mut s, 0.0, "sleep"), None);
        assert!(s.asleep);
        assert_eq!(command(&mut s, 0.0, "wake"), None);
        assert!(!s.asleep);
        assert_eq!(
            command(&mut s, 0.0, "status").unwrap(),
            "mood=campfire asleep=false"
        );
        assert!(command(&mut s, 0.0, "dance").unwrap().starts_with("error"));
        assert_eq!(command(&mut s, 0.0, ""), None);
    }

    #[test]
    fn options_parse_and_bad_moods_are_refused() {
        let a = |s: &str| parse(&s.split_whitespace().map(String::from).collect::<Vec<_>>());
        let o = a("--mood campfire --fine --fps 99 --max 25").unwrap();
        assert!(o.fine && (o.fps - 30.0).abs() < 1e-9 && (o.max - 25.0).abs() < 1e-9);
        assert!(a("--mood nope").is_err());
        assert!(a("--wat").is_err());
        assert!(a("--fps").is_err());
    }
}
