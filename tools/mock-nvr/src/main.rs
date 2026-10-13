//! Command-line mock NVR. The password comes from the `MOCK_NVR_PASSWORD`
//! environment variable (default `example-password`), never from an argument.

use bc_protocol::archive::DateTime;
use mock_nvr::archive::ArchiveConfig;
use mock_nvr::audio::{AudioConfig, AudioFixture};
use mock_nvr::replay::{Replay, ReplayConfig};
use mock_nvr::{Config, ExternMode, fixture::Fixture, spawn};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: mock-nvr [--bind ADDR] [--port N] [--channels N] [--fixture FILE]\n\
    \x20               [--user NAME] [--max-logins N] [--fps N] [--video-burst N] [--live-speed X] [--width N] [--height N]\n\
    \x20               [--audio FILE.aac] [--audio-clock HZ] [--audio-jitter-ms N]\n\
    \x20               [--replay FILE.nvrrec] [--replay-loop] [--replay-speed N]\n\
    \x20               [--recordings DAYS] [--recordings-fps N] [--aitype BITS] [--detections SECONDS]\n\
    \x20               [--older-playback] [--refuse-play-speed N]\n\
    \x20               [--multi-lens CH:BITS,BITS] [--extern CH:MODE,CH:MODE]\n\
    \x20               [--third-party CH,CH] [--third-party-refusals N] [--third-party-delay-ms N]\n\
    \x20               [--talk CH,CH] [--fisheye CH,CH] [--fisheye-fixture FILE]\n\
    \x20               [--fisheye-size N] [--ptz CH[.LENS][:lens],...]\n\
    --live-speed X sends live video and audio X times faster than real time with their picture\n\
    times unchanged (default 1), so a test that waits on media time finishes sooner.\n\
    --multi-lens makes channel CH a two-lens camera, each lens with its aitype bits.\n\
    --extern sets how channel CH lists and answers externStream/mobileStream: listed\n\
    (default), hidden (not listed, served: a second lens), info-only (not listed, one\n\
    0x0 stream-info record and no video), refused (400), or silent (no answer).\n\
    --third-party makes channels behave like third-party cameras behind an NVR: every\n\
    preview is answered 400, the first N (default 2) per channel and tier send nothing,\n\
    later ones stream after the delay (default 200 ms) with `:0dc`/`:1dc` magics.\n\
    --older-playback makes recordings play as on older NVR firmware: file searches list no Id,\n\
    a play is a file-information request (13) by name and then a play (5) by the file's start\n\
    time on its message ID; a play naming an Id is answered 400 (docs/protocol.md).\n\
    --refuse-play-speed N answers every play at playSpeed N with status 400 (a refused play).\n\
    --recordings-fps N records the days at N pictures a second (default 10), so a test that\n\
    plays a set number of recorded pictures at 1x finishes sooner.\n\
    --talk gives channels a speaker (doorbell talk-back: ipcAudioTalk, ability, talk).\n\
    --ptz makes channels cameras that move: CH pans, tilts, zooms and has 3 presets; CH:lens\n\
    has only a motorized zoom and focus lens; CH.1 is the second lens of a multi-lens\n\
    channel (addressed with subChannel 1), also CH.1:lens.\n\
    --fisheye makes channels fisheye cameras (fishEye 1, channelType fisheye_ipc) that\n\
    stream --fisheye-fixture (default fixtures/synthetic/fisheye/h264-fisheye.h264), advertised\n\
    as --fisheye-size squared (default 384, the synthetic fixture's size).\n\
    password: environment variable MOCK_NVR_PASSWORD (default example-password)";

fn run() -> Result<(), String> {
    let mut bind: IpAddr = [127, 0, 0, 1].into();
    let mut port = 9000u16;
    let mut fixture = PathBuf::from("fixtures/synthetic/h264-basic.h264");
    let (mut channels, mut max_logins, mut fps) = (4u8, 4usize, 25u32);
    let (mut width, mut height) = (320u32, 240u32);
    let mut video_burst = 1u32;
    let mut live_speed = 1.0f64;
    let mut user = String::from("example-user");
    let mut audio: Option<PathBuf> = None;
    let (mut audio_clock, mut audio_jitter_ms) = (16_000.0f64, 0u32);
    let mut replay: Option<PathBuf> = None;
    let (mut replay_loop, mut replay_speed) = (false, 1.0f64);
    let mut recording_days = 0u32;
    let mut older_playback = false;
    let mut refuse_play_speed: Option<u32> = None;
    let mut recordings_fps: Option<u32> = None;
    let mut aitype = 0u32;
    let mut multi_lens: Option<(u8, Vec<u32>)> = None;
    let mut extern_modes: Vec<(u8, ExternMode)> = Vec::new();
    let mut detections: Option<f64> = None;
    let mut third_party: Vec<u8> = Vec::new();
    let (mut third_party_refusals, mut third_party_delay_ms) = (2u32, 200u64);
    let mut talk: Vec<u8> = Vec::new();
    let mut ptz: Vec<(u8, u8, bool)> = Vec::new();
    let mut fisheye: Vec<u8> = Vec::new();
    let mut fisheye_fixture = PathBuf::from("fixtures/synthetic/fisheye/h264-fisheye.h264");
    let mut fisheye_size = 384u32;
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
        if a == "--older-playback" {
            older_playback = true;
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
            "--live-speed" => {
                live_speed = v.parse().map_err(|_| format!("bad value for {a}: {v}"))?
            }
            "--video-burst" => {
                video_burst = v.parse().map_err(|_| format!("bad value for {a}: {v}"))?
            }
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
            "--recordings-fps" => {
                recordings_fps = Some(
                    v.parse()
                        .ok()
                        .filter(|n| (1..=60).contains(n))
                        .ok_or_else(|| format!("bad value for {a}: {v} (1 to 60)"))?,
                )
            }
            "--refuse-play-speed" => {
                refuse_play_speed = Some(v.parse().map_err(|_| format!("bad value for {a}: {v}"))?)
            }
            "--aitype" => aitype = v.parse().map_err(|_| format!("bad value for {a}: {v}"))?,
            "--multi-lens" => {
                let bad = || format!("bad value for {a}: {v} (CH:BITS,BITS)");
                let (ch, bits) = v.split_once(':').ok_or_else(bad)?;
                let bits: Vec<u32> = bits
                    .split(',')
                    .map(|b| b.parse().map_err(|_| bad()))
                    .collect::<Result<_, _>>()?;
                if bits.len() < 2 {
                    return Err(bad());
                }
                multi_lens = Some((ch.parse().map_err(|_| bad())?, bits));
            }
            "--extern" => {
                for item in v.split(',') {
                    let bad = || format!("bad value for {a}: {item} (CH:MODE)");
                    let (ch, mode) = item.split_once(':').ok_or_else(bad)?;
                    let mode = match mode {
                        "listed" => ExternMode::Listed,
                        "hidden" => ExternMode::Hidden,
                        "info-only" => ExternMode::InfoOnly,
                        "refused" => ExternMode::Refused,
                        "silent" => ExternMode::Silent,
                        _ => return Err(bad()),
                    };
                    extern_modes.push((ch.parse().map_err(|_| bad())?, mode));
                }
            }
            "--third-party" => {
                third_party = v
                    .split(',')
                    .map(|c| c.parse().map_err(|_| format!("bad value for {a}: {v}")))
                    .collect::<Result<_, _>>()?
            }
            "--third-party-refusals" => {
                third_party_refusals = v.parse().map_err(|_| format!("bad value for {a}: {v}"))?
            }
            "--third-party-delay-ms" => {
                third_party_delay_ms = v.parse().map_err(|_| format!("bad value for {a}: {v}"))?
            }
            "--talk" => {
                talk = v
                    .split(',')
                    .map(|c| c.parse().map_err(|_| format!("bad value for {a}: {v}")))
                    .collect::<Result<_, _>>()?
            }
            "--ptz" => {
                for item in v.split(',') {
                    let bad = || format!("bad value for {a}: {item} (CH[.LENS][:lens])");
                    let (cam, kind) = match item.split_once(':') {
                        Some((cam, "lens")) => (cam, true),
                        Some(_) => return Err(bad()),
                        None => (item, false),
                    };
                    let (ch, lens) = match cam.split_once('.') {
                        Some((ch, "1")) => (ch, 1),
                        Some(_) => return Err(bad()),
                        None => (cam, 0),
                    };
                    ptz.push((ch.parse().map_err(|_| bad())?, lens, kind));
                }
            }
            "--fisheye" => {
                fisheye = v
                    .split(',')
                    .map(|c| c.parse().map_err(|_| format!("bad value for {a}: {v}")))
                    .collect::<Result<_, _>>()?
            }
            "--fisheye-fixture" => fisheye_fixture = PathBuf::from(v),
            "--fisheye-size" => {
                fisheye_size = v.parse().map_err(|_| format!("bad value for {a}: {v}"))?
            }
            "--detections" => {
                detections = Some(v.parse().map_err(|_| format!("bad value for {a}: {v}"))?)
            }
            "--replay-speed" => {
                replay_speed = v.parse().map_err(|_| format!("bad value for {a}: {v}"))?
            }
            _ => return Err(format!("unknown argument {a}\n{USAGE}")),
        }
    }
    if !(live_speed.is_finite() && live_speed > 0.0) {
        return Err("--live-speed must be positive".into());
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
    cfg.video_burst = video_burst;
    cfg.live_speed = live_speed;
    cfg.width = width;
    cfg.height = height;
    cfg.user = user;
    if let Some(path) = &audio {
        let mut a = AudioConfig::new(AudioFixture::load(path).map_err(|e| e.to_string())?);
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
        let mut a = ArchiveConfig::new(days);
        if let Some(path) = &audio {
            // The recorded days carry the same audio, so a download has a track.
            a.audio = Some(std::sync::Arc::new(
                AudioFixture::load(path).map_err(|e| e.to_string())?,
            ));
        }
        a.older_playback = older_playback;
        a.refuse_play_speed = refuse_play_speed;
        if let Some(fps) = recordings_fps {
            a.fps = fps;
        }
        cfg.archive = Some(a);
    }
    // Every channel advertises the same capabilities.
    cfg.aitypes = vec![aitype; usize::from(channels)];
    if let Some((ch, bits)) = multi_lens {
        if ch >= channels {
            return Err(format!("--multi-lens channel {ch} is not below --channels"));
        }
        cfg.lens_aitypes = vec![Vec::new(); usize::from(channels)];
        cfg.lens_aitypes[usize::from(ch)] = bits;
    }
    if !extern_modes.is_empty() {
        cfg.extern_modes = (0..channels).map(|c| cfg.extern_mode(c)).collect();
        for (ch, mode) in extern_modes {
            if ch >= channels {
                return Err(format!("--extern channel {ch} is not below --channels"));
            }
            cfg.extern_modes[usize::from(ch)] = mode;
        }
    }
    if let Some(secs) = detections {
        if !(secs.is_finite() && secs > 0.0) {
            return Err("--detections must be positive".into());
        }
        cfg.detection_cycle = Some(std::time::Duration::from_secs_f64(secs));
    }
    if !third_party.is_empty() {
        if let Some(ch) = third_party.iter().find(|c| **c >= channels) {
            return Err(format!(
                "--third-party channel {ch} is not below --channels"
            ));
        }
        let mut tp = mock_nvr::ThirdPartyConfig::new(third_party, third_party_refusals);
        tp.media_delay = std::time::Duration::from_millis(third_party_delay_ms);
        cfg.third_party = Some(tp);
    }
    if let Some(ch) = talk.iter().find(|c| **c >= channels) {
        return Err(format!("--talk channel {ch} is not below --channels"));
    }
    cfg.talk_channels = talk;
    for (ch, lens, lens_only) in ptz {
        if ch >= channels {
            return Err(format!("--ptz channel {ch} is not below --channels"));
        }
        let camera = if lens_only {
            mock_nvr::ptz::PtzCamera::lens_only()
        } else {
            mock_nvr::ptz::PtzCamera::pan_tilt_zoom(3)
        };
        if lens == 1 {
            cfg.ptz_lens1.insert(ch, camera);
        } else {
            cfg.ptz.insert(ch, camera);
        }
    }
    if !fisheye.is_empty() {
        if let Some(ch) = fisheye.iter().find(|c| **c >= channels) {
            return Err(format!("--fisheye channel {ch} is not below --channels"));
        }
        let fix = Fixture::load(&fisheye_fixture).map_err(|e| e.to_string())?;
        cfg.fisheye = Some(mock_nvr::FisheyeConfig::new(fisheye, fix, fisheye_size));
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
