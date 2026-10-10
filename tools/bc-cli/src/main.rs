//! bc-cli: log in to an NVR or camera, list its channels, or record one live
//! channel to a raw `.h264`/`.h265` file that ffplay and VLC can open.
//!
//! The password is never taken from the command line (it would end up in
//! shell history and process listings): it comes from the environment
//! variable `NVRVIEW_PASSWORD` or, on macOS, from a Keychain item.

use bc_cli::{CliError, RecordLimits, list_channels, record, wait_logged_in};
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
         [--seconds S] [--frames N]

The port defaults to 9000. The user name may also come from NVRVIEW_USER.
The password comes from NVRVIEW_PASSWORD or, on macOS, from the Keychain
item named with --keychain; never from the command line.
record writes video only (Annex-B), starting at a keyframe, until Ctrl-C,
--seconds, or --frames.";

/// How long to wait for the login before giving up.
const LOGIN_LIMIT: Duration = Duration::from_secs(20);

enum Action {
    Channels,
    Record {
        channel: u8,
        tier: StreamTier,
        out: PathBuf,
        seconds: Option<u64>,
        frames: Option<u64>,
    },
}

struct Args {
    host: String,
    user: String,
    keychain: Option<String>,
    action: Action,
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut host = None;
    let mut user = std::env::var("NVRVIEW_USER").ok();
    let mut keychain = None;
    let mut command = None;
    let (mut channel, mut tier, mut out, mut seconds, mut frames) =
        (None, StreamTier::Sub, None, None, None);
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
            "--tier" => {
                tier = match value("--tier")?.as_str() {
                    "main" => StreamTier::Main,
                    "sub" => StreamTier::Sub,
                    "extern" => StreamTier::Extern,
                    "mobile" => StreamTier::Mobile,
                    other => return Err(format!("unknown tier {other}")),
                }
            }
            "--out" => out = Some(PathBuf::from(value("--out")?)),
            "--seconds" => {
                seconds = Some(value("--seconds")?.parse().map_err(|_| "bad --seconds")?);
            }
            "--frames" => {
                frames = Some(value("--frames")?.parse().map_err(|_| "bad --frames")?);
            }
            "-h" | "--help" => return Err(String::new()),
            "channels" | "record" if command.is_none() => command = Some(a),
            other => return Err(format!("unexpected argument {other}")),
        }
    }
    let action = match command.as_deref() {
        Some("channels") => Action::Channels,
        Some("record") => Action::Record {
            channel: channel.ok_or("record needs --channel")?,
            tier,
            out: out.ok_or("record needs --out")?,
            seconds,
            frames,
        },
        _ => return Err("missing command".into()),
    };
    Ok(Args {
        host: host.ok_or("missing --host")?,
        user: user.ok_or("missing --user (or NVRVIEW_USER)")?,
        keychain,
        action,
    })
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
        Action::Record {
            channel,
            tier,
            out,
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
            let stats = record(
                session,
                channel,
                tier,
                &mut file,
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
    }
}

fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(a) => a,
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
