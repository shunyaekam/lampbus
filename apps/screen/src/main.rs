//! `lampbus-screen`: ambient lighting from your screen. Captures a display (the main one by default), reduces it to a colour and an
//! intensity (see `analyse.rs`), and sends 4 bytes over UDP to a running `lampbus serve` with `udp_stream` enabled.
//!
//! macOS only; needs Screen Recording permission for the terminal you run it from.

mod analyse;

use analyse::{analyse, Image, Mode, Params};
use std::net::UdpSocket;
use std::process::ExitCode;
use std::time::{Duration, Instant};

const HELP: &str = "lampbus-screen: ambient lighting from your screen (macOS)

USAGE  lampbus-screen [--to host:port] [--fps N] [--mode mean|vivid] [--display N] [--min-brightness N] [--once]

  --to host:port      where `lampbus serve` listens for UDP (default 127.0.0.1:8789;
                      enable it with \"udp_stream\": \"127.0.0.1:8789\" in the lampbus config)
  --fps N             frames per second, 1-30 (default 10)
  --mode mean|vivid   plain average, or favour colourful pixels (default vivid)
  --display N         which screen: 1 is the main display, 2 the next, and so on (default 1)
  --min-brightness N  intensity of a black screen, 0-100 (default 8)
  --creature host:port  tell a running lampbus-life to hold off while this runs (default 127.0.0.1:8790; off disables)
  --once              print one analysis and exit (use this to check permissions)";

struct Opts {
    to: String,
    fps: f64,
    mode: Mode,
    min_level: u8,
    display: usize,
    once: bool,
    creature: Option<String>,
}

fn parse(args: Vec<String>) -> Result<Option<Opts>, String> {
    let mut o = Opts {
        to: "127.0.0.1:8789".into(),
        fps: 10.0,
        mode: Mode::Vivid,
        min_level: 8,
        display: 1,
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
            "--display" => {
                o.display = value("--display")?
                    .parse::<usize>()
                    .ok()
                    .filter(|n| *n >= 1)
                    .ok_or("--display must be 1 or more")?;
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
    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;
    use core_graphics::access::ScreenCaptureAccess;
    use core_graphics::base::{kCGBitmapByteOrder32Little, kCGImageAlphaPremultipliedFirst};
    use core_graphics::color_space::CGColorSpace;
    use core_graphics::context::{CGContext, CGInterpolationQuality};
    use core_graphics::display::CGDisplay;
    use core_graphics::geometry::{CGPoint, CGRect, CGSize};

    /// Width of the picture handed to the analysis: it samples a 64-column grid, so more than this is wasted.
    const WIDTH: usize = 512;

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

    /// Display `n` (1 is the main display, then the others in the order macOS lists them).
    fn pick(n: usize) -> Option<CGDisplay> {
        let main = CGDisplay::main();
        let others = CGDisplay::active_displays()
            .ok()?
            .into_iter()
            .filter(|id| *id != main.id);
        std::iter::once(main.id)
            .chain(others)
            .nth(n - 1)
            .map(CGDisplay::new)
    }

    /// The main display as BGRA, drawn into an sRGB bitmap so macOS does the colour management. The raw capture is in the
    /// display's own space (Display P3 on most Macs), which read as sRGB washes every colour towards white: pure green
    /// arrives as (117, 251, 76).
    pub fn grab(display: usize) -> Option<Shot> {
        let img = pick(display)?.image()?;
        let width = img.width().min(WIDTH);
        let height = (img.height() * width / img.width().max(1)).max(1);
        let name = CFString::new("kCGColorSpaceSRGB");
        let space = CGColorSpace::create_with_name(name.as_concrete_TypeRef())?;
        let mut ctx = CGContext::create_bitmap_context(
            None,
            width,
            height,
            8,
            width * 4,
            &space,
            kCGImageAlphaPremultipliedFirst | kCGBitmapByteOrder32Little,
        );
        ctx.set_interpolation_quality(CGInterpolationQuality::CGInterpolationQualityNone);
        let rect = CGRect::new(
            &CGPoint::new(0.0, 0.0),
            &CGSize::new(width as f64, height as f64),
        );
        ctx.draw_image(rect, &img);
        let stride = ctx.bytes_per_row();
        Some(Shot {
            bytes: ctx.data().to_vec(),
            width,
            height,
            stride,
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
    pub fn grab(_display: usize) -> Option<Shot> {
        None
    }
}

/// How long a still screen may go without a frame being sent (the core waits 2 s before it drops a stream).
const REPEAT: Duration = Duration::from_millis(500);

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
    let mut last_sent = Instant::now();
    let mut last_hold: Option<Instant> = None;
    loop {
        let started = Instant::now();
        let shot = capture::grab(o.display)
            .ok_or_else(|| format!("could not capture display {}", o.display))?;
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
        // an unchanged screen is repeated too: the lamp drops a stream that has been quiet for a couple of seconds and
        // falls back to whatever was under it, which is a lamp that turns white on every still scene
        if last != Some(f) || last_sent.elapsed() >= REPEAT {
            last = Some(f);
            last_sent = Instant::now();
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
