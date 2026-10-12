//! bc-cli: log in to an NVR or camera, list its channels, record one live
//! channel to a raw `.h264`/`.h265` file that ffplay and VLC can open,
//! capture several tiers of a channel for timing analysis, analyze such a
//! capture, or search the local network for devices (`discover`).
//!
//! The password is never taken from the command line (it would end up in
//! shell history and process listings): it comes from the environment
//! variable `NVRVIEW_PASSWORD` or, on macOS, from a Keychain item.

use bc_cli::analyze::Analysis;
use bc_cli::playback_probe;
use bc_cli::replay_probe::{self, MAX_PLAYS, ProbeConfig};
use bc_cli::{
    CaptureConfig, CliError, ExtraOutputs, RecordLimits, Restart, capture, info_command,
    info_query, list_channels, record, wait_logged_in,
};
use bc_protocol::recording;
use bc_protocol::stream::StreamTier;
use bc_session::{DeviceSession, SessionConfig, TcpConnector};
use std::fs::File;
use std::io::BufWriter;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

mod discover;

const USAGE: &str = "\
usage:
  bc-cli --host HOST[:PORT] --user NAME [--keychain SERVICE[:ACCOUNT]] channels
  bc-cli --host HOST[:PORT] --user NAME [--keychain SERVICE[:ACCOUNT]] record
         --channel N [--tier main|sub|extern|mobile] --out FILE
         [--audio FILE.aac] [--raw FILE.nvrrec] [--seconds S] [--frames N]
  bc-cli --host HOST[:PORT] --user NAME [--keychain SERVICE[:ACCOUNT]] capture
         --channel N --raw FILE.nvrrec [--tiers sub,main] [--seconds S]
         [--stagger S] [--restart TIER:EVERY_S] [--restart-gap S]
  bc-cli --host HOST[:PORT] --user NAME [--keychain SERVICE[:ACCOUNT]] query
         80|146|199|318 [--channel N]
  bc-cli --host HOST[:PORT] --user NAME [--keychain SERVICE[:ACCOUNT]] lenses
         [--channel N] [--seconds S]
  bc-cli --host HOST[:PORT] --user NAME [--keychain SERVICE[:ACCOUNT]] talk
         --channel N --allow-talk [--pcm FILE.s16] [--tone HZ] [--seconds S]
  bc-cli --host HOST[:PORT] --user NAME [--keychain SERVICE[:ACCOUNT]] playback-probe
         --allow-playback-probe --now YYYY-MM-DDTHH:MM:SS --utc-offset-s N
         --channels A,B,C,D
  bc-cli --host HOST[:PORT] --user NAME [--keychain SERVICE[:ACCOUNT]] replay-probe
         --channel N --allow-replay-probe --now YYYY-MM-DDTHH:MM:SS
         [--tier main|sub] [--max-plays 1-4 | --plays N[~K],...] [--gap 0-10] [--no-live] [--batched]
  bc-cli analyze FILE.nvrrec
  bc-cli discover --help

The port defaults to 9000. The user name may also come from NVRVIEW_USER.
The password comes from NVRVIEW_PASSWORD or, on macOS, from the Keychain
item named with --keychain; never from the command line.
record writes video (Annex-B), starting at a keyframe, until Ctrl-C,
--seconds, or --frames. --audio also writes the audio as received (ADTS AAC).
--raw also writes every record with its arrival time, for replaying the
stream later; keep real recordings in the git-ignored captures/ directory.
capture records several tiers of one channel on one session and one clock
(NVRREC02): the tiers start in the order given, --stagger seconds apart
(default 2); --restart stops one tier after EVERY_S seconds and starts it
again --restart-gap seconds later (default 2), repeatedly, to sample start
latency. Only preview start and stop are sent.
query sends one read-only info query (80 device version, 146 stream info,
199 capabilities, 318 a channel's version, which needs --channel) and prints
the decrypted XML reply body to stdout, the reply status to stderr. Nothing
else can be sent. The output can contain device identifiers (UIDs, serial
numbers): keep it in the git-ignored captures/ directory, never commit it.
lenses runs the viewer's second-lens probe (docs/protocol.md, \"Second
lenses\") on each connected channel whose list does not name externStream,
or on --channel N whatever it lists: one externStream preview, up to
--seconds (default 2) for the first picture, then a stop; one at a time.
talk sends sound to a camera's or doorbell's speaker (talk-back, camera
motion control, docs/protocol.md \"Talk-back (motion)\"): only with
--allow-talk, which enables it for --channel for this run. The sound is
--pcm (raw 16 kHz mono signed 16-bit little-endian, as from
`ffmpeg -i IN -ar 16000 -ac 1 -f s16le OUT`) or a quiet tone (--tone, default
440 Hz) for --seconds (default 2), pushed in real time like a microphone.
The talk is closed at the end, on Ctrl-C, and on every error.
playback-probe runs the playback engine of viewer-core against a real device
(docs/MILESTONE-P.md, C10): only with --allow-playback-probe. Through the
core it adds the device, indexes today for 1 to 4 --channels, opens a playback
of them at a time all recorded (at least 190 s old), and runs a fixed script
(1x 20 s, exact seek +60 s 15 s, 4x 15 s, 16x 10 s, 1x 10 s, pause, resume)
with one live tile beside it, then prints per camera the time to the first
picture, pictures released per second and media speed against the rate, the
cameras' sync spread, stalls, late/no_recording states, and the engine's
buffer stops and restarts. --now is the device's local time and
--utc-offset-s its offset from UTC. Read-only; capped at 150 s; Ctrl-C closes
the playback, the live tile, and the session. Keep its output in the ignored
captures/ directory.

replay-probe measures whether one session carries live view and recorded
plays at once (docs/MILESTONE-P.md, B5): only with --allow-replay-probe. On
one session it starts live view of --channel, finds a recording of that
channel at least 60 s long that ended at least 2 minutes before --now (the
device's local time, which its recording times use; a keyframe's wall clock
can be UTC), then runs stages of
1, 2, ... up to --max-plays (default and at most 4) simultaneous plays of it
at normal speed (--plays instead names each play's recording: N for channel
N's latest finished one, N~K for the K-th before it, K at most 3; stage k
runs the first k, so 11,3,11~1 tells a per-file, per-camera and total limit
apart), each for at most 10 s, with --gap seconds (default 1, at most 10)
of live view alone between stages, counting live and play frames per
second. It stops at the first refusal, error, stall, or live disruption;
every play is stopped (7) and the session closed on every path, Ctrl-C
included; the whole run is capped at 90 s. --no-live leaves out the live
preview and every live measurement and check (the plays alone; --now is then
the only source of the device's time). --batched starts each stage's plays the way the
official client does (docs/protocol.md, \"Four plays at once\"): all seeks back
to back at one time (5 s after the latest start of the stage's recordings;
when they share no such time, each at its own recording's start + 5 s), a
0.5 s pause, then all plays back to back, instead of seek, play, seek, play
at each play's own time. Only the file search, live
preview, seek (123), play (5) and stop (7) are sent. Keep its output in the
ignored captures/ directory.
analyze prints a Markdown timing report of a recording (aggregates only).";

/// How long to wait for the login before giving up.
const LOGIN_LIMIT: Duration = Duration::from_secs(20);

enum Action {
    Channels,
    Record {
        channel: u8,
        tier: StreamTier,
        out: PathBuf,
        audio: Option<PathBuf>,
        raw: Option<PathBuf>,
        seconds: Option<u64>,
        frames: Option<u64>,
    },
    Capture {
        cfg: CaptureConfig,
        raw: PathBuf,
        seconds: Option<u64>,
    },
    Query {
        cmd: bc_protocol::session::Command,
        channel: Option<u8>,
    },
    Lenses {
        channel: Option<u8>,
        window: Duration,
    },
    Talk {
        channel: u8,
        pcm: Option<PathBuf>,
        tone: f64,
        seconds: u64,
    },
    ReplayProbe(ProbeConfig),
    PlaybackProbe(playback_probe::ProbeConfig),
}

enum Parsed {
    Device(Args),
    Analyze(PathBuf),
}

fn parse_tier(name: &str) -> Result<StreamTier, String> {
    Ok(match name {
        "main" => StreamTier::Main,
        "sub" => StreamTier::Sub,
        "extern" => StreamTier::Extern,
        "mobile" => StreamTier::Mobile,
        other => return Err(format!("unknown tier {other}")),
    })
}

fn parse_secs(name: &str, v: &str) -> Result<std::time::Duration, String> {
    v.parse::<f64>()
        .ok()
        .filter(|s| s.is_finite() && *s >= 0.0)
        .map(std::time::Duration::from_secs_f64)
        .ok_or_else(|| format!("bad {name}"))
}

struct Args {
    host: String,
    user: String,
    keychain: Option<String>,
    action: Action,
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Parsed, String> {
    let mut host = None;
    let mut user = std::env::var("NVRVIEW_USER").ok();
    let mut keychain = None;
    let mut command = None;
    let (mut channel, mut tier, mut out, mut seconds, mut frames) =
        (None, StreamTier::Sub, None, None, None);
    let (mut audio, mut raw) = (None, None);
    let mut tiers = vec![StreamTier::Sub, StreamTier::Main];
    let mut stagger = Duration::from_secs(2);
    let (mut restart, mut restart_gap) = (None, Duration::from_secs(2));
    let mut file = None;
    let mut number = None;
    let (mut allow_talk, mut pcm, mut tone) = (false, None, 440.0f64);
    let (mut allow_probe, mut max_plays, mut now) = (false, None, None);
    let (mut no_live, mut batched) = (false, false);
    let (mut allow_pb_probe, mut utc_offset, mut pb_channels) = (false, None, None);
    let mut plays = None;
    let mut gap = None;
    let mut it = args;
    while let Some(a) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match a.as_str() {
            "--host" => host = Some(value("--host")?),
            "--user" => user = Some(value("--user")?),
            "--keychain" => keychain = Some(value("--keychain")?),
            "--password" => return Err("passwords are not accepted on the command line".into()),
            "--channel" => {
                channel = Some(value("--channel")?.parse().map_err(|_| "bad --channel")?);
            }
            "--tier" => tier = parse_tier(&value("--tier")?)?,
            "--tiers" => {
                tiers = value("--tiers")?
                    .split(',')
                    .map(parse_tier)
                    .collect::<Result<_, _>>()?;
            }
            "--stagger" => stagger = parse_secs("--stagger", &value("--stagger")?)?,
            "--restart" => {
                let v = value("--restart")?;
                let (t, every) = v.split_once(':').ok_or("--restart needs TIER:EVERY_S")?;
                restart = Some((parse_tier(t)?, parse_secs("--restart", every)?));
            }
            "--restart-gap" => {
                restart_gap = parse_secs("--restart-gap", &value("--restart-gap")?)?;
            }
            "--out" => out = Some(PathBuf::from(value("--out")?)),
            "--audio" => audio = Some(PathBuf::from(value("--audio")?)),
            "--raw" => raw = Some(PathBuf::from(value("--raw")?)),
            "--seconds" => {
                seconds = Some(value("--seconds")?.parse().map_err(|_| "bad --seconds")?);
            }
            "--frames" => {
                frames = Some(value("--frames")?.parse().map_err(|_| "bad --frames")?);
            }
            "--allow-talk" => allow_talk = true,
            "--allow-replay-probe" => allow_probe = true,
            "--max-plays" => {
                max_plays = Some(
                    value("--max-plays")?
                        .parse::<usize>()
                        .ok()
                        .filter(|n| (1..=MAX_PLAYS).contains(n))
                        .ok_or("bad --max-plays (1 to 4)")?,
                );
            }
            "--plays" => {
                plays = Some(
                    replay_probe::parse_plays(&value("--plays")?)
                        .ok_or("bad --plays (1 to 4 of N or N~K, K at most 3)")?,
                );
            }
            "--gap" => {
                gap = Some(
                    value("--gap")?
                        .parse::<u64>()
                        .ok()
                        .filter(|g| *g <= 10)
                        .map(Duration::from_secs)
                        .ok_or("bad --gap (0 to 10 s)")?,
                );
            }
            "--now" => {
                now = Some(
                    replay_probe::parse_time(&value("--now")?)
                        .ok_or("bad --now (YYYY-MM-DDTHH:MM:SS)")?,
                );
            }
            "--allow-playback-probe" => allow_pb_probe = true,
            "--utc-offset-s" => {
                utc_offset = Some(
                    playback_probe::parse_utc_offset(&value("--utc-offset-s")?)
                        .ok_or("bad --utc-offset-s (seconds from UTC, within 18 hours)")?,
                );
            }
            "--channels" => {
                pb_channels = Some(
                    playback_probe::parse_channels(&value("--channels")?)
                        .ok_or("bad --channels (1 to 4 distinct channel numbers, A,B,C,D)")?,
                );
            }
            "--no-live" => no_live = true,
            "--batched" => batched = true,
            "--pcm" => pcm = Some(PathBuf::from(value("--pcm")?)),
            "--tone" => {
                tone = value("--tone")?
                    .parse::<f64>()
                    .ok()
                    .filter(|f| (50.0..=4000.0).contains(f))
                    .ok_or("bad --tone (50 to 4000 Hz)")?;
            }
            "-h" | "--help" => return Err(String::new()),
            "channels" | "record" | "capture" | "analyze" | "query" | "lenses" | "talk"
            | "replay-probe" | "playback-probe"
                if command.is_none() =>
            {
                command = Some(a);
            }
            _ if command.as_deref() == Some("query") && number.is_none() => {
                number = Some(a.parse::<u32>().map_err(|_| format!("bad query {a}"))?);
            }
            _ if command.as_deref() == Some("analyze") && file.is_none() => {
                file = Some(PathBuf::from(a));
            }
            other => return Err(format!("unexpected argument {other}")),
        }
    }
    let action = match command.as_deref() {
        Some("channels") => Action::Channels,
        Some("record") => Action::Record {
            channel: channel.ok_or("record needs --channel")?,
            tier,
            out: out.ok_or("record needs --out")?,
            audio,
            raw,
            seconds,
            frames,
        },
        Some("capture") => {
            if tiers.is_empty()
                || tiers
                    .iter()
                    .enumerate()
                    .any(|(i, t)| tiers[..i].contains(t))
            {
                return Err("--tiers must name distinct tiers".into());
            }
            let restart = match restart {
                Some((tier, every)) => {
                    if !tiers.contains(&tier) {
                        return Err("--restart names a tier not in --tiers".into());
                    }
                    if every.is_zero() {
                        return Err("--restart needs a positive period".into());
                    }
                    Some(Restart {
                        tier,
                        every,
                        gap: restart_gap,
                    })
                }
                None => None,
            };
            Action::Capture {
                cfg: CaptureConfig {
                    channel: channel.ok_or("capture needs --channel")?,
                    tiers,
                    stagger,
                    restart,
                },
                raw: raw.ok_or("capture needs --raw FILE.nvrrec")?,
                seconds,
            }
        }
        Some("analyze") => return Ok(Parsed::Analyze(file.ok_or("analyze needs a file")?)),
        Some("talk") => {
            if !allow_talk {
                return Err("talk sends sound to a camera's speaker; pass --allow-talk".into());
            }
            Action::Talk {
                channel: channel.ok_or("talk needs --channel")?,
                pcm,
                tone,
                seconds: seconds.unwrap_or(2),
            }
        }
        Some("replay-probe") => {
            if !allow_probe {
                return Err(
                    "replay-probe plays recordings on a real device; pass --allow-replay-probe"
                        .into(),
                );
            }
            if !matches!(tier, StreamTier::Sub | StreamTier::Main) {
                return Err("replay-probe plays the main or sub stream only".into());
            }
            let mut cfg = ProbeConfig::new(channel.ok_or("replay-probe needs --channel")?);
            cfg.tier = tier;
            if max_plays.is_some() && plays.is_some() {
                return Err("--max-plays and --plays do not go together".into());
            }
            cfg.max_plays = max_plays.unwrap_or(MAX_PLAYS);
            cfg.plays = plays.unwrap_or_default();
            cfg.gap = gap.unwrap_or(cfg.gap);
            cfg.now = Some(now.ok_or("replay-probe needs --now (the device's local time)")?);
            cfg.no_live = no_live;
            cfg.batched = batched;
            cfg.login_limit = LOGIN_LIMIT;
            Action::ReplayProbe(cfg)
        }
        Some("playback-probe") => {
            if !allow_pb_probe {
                return Err(
                    "playback-probe plays recordings on a real device; pass --allow-playback-probe"
                        .into(),
                );
            }
            let now = now.ok_or("playback-probe needs --now (the device's local time)")?;
            let utc_offset = utc_offset
                .ok_or("playback-probe needs --utc-offset-s (the device's UTC offset)")?;
            let channels = pb_channels.ok_or("playback-probe needs --channels A,B,C,D")?;
            // The host is resolved when the run starts.
            Action::PlaybackProbe(playback_probe::ProbeConfig::new(
                "", 0, "", now, utc_offset, channels,
            ))
        }
        Some("lenses") => Action::Lenses {
            channel,
            window: seconds.map_or(Duration::from_secs(2), Duration::from_secs),
        },
        Some("query") => {
            let n = number.ok_or("query needs a command number: 80, 146, 199, or 318")?;
            let cmd = info_command(n, channel).ok_or_else(|| {
                if n == 318 {
                    "query 318 needs --channel".to_owned()
                } else if matches!(n, 80 | 146 | 199) {
                    format!("query {n} takes no --channel")
                } else {
                    format!("query {n} is not a read-only info query (80, 146, 199, 318)")
                }
            })?;
            Action::Query { cmd, channel }
        }
        _ => return Err("missing command".into()),
    };
    Ok(Parsed::Device(Args {
        host: host.ok_or("missing --host")?,
        user: user.ok_or("missing --user (or NVRVIEW_USER)")?,
        keychain,
        action,
    }))
}

fn password(keychain: Option<&str>) -> Result<String, String> {
    if let Some(item) = keychain {
        return keychain_password(item);
    }
    std::env::var("NVRVIEW_PASSWORD")
        .map_err(|_| "set NVRVIEW_PASSWORD or use --keychain SERVICE[:ACCOUNT]".to_owned())
}

#[cfg(target_os = "macos")]
fn keychain_password(item: &str) -> Result<String, String> {
    let (service, account) = match item.split_once(':') {
        Some((s, a)) => (s, Some(a)),
        None => (item, None),
    };
    let mut cmd = std::process::Command::new("/usr/bin/security");
    cmd.args(["find-generic-password", "-s", service]);
    if let Some(a) = account {
        cmd.args(["-a", a]);
    }
    let out = cmd
        .arg("-w")
        .output()
        .map_err(|e| format!("cannot run security: {e}"))?;
    if !out.status.success() {
        return Err("no matching Keychain item".into());
    }
    let s = String::from_utf8(out.stdout).map_err(|_| "Keychain item is not text")?;
    Ok(s.trim_end_matches('\n').to_owned())
}

#[cfg(not(target_os = "macos"))]
fn keychain_password(_item: &str) -> Result<String, String> {
    Err("--keychain is only available on macOS; use NVRVIEW_PASSWORD".into())
}

async fn resolve(host: &str) -> Result<SocketAddr, String> {
    let with_port = if host.parse::<SocketAddr>().is_ok()
        || host
            .rsplit_once(':')
            .is_some_and(|(h, p)| !h.contains(':') && p.parse::<u16>().is_ok())
    {
        host.to_owned()
    } else {
        format!("{host}:9000")
    };
    tokio::net::lookup_host(&with_port)
        .await
        .map_err(|e| format!("cannot resolve {host}: {e}"))?
        .next()
        .ok_or_else(|| format!("cannot resolve {host}"))
}

async fn ctrl_c_or(seconds: Option<u64>) {
    match seconds {
        Some(s) => {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                () = tokio::time::sleep(Duration::from_secs(s)) => {}
            }
        }
        None => {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}

async fn run(
    args: Args,
    logs: Option<std::sync::Arc<playback_probe::LogCounts>>,
) -> Result<(), String> {
    let password = password(args.keychain.as_deref())?;
    let addr = resolve(&args.host).await?;
    if let Action::PlaybackProbe(mut cfg) = args.action {
        cfg.host = addr.ip().to_string();
        cfg.port = addr.port();
        cfg.user = args.user;
        return run_playback_probe(cfg, password, logs.unwrap_or_default()).await;
    }
    let mut cfg = SessionConfig::new(args.user, password);
    if matches!(args.action, Action::ReplayProbe(_)) {
        cfg.max_plays = MAX_PLAYS;
        cfg.query_timeout = Duration::from_secs(10);
    }
    let session = DeviceSession::spawn(TcpConnector::new(addr), cfg);
    let result = run_action(&session, args.action).await;
    // Stops previews and closes the socket before the process exits.
    session.close().await;
    result.map_err(|e| e.to_string())
}

/// Runs the playback probe on a blocking thread (the core has its own
/// threads) and turns Ctrl-C into its stop flag.
async fn run_playback_probe(
    cfg: playback_probe::ProbeConfig,
    password: String,
    logs: std::sync::Arc<playback_probe::LogCounts>,
) -> Result<(), String> {
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = stop.clone();
    let ctrl_c = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    });
    let r = tokio::task::spawn_blocking(move || {
        let mut out = std::io::stdout();
        playback_probe::run(&cfg, &password, logs, stop, &mut out)
    })
    .await
    .map_err(|e| format!("the probe thread failed: {e}"))?;
    ctrl_c.abort();
    let report = r?;
    print!("{}", playback_probe::format_summary(&report));
    Ok(())
}

async fn run_action(session: &DeviceSession, action: Action) -> Result<(), CliError> {
    match action {
        Action::Channels => {
            let listing = tokio::select! {
                r = list_channels(session, LOGIN_LIMIT) => r?,
                _ = tokio::signal::ctrl_c() => return Ok(()),
            };
            println!("device: {} ({})", listing.device.name, listing.device.model);
            println!("{:>3}  {:<9}  {:<22}  name (model)", "ch", "state", "tiers");
            for c in listing.channels {
                let tiers: Vec<&str> = c.tiers.iter().map(|t| t.name()).collect();
                let version = c
                    .version
                    .map(|v| format!("{} ({})", v.name, v.model))
                    .unwrap_or_default();
                println!(
                    "{:>3}  {:<9}  {:<22}  {}",
                    c.channel,
                    if c.connected { "connected" } else { "empty" },
                    tiers.join(","),
                    version
                );
            }
            Ok(())
        }
        Action::Lenses { channel, window } => {
            let listing = tokio::select! {
                r = list_channels(session, LOGIN_LIMIT) => r?,
                _ = tokio::signal::ctrl_c() => return Ok(()),
            };
            println!("{:>3}  {:<36}  probe", "ch", "tiers");
            for c in listing.channels {
                let lists_extern = c.tiers.contains(&StreamTier::Extern);
                let wanted = match channel {
                    Some(n) => n == c.channel,
                    None => c.connected && !lists_extern,
                };
                if !wanted {
                    continue;
                }
                let tiers: Vec<&str> = c.tiers.iter().map(|t| t.name()).collect();
                let r = tokio::select! {
                    r = session.probe_second_lens(c.channel, window) => r,
                    _ = tokio::signal::ctrl_c() => return Ok(()),
                };
                let what = match r {
                    bc_session::LensProbe::SecondLens { after } => {
                        format!("second lens (first picture after {} ms)", after.as_millis())
                    }
                    bc_session::LensProbe::NoVideo { info: true } => {
                        "single lens (stream info only, no video)".to_owned()
                    }
                    bc_session::LensProbe::NoVideo { info: false } => {
                        "single lens (no answer)".to_owned()
                    }
                    bc_session::LensProbe::Refused { status } => {
                        format!("single lens (refused, status {status})")
                    }
                    bc_session::LensProbe::Inconclusive => "inconclusive".to_owned(),
                };
                let note = if lists_extern {
                    "  (lists externStream: video does not prove a second lens)"
                } else {
                    ""
                };
                println!("{:>3}  {:<36}  {what}{note}", c.channel, tiers.join(","));
            }
            Ok(())
        }
        Action::Record {
            channel,
            tier,
            out,
            audio,
            raw,
            seconds,
            frames,
        } => {
            tokio::select! {
                r = wait_logged_in(session, LOGIN_LIMIT) => r?,
                _ = tokio::signal::ctrl_c() => return Ok(()),
            }
            let mut file = BufWriter::new(File::create(&out)?);
            eprintln!(
                "recording channel {channel} ({}) to {}; Ctrl-C to stop",
                tier.name(),
                out.display()
            );
            let mut audio_file = audio
                .as_ref()
                .map(File::create)
                .transpose()?
                .map(BufWriter::new);
            let mut raw_file = raw
                .as_ref()
                .map(File::create)
                .transpose()?
                .map(BufWriter::new);
            let extra = ExtraOutputs {
                audio: audio_file.as_mut().map(|f| f as &mut dyn std::io::Write),
                raw: match raw_file.as_mut() {
                    Some(f) => Some(recording::Writer::new(f as &mut dyn std::io::Write)?),
                    None => None,
                },
            };
            let stats = record(
                session,
                channel,
                tier,
                &mut file,
                extra,
                RecordLimits { frames },
                ctrl_c_or(seconds),
            )
            .await?;
            let ext = out.extension().and_then(|e| e.to_str()).unwrap_or("");
            let codec = match stats.codec {
                Some(bc_protocol::media::Codec::H264) => "H.264",
                Some(bc_protocol::media::Codec::H265) => "H.265",
                None => "none",
            };
            eprintln!(
                "wrote {} frames ({} keyframes, {} bytes, {:.1} s of video, codec {codec}); \
                 {} gaps, {} dropped",
                stats.frames,
                stats.keyframes,
                stats.bytes,
                stats.duration_us as f64 / 1e6,
                stats.discontinuities,
                stats.dropped
            );
            if audio.is_some() || stats.audio_frames > 0 {
                eprintln!("{} audio frames", stats.audio_frames);
            }
            if raw.is_some() {
                eprintln!("{} records in the raw recording", stats.raw_records);
            }
            if stats.waits > 0 {
                eprintln!(
                    "note: the device answered the preview with status 400; \
                     the session waited for its stream ({} wait(s))",
                    stats.waits
                );
            }
            if stats.skipped_codec_change > 0 {
                eprintln!(
                    "skipped {} frames after a codec change",
                    stats.skipped_codec_change
                );
            }
            let expected = match stats.codec {
                Some(bc_protocol::media::Codec::H264) => Some("h264"),
                Some(bc_protocol::media::Codec::H265) => Some("h265"),
                None => None,
            };
            if let Some(e) = expected.filter(|e| !ext.eq_ignore_ascii_case(e)) {
                eprintln!("note: the stream is {codec}; players may need a .{e} extension");
            }
            Ok(())
        }
        Action::Talk {
            channel,
            pcm,
            tone,
            seconds,
        } => {
            let samples: Vec<i16> = match pcm {
                Some(p) => {
                    let bytes = std::fs::read(p)?;
                    bytes
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|b| i16::from_le_bytes(*b))
                        .collect()
                }
                // About -20 dBFS: audible without being loud.
                None => (0..seconds as usize * 16_000)
                    .map(|i| {
                        let t = i as f64 / 16_000.0;
                        (3000.0 * (2.0 * std::f64::consts::PI * tone * t).sin()) as i16
                    })
                    .collect(),
            };
            tokio::select! {
                r = wait_logged_in(session, LOGIN_LIMIT) => r?,
                _ = tokio::signal::ctrl_c() => return Ok(()),
            }
            session.set_motion_allowed(channel, true);
            let started = std::time::Instant::now();
            let mut talk = tokio::select! {
                r = session.talk(channel) => r.map_err(CliError::TalkRefused)?,
                _ = tokio::signal::ctrl_c() => return Ok(()),
            };
            eprintln!(
                "talk-back active on channel {channel} after {} ms; sending {:.2} s",
                started.elapsed().as_millis(),
                samples.len() as f64 / 16_000.0
            );
            let mut tick = tokio::time::interval(Duration::from_millis(20));
            let mut interrupted = false;
            for chunk in samples.chunks(320) {
                tokio::select! {
                    _ = tick.tick() => {}
                    _ = tokio::signal::ctrl_c() => {
                        interrupted = true;
                        break;
                    }
                }
                talk.push(chunk);
                if !matches!(talk.state(), bc_session::TalkState::Active) {
                    break;
                }
            }
            if interrupted {
                talk.stop();
            } else {
                talk.finish();
            }
            let end = tokio::time::timeout(Duration::from_secs(15), talk.ended()).await;
            let st = talk.stats();
            eprintln!(
                "talk-back ended: {:?}; records sent {}, acknowledged {}, samples dropped {}, \
                 clock restarts {}",
                end.unwrap_or(talk.state()),
                st.records_sent,
                st.records_acked,
                st.samples_dropped,
                st.clock_restarts
            );
            Ok(())
        }
        Action::PlaybackProbe(_) => unreachable!("handled before the session opens"),
        Action::ReplayProbe(cfg) => {
            let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
            let ctrl_c = tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    let _ = stop_tx.send(true);
                }
            });
            let mut out = std::io::stdout();
            let r = replay_probe::replay_probe(session, &cfg, &mut out, stop_rx).await;
            ctrl_c.abort();
            let report = r?;
            println!("result: {}", report.outcome);
            if cfg.no_live {
                println!(
                    "highest number of simultaneous plays that all delivered with no live view: {}",
                    report.max_ok
                );
            } else {
                println!(
                    "highest number of simultaneous plays that all delivered beside live view: {}",
                    report.max_ok
                );
                println!(
                    "live fps: baseline {:.1}, after the plays {}",
                    report.baseline_live_fps,
                    report
                        .tail_live_fps
                        .map_or("not measured".into(), |f| format!("{f:.1}"))
                );
            }
            println!(
                "plays requested {}, stops sent {}; the session is closed next",
                report.plays_started, report.stops_sent
            );
            Ok(())
        }
        Action::Query { cmd, channel } => {
            let reply = tokio::select! {
                r = info_query(session, cmd, channel, LOGIN_LIMIT) => r?,
                _ = tokio::signal::ctrl_c() => return Ok(()),
            };
            eprintln!("status {}", reply.status);
            print!("{}", reply.xml);
            if !reply.xml.is_empty() && !reply.xml.ends_with('\n') {
                println!();
            }
            Ok(())
        }
        Action::Capture { cfg, raw, seconds } => {
            tokio::select! {
                r = wait_logged_in(session, LOGIN_LIMIT) => r?,
                _ = tokio::signal::ctrl_c() => return Ok(()),
            }
            let file = BufWriter::new(File::create(&raw)?);
            let mut out = recording::TierWriter::new(file)?;
            let names: Vec<&str> = cfg.tiers.iter().map(|t| t.name()).collect();
            eprintln!(
                "capturing channel {} ({}) to {}; Ctrl-C to stop",
                cfg.channel,
                names.join(", "),
                raw.display()
            );
            let result = capture(session, &cfg, &mut out, ctrl_c_or(seconds)).await;
            out.finish()?;
            let stats = result?;
            eprintln!("captured {:.1} s", stats.duration.as_secs_f64());
            eprintln!(
                "{:<12} {:>6} {:>6} {:>8} {:>8} {:>9} {:>5} {:>8}",
                "tier", "starts", "stops", "records", "video", "keyframes", "gaps", "dropped"
            );
            for (tier, c) in &stats.tiers {
                eprintln!(
                    "{:<12} {:>6} {:>6} {:>8} {:>8} {:>9} {:>5} {:>8}",
                    tier.name(),
                    c.starts,
                    c.stops,
                    c.records,
                    c.videos,
                    c.keyframes,
                    c.discontinuities,
                    c.dropped
                );
            }
            for (tier, c) in stats.tiers.iter().filter(|(_, c)| c.waits > 0) {
                eprintln!(
                    "note: {}: the device answered the preview with status 400; \
                     the session waited for its stream ({} wait(s))",
                    tier.name(),
                    c.waits
                );
            }
            eprintln!("analyze with: bc-cli analyze {}", raw.display());
            Ok(())
        }
    }
}

fn analyze(path: &std::path::Path) -> ExitCode {
    let result = File::open(path).and_then(Analysis::read);
    match result {
        Ok(a) => {
            print!("{}", a.to_markdown());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("bc-cli: {}: {e}", path.display());
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.first().map(String::as_str) == Some("discover") {
        if argv.iter().any(|a| a == "-h" || a == "--help") {
            eprintln!("usage:\n{}", discover::USAGE);
            return ExitCode::from(2);
        }
        init_logging();
        return discover::main(&argv[1..]);
    }
    let args = match parse_args(argv.into_iter()) {
        Ok(Parsed::Device(a)) => a,
        Ok(Parsed::Analyze(path)) => return analyze(&path),
        Err(e) => {
            if !e.is_empty() {
                eprintln!("bc-cli: {e}\n");
            }
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let logs = if matches!(args.action, Action::PlaybackProbe(_)) {
        Some(playback_probe::init_logging())
    } else {
        init_logging();
        None
    };
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("bc-cli: cannot start the runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(args, logs)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("bc-cli: {e}");
            ExitCode::FAILURE
        }
    }
}

fn init_logging() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("NVRVIEW_LOG")
                .unwrap_or_else(|_| "warn".into()),
        )
        .init();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Parsed, String> {
        parse_args(args.iter().map(|s| (*s).to_owned()))
    }

    const BASE: [&str; 4] = ["--host", "192.0.2.10", "--user", "example-user"];

    #[test]
    fn replay_probe_needs_its_flag_and_bounds_plays() {
        let with = |extra: &[&str]| {
            let mut v = BASE.to_vec();
            v.extend_from_slice(extra);
            parse(&v)
        };
        let e = with(&["replay-probe", "--channel", "1"]).err().unwrap();
        assert!(e.contains("--allow-replay-probe"), "{e}");
        let e = with(&["replay-probe", "--channel", "1", "--allow-replay-probe"])
            .err()
            .unwrap();
        assert!(e.contains("--now"), "{e}");
        let ok = with(&[
            "replay-probe",
            "--channel",
            "1",
            "--allow-replay-probe",
            "--now",
            "2026-10-08T09:30:00",
        ]);
        let Ok(Parsed::Device(Args {
            action: Action::ReplayProbe(cfg),
            ..
        })) = ok
        else {
            panic!("not parsed");
        };
        assert_eq!((cfg.channel, cfg.max_plays), (1, MAX_PLAYS));
        assert!(cfg.now.is_some());
        for bad in ["0", "5"] {
            let r = with(&[
                "replay-probe",
                "--channel",
                "1",
                "--allow-replay-probe",
                "--max-plays",
                bad,
            ]);
            assert!(r.is_err(), "--max-plays {bad}");
        }
        let r = with(&[
            "replay-probe",
            "--channel",
            "1",
            "--allow-replay-probe",
            "--tier",
            "extern",
        ]);
        assert!(r.is_err());
        let base = [
            "replay-probe",
            "--channel",
            "1",
            "--allow-replay-probe",
            "--now",
            "2026-10-08T09:30:00",
        ];
        let mut v = base.to_vec();
        v.extend_from_slice(&["--plays", "1,2,1~1"]);
        let Ok(Parsed::Device(Args {
            action: Action::ReplayProbe(cfg),
            ..
        })) = with(&v)
        else {
            panic!("--plays not parsed");
        };
        assert_eq!(cfg.plays.len(), 3);
        let mut v = base.to_vec();
        v.extend_from_slice(&["--plays", "1,2", "--max-plays", "2"]);
        assert!(with(&v).is_err());
        let mut v = base.to_vec();
        v.extend_from_slice(&["--plays", "1~9"]);
        assert!(with(&v).is_err());
        let Ok(Parsed::Device(Args {
            action: Action::ReplayProbe(cfg),
            ..
        })) = with(&base)
        else {
            panic!("base not parsed");
        };
        assert!(!cfg.no_live);
        let mut v = base.to_vec();
        v.push("--no-live");
        let Ok(Parsed::Device(Args {
            action: Action::ReplayProbe(cfg),
            ..
        })) = with(&v)
        else {
            panic!("--no-live not parsed");
        };
        assert!(cfg.no_live && cfg.now.is_some());
        // --now stays required without live view.
        let e = with(&[
            "replay-probe",
            "--channel",
            "1",
            "--allow-replay-probe",
            "--no-live",
        ])
        .err()
        .unwrap();
        assert!(e.contains("--now"), "{e}");
        let mut v = base.to_vec();
        v.extend_from_slice(&["--gap", "5"]);
        let Ok(Parsed::Device(Args {
            action: Action::ReplayProbe(cfg),
            ..
        })) = with(&v)
        else {
            panic!("--gap not parsed");
        };
        assert_eq!(cfg.gap, Duration::from_secs(5));
        let mut v = base.to_vec();
        v.extend_from_slice(&["--gap", "11"]);
        assert!(with(&v).is_err());
        // --batched is a plain flag, off by default.
        let Ok(Parsed::Device(Args {
            action: Action::ReplayProbe(cfg),
            ..
        })) = with(&base)
        else {
            panic!("base not parsed");
        };
        assert!(!cfg.batched);
        let mut v = base.to_vec();
        v.extend_from_slice(&["--batched", "--plays", "0,1,2,0~1"]);
        let Ok(Parsed::Device(Args {
            action: Action::ReplayProbe(cfg),
            ..
        })) = with(&v)
        else {
            panic!("--batched not parsed");
        };
        assert!(cfg.batched && cfg.plays.len() == 4);
        assert!(with(&["replay-probe", "--allow-replay-probe"]).is_err());
        assert!(with(&["--password", "x", "replay-probe"]).is_err());
    }

    #[test]
    fn playback_probe_needs_its_flags() {
        let with = |extra: &[&str]| {
            let mut v: Vec<&str> = BASE.to_vec();
            v.push("playback-probe");
            v.extend_from_slice(extra);
            parse(&v)
        };
        let all = [
            "--allow-playback-probe",
            "--now",
            "2026-10-08T12:00:00",
            "--utc-offset-s",
            "-14400",
            "--channels",
            "0,1,2,3",
        ];
        let e = with(&all[1..]).err().unwrap();
        assert!(e.contains("--allow-playback-probe"), "{e}");
        for flag in ["--now", "--utc-offset-s", "--channels"] {
            let i = all.iter().position(|a| *a == flag).unwrap();
            let mut v = all.to_vec();
            v.drain(i..i + 2);
            let e = with(&v).err().unwrap();
            assert!(e.contains(flag), "{flag}: {e}");
        }
        let Ok(Parsed::Device(Args {
            action: Action::PlaybackProbe(cfg),
            ..
        })) = with(&all)
        else {
            panic!("did not parse");
        };
        assert_eq!(cfg.utc_offset_s, -14400);
        assert_eq!(cfg.channels, vec![0, 1, 2, 3]);
        let want = replay_probe::parse_time("2026-10-08T12:00:00").unwrap();
        assert_eq!(cfg.now.to_naive_secs(), want.to_naive_secs());
        let mut bad = all.to_vec();
        bad[6] = "0,1,2,3,4";
        assert!(with(&bad).err().unwrap().contains("--channels"));
    }
}
