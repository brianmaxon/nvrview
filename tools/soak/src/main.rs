//! Soak binary: `soak [--recording FILE] [--speed N] [--hours H | --seconds S]
//! [--tiles N] [--sample-secs N] [--no-audio] [--csv FILE]`. Without
//! `--recording` it uses a synthetic stream. Prints a Markdown report.

use soak::{SoakConfig, load, run, synth};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

const USAGE: &str = "usage: soak [--recording FILE.nvrrec] [--speed N] [--hours H | --seconds S]\n\
    \x20           [--tiles N] [--sample-secs N] [--no-audio] [--csv FILE]\n\
    defaults: synthetic stream, speed 60, 1 hour simulated, 1 tile";

fn parse<T: std::str::FromStr>(a: &str, v: &str) -> Result<T, String> {
    v.parse().map_err(|_| format!("bad value for {a}: {v}"))
}

fn main_inner() -> Result<(), String> {
    let (mut recording, mut speed, mut secs) = (None::<PathBuf>, 60.0f64, 3600.0f64);
    let (mut tiles, mut sample, mut audio, mut csv) = (1u8, 10.0f64, true, None::<PathBuf>);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--help" | "-h" => {
                println!("{USAGE}");
                return Ok(());
            }
            "--no-audio" => audio = false,
            _ => {
                let v = args.next().ok_or(format!("{a} needs a value"))?;
                match a.as_str() {
                    "--recording" => recording = Some(PathBuf::from(v)),
                    "--speed" => speed = parse(&a, &v)?,
                    "--hours" => secs = parse::<f64>(&a, &v)? * 3600.0,
                    "--seconds" => secs = parse(&a, &v)?,
                    "--tiles" => tiles = parse(&a, &v)?,
                    "--sample-secs" => sample = parse(&a, &v)?,
                    "--csv" => csv = Some(PathBuf::from(v)),
                    _ => return Err(format!("unknown argument {a}\n{USAGE}")),
                }
            }
        }
    }
    let replay = match recording {
        Some(p) => load(&p)?,
        None => synth::recording(&synth::fixtures_dir(), 600, 15)?,
    };
    let mut cfg = SoakConfig::new(replay, speed, Duration::from_secs_f64(secs));
    cfg.tiles = tiles;
    cfg.sample_every = Duration::from_secs_f64(sample);
    cfg.audio = audio;
    let report = run(&cfg)?;
    print!("{}", report.to_markdown());
    if let Some(p) = csv {
        std::fs::write(&p, report.memory_csv()).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    Ok(())
}

fn main() -> ExitCode {
    match main_inner() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("soak: {e}");
            ExitCode::FAILURE
        }
    }
}
