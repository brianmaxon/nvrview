//! Command-line mock NVR. The password comes from the `MOCK_NVR_PASSWORD`
//! environment variable (default `example-password`), never from an argument.

use mock_nvr::{Config, fixture::Fixture, spawn};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: mock-nvr [--bind ADDR] [--port N] [--channels N] [--fixture FILE]\n\
    \x20               [--user NAME] [--max-logins N] [--fps N] [--width N] [--height N]\n\
    password: environment variable MOCK_NVR_PASSWORD (default example-password)";

fn run() -> Result<(), String> {
    let mut bind: IpAddr = [127, 0, 0, 1].into();
    let mut port = 9000u16;
    let mut fixture = PathBuf::from("fixtures/synthetic/h264-basic.h264");
    let (mut channels, mut max_logins, mut fps) = (4u8, 4usize, 25u32);
    let (mut width, mut height) = (320u32, 240u32);
    let mut user = String::from("example-user");
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--help" || a == "-h" {
            println!("{USAGE}");
            return Ok(());
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
            _ => return Err(format!("unknown argument {a}\n{USAGE}")),
        }
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
