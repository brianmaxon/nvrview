//! Command-line mock NVR. The password comes from the `MOCK_NVR_PASSWORD`
//! environment variable (default `example-password`), never from an argument.

use bc_protocol::archive::DateTime;
use mock_nvr::archive::ArchiveConfig;
use mock_nvr::audio::{AudioConfig, AudioFixture};
use mock_nvr::replay::{Replay, ReplayConfig};
use mock_nvr::{Config, fixture::Fixture, spawn};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: mock-nvr [--bind ADDR] [--port N] [--channels N] [--fixture FILE]\n\
    \x20               [--user NAME] [--max-logins N] [--fps N] [--width N] [--height N]\n\
    \x20               [--audio FILE.aac] [--audio-clock HZ] [--audio-jitter-ms N]\n\
    \x20               [--replay FILE.nvrrec] [--replay-loop] [--replay-speed N]\n\
    \x20               [--recordings DAYS] [--aitype BITS] [--detections SECONDS]\n\
    password: environment variable MOCK_NVR_PASSWORD (default example-password)";

fn run() -> Result<(), String> {
    let mut bind: IpAddr = [127, 0, 0, 1].into();
    let mut port = 9000u16;
    let mut fixture = PathBuf::from("fixtures/synthetic/h264-basic.h264");
    let (mut channels, mut max_logins, mut fps) = (4u8, 4usize, 25u32);
    let (mut width, mut height) = (320u32, 240u32);
    let mut user = String::from("example-user");
    let mut audio: Option<PathBuf> = None;
    let (mut audio_clock, mut audio_jitter_ms) = (16_000.0f64, 0u32);
    let mut replay: Option<PathBuf> = None;
    let (mut replay_loop, mut replay_speed) = (false, 1.0f64);
    let mut recording_days = 0u32;
    let mut aitype = 0u32;
    let mut detections: Option<f64> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--help" || a == "-h" {
            println!("{USAGE}");
            return Ok(());
        }
        if a == "--replay-loop" {
            replay_loop = true;
            continue;
        }
        let v = args.next().ok_or(format!("{a} needs a value"))?;
        match a.as_str() {
            "--bind" => bind = v.parse().map_err(|_| format!("bad value for {a}: {v}"))?,
            "--port" => port = v.parse().map_err(|_| format!("bad value for {a}: {v}"))?,
            "--channels" => channels = v.parse().map_err(|_| format!("bad value for {a}: {v}"))?,
            "--fixture" => fixture = PathBuf::from(v),
            "--user" => user = v,
            "--max-logins" => {
                max_logins = v.parse().map_err(|_| format!("bad value for {a}: {v}"))?
            }
            "--fps" => fps = v.parse().map_err(|_| format!("bad value for {a}: {v}"))?,
            "--width" => width = v.parse().map_err(|_| format!("bad value for {a}: {v}"))?,
            "--height" => height = v.parse().map_err(|_| format!("bad value for {a}: {v}"))?,
            "--audio" => audio = Some(PathBuf::from(v)),
            "--audio-clock" => {
                audio_clock = v.parse().map_err(|_| format!("bad value for {a}: {v}"))?
            }
            "--audio-jitter-ms" => {
                audio_jitter_ms = v.parse().map_err(|_| format!("bad value for {a}: {v}"))?
            }
            "--replay" => replay = Some(PathBuf::from(v)),
            "--recordings" => {
                recording_days = v.parse().map_err(|_| format!("bad value for {a}: {v}"))?
            }
            "--aitype" => aitype = v.parse().map_err(|_| format!("bad value for {a}: {v}"))?,
            "--detections" => {
                detections = Some(v.parse().map_err(|_| format!("bad value for {a}: {v}"))?)
            }
            "--replay-speed" => {
                replay_speed = v.parse().map_err(|_| format!("bad value for {a}: {v}"))?
            }
            _ => return Err(format!("unknown argument {a}\n{USAGE}")),
        }
    }
    if !(replay_speed.is_finite() && replay_speed > 0.0) {
        return Err("--replay-speed must be positive".into());
    }
    let fix = Fixture::load(&fixture).map_err(|e| e.to_string())?;
    let mut cfg = Config::new(fix);
    cfg.bind = SocketAddr::new(bind, port);
    cfg.channels = channels;
    cfg.max_logins = max_logins;
    cfg.fps = fps;
    cfg.width = width;
    cfg.height = height;
    cfg.user = user;
    if let Some(path) = audio {
        let mut a = AudioConfig::new(AudioFixture::load(&path).map_err(|e| e.to_string())?);
        a.clock_hz = audio_clock;
        a.jitter_us = audio_jitter_ms.saturating_mul(1000);
        cfg.audio = Some(std::sync::Arc::new(a));
    }
    if let Some(path) = replay {
        let rep = Replay::load(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if let Some((w, h, f)) = rep.info() {
            // The recording's own geometry wins over the defaults.
            (cfg.width, cfg.height, cfg.fps) = (w, h, u32::from(f));
        }
        let mut rc = ReplayConfig::new(rep);
        rc.speed = replay_speed;
        rc.looped = replay_loop;
        cfg.replay = Some(rc);
    }
    if recording_days > 0 {
        // Recordings on the last N days by this computer's UTC date (the
        // mock has no time zone; device times are wall-clock times).
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        let days = (0..i64::from(recording_days))
            .filter_map(|i| DateTime::from_naive_secs(now - i * 86_400))
            .collect();
        cfg.archive = Some(ArchiveConfig::new(days));
    }
    // Every channel advertises the same capabilities.
    cfg.aitypes = vec![aitype; usize::from(channels)];
    if let Some(secs) = detections {
        if !(secs.is_finite() && secs > 0.0) {
            return Err("--detections must be positive".into());
        }
        cfg.detection_cycle = Some(std::time::Duration::from_secs_f64(secs));
    }
    if let Ok(p) = std::env::var("MOCK_NVR_PASSWORD") {
        cfg.password = p;
    }
    let nvr = spawn(cfg).map_err(|e| format!("cannot listen: {e}"))?;
    println!("mock-nvr listening on {}", nvr.addr());
    loop {
        std::thread::park();
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mock-nvr: {e}");
            ExitCode::FAILURE
        }
    }
}
