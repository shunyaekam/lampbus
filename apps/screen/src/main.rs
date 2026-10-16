//! `lampbus-screen`: ambient lighting from your screen. Captures the main display, reduces it to a colour and an
//! intensity (see `analyse.rs`), and sends 4 bytes over UDP to a running `lampbus serve` with `udp_stream` enabled.
//!
//! macOS only; needs Screen Recording permission for the terminal you run it from.

mod analyse;

use analyse::{analyse, Image, Mode, Params};
use std::net::UdpSocket;
use std::process::ExitCode;
use std::time::{Duration, Instant};

const HELP: &str = "lampbus-screen: ambient lighting from your screen (macOS)

USAGE  lampbus-screen [--to host:port] [--fps N] [--mode mean|vivid] [--min-brightness N] [--once]

  --to host:port      where `lampbus serve` listens for UDP (default 127.0.0.1:8789;
                      enable it with \"udp_stream\": \"127.0.0.1:8789\" in the lampbus config)
  --fps N             frames per second, 1-30 (default 10)
  --mode mean|vivid   plain average, or favour colourful pixels (default vivid)
  --min-brightness N  intensity of a black screen, 0-100 (default 8)
  --creature host:port  tell a running lampbus-life to hold off while this runs (default 127.0.0.1:8790; off disables)
  --once              print one analysis and exit (use this to check permissions)";

struct Opts {
    to: String,
    fps: f64,
    mode: Mode,
    min_level: u8,
    once: bool,
    creature: Option<String>,
}

fn parse(args: Vec<String>) -> Result<Option<Opts>, String> {
    let mut o = Opts {
        to: "127.0.0.1:8789".into(),
        fps: 10.0,
        mode: Mode::Vivid,
        min_level: 8,
        once: false,
        creature: Some("127.0.0.1:8790".into()),
    };
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match a.as_str() {
            "-h" | "--help" => return Ok(None),
            "--to" => o.to = value("--to")?,
            "--fps" => {
                o.fps = value("--fps")?
                    .parse::<f64>()
                    .map_err(|_| "--fps must be a number")?
                    .clamp(1.0, 30.0)
            }
            "--min-brightness" => {
                o.min_level = value("--min-brightness")?
                    .parse::<u8>()
                    .map_err(|_| "--min-brightness must be 0-100")?
                    .min(100)
            }
            "--mode" => {
                o.mode = match value("--mode")?.as_str() {
                    "mean" => Mode::Mean,
                    "vivid" => Mode::Vivid,
                    other => return Err(format!("unknown mode {other:?}")),
                }
            }
            "--once" => o.once = true,
            "--creature" => {
                let v = value("--creature")?;
                o.creature = (v != "off").then_some(v);
            }
            other => return Err(format!("unknown option {other:?}; try --help")),
        }
    }
    Ok(Some(o))
}

#[cfg(target_os = "macos")]
mod capture {
    use core_graphics::access::ScreenCaptureAccess;
    use core_graphics::display::CGDisplay;

    pub struct Shot {
        pub bytes: Vec<u8>,
        pub width: usize,
        pub height: usize,
        pub stride: usize,
    }

    /// Whether this process may record the screen. Without it macOS returns only the desktop wallpaper.
    pub fn allowed() -> bool {
        ScreenCaptureAccess.preflight()
    }

    pub fn request() -> bool {
        ScreenCaptureAccess.request()
    }

    pub fn grab() -> Option<Shot> {
        let img = CGDisplay::main().image()?;
        if img.bits_per_pixel() != 32 {
            return None;
        }
        let data = img.data();
        Some(Shot {
            bytes: data.bytes().to_vec(),
            width: img.width(),
            height: img.height(),
            stride: img.bytes_per_row(),
        })
    }
}

#[cfg(not(target_os = "macos"))]
mod capture {
    pub struct Shot {
        pub bytes: Vec<u8>,
        pub width: usize,
        pub height: usize,
        pub stride: usize,
    }
    pub fn allowed() -> bool {
        false
    }
    pub fn request() -> bool {
        false
    }
    pub fn grab() -> Option<Shot> {
        None
    }
}

fn run(o: &Opts) -> Result<(), String> {
    if cfg!(not(target_os = "macos")) {
        return Err("lampbus-screen currently supports macOS only".into());
    }
    if !capture::allowed() {
        eprintln!("lampbus-screen: Screen Recording permission is not granted to this terminal.");
        eprintln!("  System Settings > Privacy & Security > Screen Recording: enable your terminal, then run again.");
        if !capture::request() {
            eprintln!("  (continuing: without it only the desktop wallpaper is visible, so colours will look wrong)");
        }
    }
    let sock = UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
    let period = Duration::from_secs_f64(1.0 / o.fps);
    let mut last = None;
    let mut last_hold: Option<Instant> = None;
    loop {
        let started = Instant::now();
        let shot = capture::grab().ok_or("could not capture the display")?;
        // BGRA: the byte order macOS display captures use
        let img = Image {
            px: &shot.bytes,
            width: shot.width,
            height: shot.height,
            stride: shot.stride,
            bgra: true,
        };
        let Some(f) = analyse(
            &img,
            &Params {
                mode: o.mode,
                grid: (64, 36),
                min_level: o.min_level,
            },
        ) else {
            return Err("unexpected image layout".into());
        };
        if o.once {
            println!(
                "screen {}x{}  ->  rgb({},{},{})  intensity {:.1}%",
                shot.width, shot.height, f.rgb[0], f.rgb[1], f.rgb[2], f.level
            );
            return Ok(());
        }
        // the creature holds off for a few seconds each time it hears this, and resumes by itself when it stops
        if let (Some(to), false) = (o.creature.as_deref(), o.once) {
            if !last_hold.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
                let _ = sock.send_to(b"hold 4", to);
                last_hold = Some(Instant::now());
            }
        }
        if last != Some(f) {
            last = Some(f);
            let [hi, lo] = ((f.level * 100.0).round().clamp(0.0, 10_000.0) as u16).to_be_bytes();
            sock.send_to(&[f.rgb[0], f.rgb[1], f.rgb[2], hi, lo], &o.to)
                .map_err(|e| format!("send to {}: {e}", o.to))?;
        }
        if let Some(rest) = period.checked_sub(started.elapsed()) {
            std::thread::sleep(rest);
        }
    }
}

fn main() -> ExitCode {
    match parse(std::env::args().skip(1).collect()) {
        Ok(None) => {
            println!("{HELP}");
            ExitCode::SUCCESS
        }
        Ok(Some(o)) => match run(&o) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("lampbus-screen: {e}");
                ExitCode::FAILURE
            }
        },
        Err(e) => {
            eprintln!("lampbus-screen: {e}");
            ExitCode::FAILURE
        }
    }
}
