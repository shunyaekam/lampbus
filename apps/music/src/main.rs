//! `lampbus-music`: music-reactive lighting. Listens to an audio input, reduces it to a colour and an intensity
//! (see `dsp.rs`), and sends 4 bytes over UDP to a running `lampbus serve` with `udp_stream` enabled.
//!
//! The input is a microphone by default. To react to what the computer is playing, route its output to a loopback
//! input (BlackHole on macOS, a monitor source on Linux) and pick it with `--device`.

mod dsp;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat};
use dsp::{Analyzer, WINDOW};
use std::collections::VecDeque;
use std::net::UdpSocket;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const HELP: &str = "lampbus-music: music-reactive lighting

USAGE  lampbus-music [--to host:port] [--device ID] [--list] [--fps N]

  --to host:port   where `lampbus serve` listens for UDP (default 127.0.0.1:8789;
                   enable it with \"udp_stream\": \"127.0.0.1:8789\" in the lampbus config)
  --device ID      audio input to use (see --list); default: the system's default input
  --list           list audio inputs and exit
  --fps N          updates per second, 5-30 (default 20)

The first run may ask for microphone permission for your terminal.";

struct Opts {
    to: String,
    device: Option<String>,
    list: bool,
    fps: f64,
}

fn parse(args: Vec<String>) -> Result<Option<Opts>, String> {
    let mut o = Opts {
        to: "127.0.0.1:8789".into(),
        device: None,
        list: false,
        fps: 20.0,
    };
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => return Ok(None),
            "--list" => o.list = true,
            "--to" => o.to = it.next().ok_or("--to needs host:port")?,
            "--device" => o.device = Some(it.next().ok_or("--device needs an id")?),
            "--fps" => {
                o.fps = it
                    .next()
                    .ok_or("--fps needs a number")?
                    .parse::<f64>()
                    .map_err(|_| "--fps must be a number")?
                    .clamp(5.0, 30.0)
            }
            other => return Err(format!("unknown option {other:?}; try --help")),
        }
    }
    Ok(Some(o))
}

type Ring = Arc<Mutex<VecDeque<f32>>>;

fn push<T: Sample>(data: &[T], channels: usize, ring: &Ring)
where
    f32: FromSample<T>,
{
    if let Ok(mut r) = ring.try_lock() {
        for frame in data.chunks(channels.max(1)) {
            // mix all channels down to mono
            let sum: f32 = frame.iter().map(|s| f32::from_sample(*s)).sum();
            r.push_back(sum / frame.len().max(1) as f32);
        }
        while r.len() > WINDOW * 4 {
            r.pop_front();
        }
    }
}

fn run(o: &Opts) -> Result<(), String> {
    let host = cpal::default_host();
    if o.list {
        for d in host.devices().map_err(|e| e.to_string())? {
            if d.supports_input() {
                println!("{}", d.id().map_err(|e| e.to_string())?);
            }
        }
        return Ok(());
    }
    let device = match &o.device {
        Some(id) => host
            .device_by_id(&id.parse().map_err(|_| format!("bad device id {id:?}"))?)
            .ok_or_else(|| format!("no audio device {id:?}; try --list"))?,
        None => host
            .default_input_device()
            .ok_or("no default audio input")?,
    };
    let config = device
        .default_input_config()
        .map_err(|e| format!("cannot read the input configuration: {e}"))?;
    let rate = f64::from(config.sample_rate());
    let channels = usize::from(config.channels());
    eprintln!(
        "lampbus-music: listening on {} ({rate} Hz, {channels} ch) -> {}",
        device.id().map_err(|e| e.to_string())?,
        o.to
    );
    let ring: Ring = Arc::new(Mutex::new(VecDeque::new()));
    let err_fn = |e| eprintln!("lampbus-music: stream error: {e}");
    let (r2, stream_config) = (Arc::clone(&ring), config.config());
    let stream = match config.sample_format() {
        SampleFormat::F32 => device.build_input_stream(
            stream_config,
            move |d: &[f32], _: &_| push(d, channels, &r2),
            err_fn,
            None,
        ),
        SampleFormat::I16 => device.build_input_stream(
            stream_config,
            move |d: &[i16], _: &_| push(d, channels, &r2),
            err_fn,
            None,
        ),
        SampleFormat::I32 => device.build_input_stream(
            stream_config,
            move |d: &[i32], _: &_| push(d, channels, &r2),
            err_fn,
            None,
        ),
        other => return Err(format!("unsupported sample format {other}")),
    }
    .map_err(|e| {
        format!("cannot open the input (is microphone permission granted to this terminal?): {e}")
    })?;
    stream.play().map_err(|e| e.to_string())?;

    let sock = UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
    let mut analyzer = Analyzer::new(rate);
    let period = Duration::from_secs_f64(1.0 / o.fps);
    let mut last = Instant::now();
    loop {
        std::thread::sleep(period);
        let samples: Vec<f32> = match ring.lock() {
            Ok(r) if r.len() >= WINDOW => r.iter().skip(r.len() - WINDOW).copied().collect(),
            _ => continue,
        };
        let dt = last.elapsed().as_secs_f64() * 1000.0;
        last = Instant::now();
        let f = analyzer.analyse(&samples, dt);
        sock.send_to(&[f.rgb[0], f.rgb[1], f.rgb[2], f.level], &o.to)
            .map_err(|e| format!("send to {}: {e}", o.to))?;
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
                eprintln!("lampbus-music: {e}");
                ExitCode::FAILURE
            }
        },
        Err(e) => {
            eprintln!("lampbus-music: {e}");
            ExitCode::FAILURE
        }
    }
}
