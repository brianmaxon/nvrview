//! `playback-probe` end to end: the real viewer-core against the in-process
//! mock NVR over loopback TCP, with the script's holds scaled down so the
//! run takes seconds. Checks that the script ran through the core, that
//! the cameras released pictures, and that every play was stopped and the
//! device's login released afterwards, also after an interrupt and at the
//! run cap.

use bc_cli::playback_probe::{self, Outcome, ProbeConfig};
use bc_protocol::archive::DateTime;
use mock_nvr::archive::ArchiveConfig;
use mock_nvr::fixture::Fixture;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

fn d(day: u8, h: u8, m: u8, s: u8) -> DateTime {
    DateTime::new(2026, 10, day, h, m, s).unwrap()
}

fn mock(archive: bool) -> mock_nvr::MockNvr {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/h264-basic.h264");
    let mut cfg = mock_nvr::Config::new(Fixture::load(&path).unwrap());
    cfg.channels = 3;
    if archive {
        let mut a = ArchiveConfig::new(vec![d(8, 0, 0, 0)]);
        a.events_per_day = 0;
        a.utc_offset_s = 3600;
        a.play_end_message = false;
        cfg.archive = Some(a);
    }
    mock_nvr::spawn(cfg).unwrap()
}

fn probe_cfg(nvr: &mock_nvr::MockNvr) -> ProbeConfig {
    let addr = nvr.addr();
    let mut c = ProbeConfig::new(
        &addr.ip().to_string(),
        addr.port(),
        "example-user",
        d(8, 12, 0, 0),
        3600,
        vec![0, 1],
    );
    c.scale = 0.1;
    c
}

fn wait_idle(nvr: &mock_nvr::MockNvr) {
    for _ in 0..200 {
        if nvr.active_logins() == 0 && nvr.active_plays() == 0 {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "left running: logins {}, plays {}",
        nvr.active_logins(),
        nvr.active_plays()
    );
}

fn run(
    cfg: &ProbeConfig,
    stop: Arc<AtomicBool>,
    out: &mut Vec<u8>,
) -> Result<playback_probe::Report, String> {
    playback_probe::run(
        cfg,
        "example-password",
        playback_probe::init_logging(),
        stop,
        out,
    )
}

#[test]
fn runs_the_script_and_leaves_nothing_running() {
    let nvr = mock(true);
    let cfg = probe_cfg(&nvr);
    let mut out = Vec::new();
    let r = run(&cfg, Arc::new(AtomicBool::new(false)), &mut out).unwrap();
    let text = String::from_utf8_lossy(&out).into_owned();
    println!("{text}{}", playback_probe::format_summary(&r));
    assert_eq!(r.outcome, Outcome::Completed, "{text}");
    assert!(r.closed);
    let names: Vec<&str> = r.segments.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names.len(), 8, "{names:?}");
    assert!(names[1].starts_with("play 1x") && names[7] == "resume");
    // The cameras released pictures while playing at 1x.
    for c in &r.segments[1].cams {
        assert!(c.frames > 0 && c.first_ms.is_some(), "{c:?}\n{text}");
    }
    assert!(text.contains("== rate 16x"), "{text}");
    // The engine's lines are counted: it started plays during the run.
    let starts: usize = r.segments.iter().map(|s| s.plays_started).sum();
    assert!(starts >= 3, "{text}");
    assert!(playback_probe::format_summary(&r).contains("session closed: yes"));
    // Plays stopped and the session closed; the rates reached the device.
    wait_idle(&nvr);
    assert!(nvr.command_count(7) >= 2);
    let speeds: Vec<Option<u32>> = nvr.play_log().iter().map(|l| l.speed).collect();
    assert!(
        speeds.contains(&Some(4)) && speeds.contains(&Some(16)),
        "{speeds:?}"
    );
}

#[test]
fn an_interrupt_stops_every_play_and_closes_the_session() {
    let nvr = mock(true);
    let mut cfg = probe_cfg(&nvr);
    cfg.scale = 1.0;
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(6));
        flag.store(true, Ordering::SeqCst);
    });
    let mut out = Vec::new();
    let r = run(&cfg, stop, &mut out).unwrap();
    t.join().unwrap();
    assert_eq!(r.outcome, Outcome::Interrupted);
    assert!(r.closed);
    wait_idle(&nvr);
}

#[test]
fn the_run_cap_ends_the_run_cleanly() {
    let nvr = mock(true);
    let mut cfg = probe_cfg(&nvr);
    cfg.scale = 1.0;
    cfg.run_limit = Duration::from_secs(5);
    let mut out = Vec::new();
    // The cap may fall in the index wait (an interrupted report) or in the
    // script; either way nothing is left running.
    let r = run(&cfg, Arc::new(AtomicBool::new(false)), &mut out).unwrap();
    assert_ne!(r.outcome, Outcome::Completed);
    assert!(r.closed);
    wait_idle(&nvr);
}

#[test]
fn a_device_without_recordings_is_reported_and_closed() {
    let nvr = mock(false);
    let cfg = probe_cfg(&nvr);
    let mut out = Vec::new();
    let r = run(&cfg, Arc::new(AtomicBool::new(false)), &mut out);
    assert!(r.is_err(), "{r:?}");
    wait_idle(&nvr);
}
