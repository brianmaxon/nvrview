//! bc-cli: log in to an NVR or camera, list its channels, record one live
//! channel to a raw `.h264`/`.h265` file that ffplay and VLC can open,
//! capture several tiers of a channel for timing analysis, or analyze such a
//! capture.
//!
//! The password is never taken from the command line (it would end up in
//! shell history and process listings): it comes from the environment
//! variable `NVRVIEW_PASSWORD` or, on macOS, from a Keychain item.

use bc_cli::analyze::Analysis;
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
  bc-cli analyze FILE.nvrrec

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

async fn run(args: Args) -> Result<(), String> {
    let password = password(args.keychain.as_deref())?;
    let addr = resolve(&args.host).await?;
    let session = DeviceSession::spawn(
        TcpConnector::new(addr),
        SessionConfig::new(args.user, password),
    );
    let result = run_action(&session, args.action).await;
    // Stops previews and closes the socket before the process exits.
    session.close().await;
    result.map_err(|e| e.to_string())
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
    let args = match parse_args(std::env::args().skip(1)) {
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
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("NVRVIEW_LOG")
                .unwrap_or_else(|_| "warn".into()),
        )
        .init();
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("bc-cli: cannot start the runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(args)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("bc-cli: {e}");
            ExitCode::FAILURE
        }
    }
}
