//! `ptz-probe` end to end against the in-process mock NVR's PTZ model, on
//! tokio's paused clock: what it sends, that nothing moves before the typed
//! confirmation, the cap on the held move, and the stop on every exit path
//! (an interrupt, an error, a dropped run).

use bc_cli::ptz_probe::{self, MOTION_CAP, ProbeConfig, Report, Step};
use bc_cli::wait_logged_in;
use bc_protocol::ptz::{Direction, Request};
use bc_session::{DeviceSession, FnConnector, PtzRefusal, StopCause};
use mock_nvr::fixture::Fixture;
use mock_nvr::ptz::{PtzCamera, PtzRequest};
use std::future::pending;
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::{Instant, sleep, timeout};

/// A pan-tilt-zoom camera with three presets.
const PTZ: u8 = 1;
/// A camera with only a motorized zoom and focus lens.
const LENS: u8 = 2;

fn spawn_mock() -> mock_nvr::MockNvr {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/h264-basic.h264");
    let mut cfg = mock_nvr::Config::new(Fixture::load(&path).unwrap());
    cfg.channels = 3;
    cfg.ptz.insert(PTZ, PtzCamera::pan_tilt_zoom(3));
    cfg.ptz.insert(LENS, PtzCamera::lens_only());
    mock_nvr::spawn_in(cfg, &tokio::runtime::Handle::current())
}

async fn session(nvr: &mock_nvr::MockNvr, cfg: &ProbeConfig) -> DeviceSession {
    let addr = nvr.addr();
    let s = DeviceSession::spawn(
        FnConnector(move || mock_nvr::connect(addr)),
        ptz_probe::session_config("example-user", "example-password", cfg.hold),
    );
    wait_logged_in(&s, Duration::from_secs(10)).await.unwrap();
    s
}

async fn run_with(
    s: &DeviceSession,
    cfg: &ProbeConfig,
    input: &str,
) -> (Result<Report, String>, String) {
    let mut out = Vec::new();
    let r = ptz_probe::run(s, cfg, input.as_bytes(), &mut out, pending()).await;
    (r, String::from_utf8(out).unwrap())
}

fn motion(nvr: &mock_nvr::MockNvr, channel: u8) -> Vec<Request> {
    nvr.ptz_log()
        .into_iter()
        .filter(|e| e.channel == Some(channel))
        .filter_map(|e| match e.request {
            PtzRequest::Motion(r) => Some(r),
            _ => None,
        })
        .collect()
}

fn controls(nvr: &mock_nvr::MockNvr, channel: u8) -> Vec<String> {
    nvr.ptz_log()
        .iter()
        .filter(|e| e.channel == Some(channel))
        .filter_map(|e| e.control().map(str::to_owned))
        .collect()
}

async fn wait_still(nvr: &mock_nvr::MockNvr, channel: u8) {
    let t0 = Instant::now();
    while nvr.ptz_state(channel).unwrap().moving.is_some() {
        assert!(t0.elapsed() < Duration::from_secs(30), "still moving");
        sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(start_paused = true)]
async fn the_full_probe_moves_briefly_and_the_core_stops_it() {
    let nvr = spawn_mock();
    let cfg = ProbeConfig::new(PTZ);
    let s = session(&nvr, &cfg).await;
    let (r, out) = run_with(&s, &cfg, "yes\n").await;
    let report = r.unwrap();
    assert!(report.confirmed);

    // (a) one move at the lowest speed, ended by the core's dead-man timer.
    let held = report.held.unwrap();
    assert!(held.moved.is_ok());
    let (cause, after) = held.stop.expect("the core's stop");
    assert_eq!(cause, StopCause::Deadman);
    assert!(after >= cfg.hold && after <= MOTION_CAP, "{after:?}");
    assert!(!held.forced_stop);
    assert!(nvr.ptz_state(PTZ).unwrap().moving.is_none());
    assert_eq!(
        controls(&nvr, PTZ).first().map(String::as_str),
        Some("left")
    );
    assert!(
        motion(&nvr, PTZ).iter().any(|m| matches!(
            m,
            Request::Control { command, speed, .. } if command == "left" && *speed == 1
        )),
        "the move was at speed 1"
    );
    assert_eq!(nvr.ptz_state(PTZ).unwrap().moves, 1);

    // (b) the unlisted preset was refused by the core and never sent.
    assert_eq!(report.unknown_preset, Some(Err(PtzRefusal::UnknownPreset)));
    assert!(
        !motion(&nvr, PTZ)
            .iter()
            .any(|m| matches!(m, Request::Preset { .. })),
        "no recall reached the device"
    );

    // (c) both motors went to the edge of the range and back.
    assert_eq!(report.lens.len(), 2);
    for l in &report.lens {
        assert!(l.moved.is_ok());
        assert_eq!(l.reached, Some(l.target), "{l:?}");
        assert_eq!(l.restored, Some(l.original), "{l:?}");
    }
    let state = nvr.ptz_state(PTZ).unwrap();
    assert_eq!(state.zoom, Some(1000));
    assert_eq!(state.focus, Some(50));

    // (d) is reported, not attempted; the output carries the timeline.
    assert!(out.contains("(d) not possible"), "{out}");
    assert!(
        out.contains("(a) the core stopped the move: deadman"),
        "{out}"
    );
    assert!(out.contains("Type yes"), "{out}");
    s.close().await;
    assert_eq!(nvr.active_logins(), 0);
}

#[tokio::test(start_paused = true)]
async fn nothing_is_sent_without_the_typed_yes() {
    for input in ["", "no\n", "y\n", "YES please\n"] {
        let nvr = spawn_mock();
        let cfg = ProbeConfig::new(PTZ);
        let s = session(&nvr, &cfg).await;
        let (r, out) = run_with(&s, &cfg, input).await;
        let report = r.unwrap();
        assert!(!report.confirmed, "{input:?}");
        assert!(out.contains("no motion command was sent"), "{out}");
        s.close().await;
        assert!(
            motion(&nvr, PTZ).is_empty(),
            "{input:?}: {:?}",
            motion(&nvr, PTZ)
        );
        assert_eq!(nvr.ptz_state(PTZ).unwrap().moves, 0);
    }
}

#[tokio::test(start_paused = true)]
async fn a_lens_number_is_refused_before_anything_is_sent() {
    let nvr = spawn_mock();
    let mut cfg = ProbeConfig::new(PTZ);
    cfg.lens = Some(1);
    let s = session(&nvr, &cfg).await;
    let (r, _) = run_with(&s, &cfg, "yes\n").await;
    assert!(r.unwrap_err().contains("lens field"));
    s.close().await;
    assert!(nvr.ptz_log().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_lens_only_camera_gets_no_pan_or_tilt() {
    let nvr = spawn_mock();
    let mut cfg = ProbeConfig::new(LENS);
    cfg.edge = ptz_probe::Edge::Min;
    let s = session(&nvr, &cfg).await;
    let (r, out) = run_with(&s, &cfg, "yes\n").await;
    let report = r.unwrap();
    assert!(report.held.is_none());
    assert!(out.contains("(a) skipped"), "{out}");
    assert!(out.contains("(b) skipped"), "{out}");
    assert!(controls(&nvr, LENS).is_empty());
    assert_eq!(report.lens.len(), 2);
    assert_eq!(report.lens[0].target, 0);
    assert_eq!(report.lens[0].restored, Some(0));
    assert_eq!(report.lens[1].restored, Some(80));
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn a_long_hold_is_capped_at_three_seconds() {
    assert!(ptz_probe::parse_hold("3").is_some());
    assert!(ptz_probe::parse_hold("3.1").is_none());
    assert!(ptz_probe::parse_hold("0").is_none());
    assert!(ptz_probe::parse_hold("-1").is_none());
    assert!(ptz_probe::parse_hold("nan").is_none());
    // Even a session configured with a longer dead-man timer is cut at the cap.
    let cfg = ptz_probe::session_config("u", "p", Duration::from_secs(60));
    assert_eq!(cfg.ptz.deadman, MOTION_CAP);
    assert_eq!(cfg.ptz.max_hold, MOTION_CAP);
}

#[tokio::test(start_paused = true)]
async fn an_interrupt_during_the_move_stops_the_camera() {
    let nvr = spawn_mock();
    let mut cfg = ProbeConfig::new(PTZ);
    cfg.steps = vec![Step::HeldMove];
    let s = session(&nvr, &cfg).await;
    let mut out = Vec::new();
    // Ctrl-C a little after the move starts (the confirmation is read at once).
    let t0 = Instant::now();
    let r = ptz_probe::run(&s, &cfg, &b"yes\n"[..], &mut out, async {
        sleep(Duration::from_millis(300)).await;
    })
    .await;
    assert_eq!(r.unwrap_err(), "interrupted");
    assert!(
        t0.elapsed() < cfg.hold,
        "stopped well before the dead-man timer"
    );
    wait_still(&nvr, PTZ).await;
    assert_eq!(controls(&nvr, PTZ).last().map(String::as_str), Some("stop"));
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn dropping_the_run_mid_move_still_stops_the_camera() {
    // The error and panic paths unwind through the same guard as a drop.
    let nvr = spawn_mock();
    let mut cfg = ProbeConfig::new(PTZ);
    cfg.steps = vec![Step::HeldMove];
    cfg.hold = Duration::from_secs(3);
    let s = session(&nvr, &cfg).await;
    let mut out = Vec::new();
    let r = timeout(
        Duration::from_millis(500),
        ptz_probe::run(&s, &cfg, &b"yes\n"[..], &mut out, pending()),
    )
    .await;
    assert!(r.is_err(), "the run was cut off");
    assert!(nvr.ptz_state(PTZ).unwrap().moves >= 1);
    wait_still(&nvr, PTZ).await;
    assert_eq!(controls(&nvr, PTZ).last().map(String::as_str), Some("stop"));
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn a_refused_move_is_reported_and_leaves_the_camera_still() {
    let nvr = spawn_mock();
    let mut cfg = ProbeConfig::new(PTZ);
    cfg.steps = vec![Step::HeldMove];
    cfg.direction = Direction::Up;
    let s = session(&nvr, &cfg).await;
    nvr.set_ptz_status(401);
    let (r, out) = run_with(&s, &cfg, "yes\n").await;
    let held = r.unwrap().held.unwrap();
    assert_eq!(held.moved, Err(PtzRefusal::DeviceRefused(401)));
    assert!(out.contains("the device refused it, status 401"), "{out}");
    wait_still(&nvr, PTZ).await;
    s.close().await;
}
