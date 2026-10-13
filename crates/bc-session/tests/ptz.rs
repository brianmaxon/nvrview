//! Camera motion control against the in-process mock NVR's PTZ model, in
//! simulated time: what is sent, the stop on every path, the limits, and
//! the grants.

use crate::common::mock_connector;
use bc_protocol::ptz::{Direction, LensAxis, Request};
use bc_session::{
    ConnState, DeviceSession, MotionOrigin, PtzNotice, PtzOutcome, PtzRefusal, SessionConfig,
    StopCause,
};
use mock_nvr::fixture::Fixture;
use mock_nvr::ptz::{PtzCamera, PtzLogEntry, PtzRequest};
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::time::Instant;

/// A pan-tilt-zoom camera with three presets.
const PTZ: u8 = 1;
/// A camera with only a motorized zoom and focus lens.
const LENS: u8 = 2;
/// Channel 0 is a fixed camera.
const FIXED: u8 = 0;

const MANUAL: MotionOrigin = MotionOrigin::Manual;

fn spawn_mock() -> mock_nvr::MockNvr {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/h264-basic.h264");
    let mut cfg = mock_nvr::Config::new(Fixture::load(&path).unwrap());
    cfg.channels = 3;
    cfg.ptz.insert(PTZ, PtzCamera::pan_tilt_zoom(3));
    cfg.ptz.insert(LENS, PtzCamera::lens_only());
    mock_nvr::spawn_in(cfg, &tokio::runtime::Handle::current())
}

fn session(nvr: &mock_nvr::MockNvr) -> DeviceSession {
    let mut cfg = SessionConfig::new("example-user", "example-password");
    cfg.query_timeout = Duration::from_secs(5);
    DeviceSession::spawn(mock_connector(nvr), cfg)
}

fn auto(name: &str) -> MotionOrigin {
    MotionOrigin::Automation(name.to_owned())
}

/// The 18 commands the mock received for `channel`, in order.
fn controls(nvr: &mock_nvr::MockNvr, channel: u8) -> Vec<String> {
    nvr.ptz_log()
        .iter()
        .filter(|e| e.channel == Some(channel))
        .filter_map(|e| e.control().map(str::to_owned))
        .collect()
}

fn motion_commands(nvr: &mock_nvr::MockNvr, channel: u8) -> Vec<PtzLogEntry> {
    nvr.ptz_log()
        .into_iter()
        .filter(|e| e.channel == Some(channel) && matches!(e.request, PtzRequest::Motion(_)))
        .collect()
}

/// Waits (in simulated time) for the mock's PTZ log to satisfy `f`.
async fn wait_for(nvr: &mock_nvr::MockNvr, what: &str, f: impl Fn(&mock_nvr::MockNvr) -> bool) {
    let t0 = Instant::now();
    while !f(nvr) {
        assert!(t0.elapsed() < Duration::from_secs(60), "{what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn moving(nvr: &mock_nvr::MockNvr, channel: u8) -> bool {
    nvr.ptz_state(channel).unwrap().moving.is_some()
}

async fn notice(rx: &mut broadcast::Receiver<PtzNotice>) -> PtzNotice {
    tokio::time::timeout(Duration::from_secs(60), rx.recv())
        .await
        .expect("no stop notice")
        .unwrap()
}

#[tokio::test(start_paused = true)]
async fn a_move_and_its_release_send_the_observed_commands() {
    let nvr = spawn_mock();
    let s = session(&nvr);
    let mut rx = s.ptz_notices();
    assert_eq!(
        s.ptz_move(PTZ, Direction::Left, 40, MANUAL).await,
        Ok(PtzOutcome::Moving)
    );
    assert!(moving(&nvr, PTZ));
    // The capabilities were read first, on the same connection.
    assert_eq!(nvr.command_count(199), 1);
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(s.ptz_stop(PTZ, MANUAL).await, Ok(PtzOutcome::Stopped));
    assert!(!moving(&nvr, PTZ));
    let st = nvr.ptz_state(PTZ).unwrap();
    assert!(st.pan > 300.0, "panned left from 0: {}", st.pan);
    assert_eq!((st.moves, st.stops), (1, 1));
    let log = motion_commands(&nvr, PTZ);
    assert_eq!(
        log.iter().map(|e| e.request.clone()).collect::<Vec<_>>(),
        vec![
            PtzRequest::Motion(Request::Control {
                channel: PTZ,
                command: "left".to_owned(),
                speed: 40
            }),
            PtzRequest::Motion(Request::Control {
                channel: PTZ,
                command: "stop".to_owned(),
                speed: 1
            }),
        ]
    );
    let n = notice(&mut rx).await;
    assert_eq!((n.cam.channel, n.cause), (PTZ, StopCause::Released));
    assert_eq!(nvr.ptz_config_requests(), 0);
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn renewals_are_not_sent_and_the_deadman_stops_a_forgotten_move() {
    let nvr = spawn_mock();
    let s = session(&nvr);
    let mut rx = s.ptz_notices();
    s.ptz_move(PTZ, Direction::Up, 20, MANUAL).await.unwrap();
    // Held for 5 s, renewed every 400 ms: one move on the wire.
    for _ in 0..12 {
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(
            s.ptz_move(PTZ, Direction::Up, 20, MANUAL).await,
            Ok(PtzOutcome::Renewed)
        );
        assert!(moving(&nvr, PTZ));
    }
    assert_eq!(controls(&nvr, PTZ), vec!["up"]);
    // The app stops renewing (it crashed, its window froze): stopped 1 s
    // after the last renewal.
    let last = Instant::now();
    wait_for(&nvr, "dead-man stop", |n| !moving(n, PTZ)).await;
    let after = last.elapsed();
    assert!(
        after >= Duration::from_millis(1000) && after < Duration::from_millis(1100),
        "{after:?}"
    );
    assert_eq!(controls(&nvr, PTZ), vec!["up", "stop"]);
    let n = notice(&mut rx).await;
    assert_eq!(n.cause, StopCause::Deadman);
    assert_eq!(n.origin, Some(MANUAL));
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn a_move_held_too_long_is_stopped_and_refused_until_released() {
    let nvr = spawn_mock();
    let s = session(&nvr);
    let mut rx = s.ptz_notices();
    let t0 = Instant::now();
    s.ptz_move(PTZ, Direction::Right, 20, MANUAL).await.unwrap();
    let mut refused = None;
    while t0.elapsed() < Duration::from_secs(40) {
        tokio::time::sleep(Duration::from_millis(400)).await;
        if let Err(e) = s.ptz_move(PTZ, Direction::Right, 20, MANUAL).await {
            refused.get_or_insert((e, t0.elapsed()));
        }
    }
    let (why, at) = refused.expect("the hold was never cut");
    assert_eq!(why, PtzRefusal::MaxHold);
    assert!(
        at >= Duration::from_secs(30) && at < Duration::from_secs(31),
        "{at:?}"
    );
    assert!(!moving(&nvr, PTZ));
    assert_eq!(controls(&nvr, PTZ), vec!["right", "stop"]);
    assert_eq!(notice(&mut rx).await.cause, StopCause::MaxHold);
    // Released, pressed again: moves.
    s.ptz_stop(PTZ, MANUAL).await.unwrap();
    assert_eq!(
        s.ptz_move(PTZ, Direction::Right, 20, MANUAL).await,
        Ok(PtzOutcome::Moving)
    );
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn a_new_direction_stops_the_old_one_first() {
    let nvr = spawn_mock();
    let s = session(&nvr);
    s.ptz_move(PTZ, Direction::Left, 20, MANUAL).await.unwrap();
    s.ptz_move(PTZ, Direction::Down, 20, MANUAL).await.unwrap();
    s.ptz_stop(PTZ, MANUAL).await.unwrap();
    assert_eq!(controls(&nvr, PTZ), vec!["left", "stop", "down", "stop"]);
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn preset_recall_reads_the_list_and_sends_to_pos_only() {
    let nvr = spawn_mock();
    let s = session(&nvr);
    assert_eq!(s.ptz_recall(PTZ, 2, MANUAL).await, Ok(PtzOutcome::Recalled));
    let st = nvr.ptz_state(PTZ).unwrap();
    assert_eq!(st.last_preset, Some(2));
    assert_eq!(st.pan, 60.0);
    // The list (190) was read first; the recall carried the preset's name.
    assert_eq!(nvr.command_count(190), 1);
    match &motion_commands(&nvr, PTZ)[0].request {
        PtzRequest::Motion(Request::Preset {
            id, command, name, ..
        }) => {
            assert_eq!((*id, command.as_str()), (2, "toPos"));
            assert_eq!(name.as_deref(), Some("Preset 3"));
        }
        other => panic!("{other:?}"),
    }
    // An id the list lacks is refused without sending anything.
    assert_eq!(
        s.ptz_recall(PTZ, 9, MANUAL).await,
        Err(PtzRefusal::UnknownPreset)
    );
    assert_eq!(motion_commands(&nvr, PTZ).len(), 1);
    // The lens is read again about 6 s later, when the camera settled.
    let reads = nvr.command_count(294);
    tokio::time::sleep(Duration::from_millis(5900)).await;
    assert_eq!(nvr.command_count(294), reads);
    wait_for(&nvr, "lens re-read", |n| n.command_count(294) == reads + 1).await;
    assert_eq!(nvr.ptz_config_requests(), 0);
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn lens_moves_are_clamped_to_the_reported_range() {
    let nvr = spawn_mock();
    let s = session(&nvr);
    assert_eq!(
        s.ptz_lens(LENS, LensAxis::Zoom, 99, MANUAL).await,
        Ok(PtzOutcome::Lens(28))
    );
    assert_eq!(
        s.ptz_lens(LENS, LensAxis::Focus, -5, MANUAL).await,
        Ok(PtzOutcome::Lens(0))
    );
    let st = nvr.ptz_state(LENS).unwrap();
    assert_eq!((st.zoom, st.focus), (Some(28), Some(0)));
    // Every lens move the device saw was in range: none refused.
    assert!(motion_commands(&nvr, LENS).iter().all(|e| e.status == 200));
    // A lens-only camera is sent no pan, tilt or preset.
    assert_eq!(
        s.ptz_move(LENS, Direction::Left, 20, MANUAL).await,
        Err(PtzRefusal::NotSupported)
    );
    assert_eq!(
        s.ptz_recall(LENS, 0, MANUAL).await,
        Err(PtzRefusal::NotSupported)
    );
    // A fixed camera is sent nothing at all, not even a stop.
    assert_eq!(
        s.ptz_move(FIXED, Direction::Left, 20, MANUAL).await,
        Err(PtzRefusal::NotSupported)
    );
    assert_eq!(
        s.ptz_lens(FIXED, LensAxis::Zoom, 1, MANUAL).await,
        Err(PtzRefusal::NotSupported)
    );
    assert_eq!(
        s.ptz_stop(FIXED, MANUAL).await,
        Err(PtzRefusal::NotSupported)
    );
    assert_eq!(motion_commands(&nvr, LENS).len(), 2);
    assert!(nvr.ptz_log().iter().all(|e| e.channel != Some(FIXED)));
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn a_runaway_caller_is_limited_and_told_why() {
    let nvr = spawn_mock();
    let s = session(&nvr);
    let dirs = [Direction::Left, Direction::Right];
    let mut refused = None;
    for i in 0..60 {
        if let Err(e) = s.ptz_move(PTZ, dirs[i % 2], 20, MANUAL).await {
            refused = Some((i, e));
            break;
        }
    }
    let (i, why) = refused.expect("never limited");
    assert!(matches!(why, PtzRefusal::RateLimited { .. }), "{why:?}");
    // About the burst of 40 got through (a little refills meanwhile).
    assert!((40..45).contains(&i), "{i}");
    let moves = controls(&nvr, PTZ).iter().filter(|c| *c != "stop").count();
    assert_eq!(moves, i);
    // Stop is never limited.
    assert_eq!(s.ptz_stop(PTZ, MANUAL).await, Ok(PtzOutcome::Stopped));
    assert!(!moving(&nvr, PTZ));
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn automation_needs_a_grant_and_yields_to_the_user() {
    let nvr = spawn_mock();
    let s = session(&nvr);
    let mut rx = s.ptz_notices();
    let api = auto("example-client");
    assert_eq!(
        s.ptz_recall(PTZ, 0, api.clone()).await,
        Err(PtzRefusal::NotGranted)
    );
    assert_eq!(
        s.ptz_move(PTZ, Direction::Left, 20, api.clone()).await,
        Err(PtzRefusal::NotGranted)
    );
    assert!(motion_commands(&nvr, PTZ).is_empty());
    // A grant is per camera and per client.
    s.set_motion_grant(PTZ, "example-client", true);
    assert_eq!(s.motion_grants(PTZ), vec!["example-client".to_owned()]);
    assert!(s.motion_grants(LENS).is_empty());
    assert_eq!(
        s.ptz_lens(LENS, LensAxis::Zoom, 3, api.clone()).await,
        Err(PtzRefusal::NotGranted)
    );
    assert_eq!(
        s.ptz_move(PTZ, Direction::Left, 20, auto("other")).await,
        Err(PtzRefusal::NotGranted)
    );
    assert_eq!(
        s.ptz_move(PTZ, Direction::Left, 20, api.clone()).await,
        Ok(PtzOutcome::Moving)
    );
    // The user steers: automation is refused while and just after.
    s.ptz_move(PTZ, Direction::Up, 20, MANUAL).await.unwrap();
    assert_eq!(notice(&mut rx).await.cause, StopCause::Replaced);
    assert_eq!(
        s.ptz_recall(PTZ, 1, api.clone()).await,
        Err(PtzRefusal::UserSteering)
    );
    s.ptz_stop(PTZ, MANUAL).await.unwrap();
    assert_eq!(notice(&mut rx).await.cause, StopCause::Released);
    tokio::time::sleep(Duration::from_secs(5)).await;
    // Withdrawing the grant stops the automation's move at once.
    s.ptz_move(PTZ, Direction::Down, 20, api.clone())
        .await
        .unwrap();
    s.set_motion_grant(PTZ, "example-client", false);
    let n = notice(&mut rx).await;
    assert_eq!(
        (n.cause, n.origin),
        (StopCause::GrantWithdrawn, Some(api.clone()))
    );
    assert!(!moving(&nvr, PTZ));
    assert_eq!(
        s.ptz_move(PTZ, Direction::Down, 20, api).await,
        Err(PtzRefusal::NotGranted)
    );
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn a_refused_move_is_still_followed_by_a_stop() {
    let nvr = spawn_mock();
    nvr.set_ptz_status(500);
    let s = session(&nvr);
    let mut rx = s.ptz_notices();
    assert_eq!(
        s.ptz_move(PTZ, Direction::Left, 20, MANUAL).await,
        Err(PtzRefusal::DeviceRefused(500))
    );
    wait_for(&nvr, "stop after refusal", |n| {
        controls(n, PTZ) == ["left", "stop"]
    })
    .await;
    assert_eq!(notice(&mut rx).await.cause, StopCause::DeviceRefused);
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn a_move_the_connection_lost_is_stopped_after_the_next_login() {
    let nvr = spawn_mock();
    let s = session(&nvr);
    let mut rx = s.ptz_notices();
    let mut state = s.watch_state();
    s.ptz_move(PTZ, Direction::Left, 20, MANUAL).await.unwrap();
    nvr.drop_connections();
    // The camera keeps moving while nobody can reach it...
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(moving(&nvr, PTZ));
    // ...and is stopped as soon as the session is back.
    wait_for(&nvr, "stop after reconnect", |n| !moving(n, PTZ)).await;
    let n = notice(&mut rx).await;
    assert_eq!(n.cause, StopCause::Reconnected);
    let log = motion_commands(&nvr, PTZ);
    assert_eq!(log.len(), 2);
    assert_ne!(log[0].connection, log[1].connection);
    assert_eq!(log[1].control(), Some("stop"));
    state
        .wait_for(|s| *s == ConnState::Connected)
        .await
        .unwrap();
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn closing_or_dropping_the_session_stops_every_moving_camera() {
    let nvr = spawn_mock();
    let s = session(&nvr);
    let mut rx = s.ptz_notices();
    s.ptz_move(PTZ, Direction::Left, 20, MANUAL).await.unwrap();
    s.close().await;
    assert!(!moving(&nvr, PTZ));
    assert_eq!(notice(&mut rx).await.cause, StopCause::SessionClosed);
    assert_eq!(controls(&nvr, PTZ), vec!["left", "stop"]);

    let s = session(&nvr);
    s.ptz_move(PTZ, Direction::Up, 20, MANUAL).await.unwrap();
    drop(s);
    wait_for(&nvr, "stop after drop", |n| !moving(n, PTZ)).await;
    assert_eq!(controls(&nvr, PTZ), vec!["left", "stop", "up", "stop"]);
    wait_for(&nvr, "logged out", |n| n.active_logins() == 0).await;
}

#[tokio::test(start_paused = true)]
async fn a_capabilities_reply_with_status_300_is_a_success() {
    // Newer firmware answers 199 with status 300 and a complete body.
    let nvr = spawn_mock();
    nvr.set_support_status(300);
    let s = session(&nvr);
    assert_eq!(
        s.ptz_move(PTZ, Direction::Left, 40, MANUAL).await,
        Ok(PtzOutcome::Moving)
    );
    assert!(moving(&nvr, PTZ));
    assert_eq!(s.ptz_stop(PTZ, MANUAL).await, Ok(PtzOutcome::Stopped));
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn a_move_refused_before_sending_owes_no_stop_after_a_reconnect() {
    let nvr = spawn_mock();
    nvr.set_support_status(500);
    let s = session(&nvr);
    let mut state = s.watch_state();
    assert_eq!(
        s.ptz_move(PTZ, Direction::Left, 40, MANUAL).await,
        Err(PtzRefusal::QueryFailed)
    );
    assert!(PtzRefusal::QueryFailed.sent_nothing());
    // A stop in flight when the connection drops is not owed again: no
    // camera ever moved.
    s.ptz_stop_now(PTZ, MANUAL);
    nvr.drop_connections();
    state
        .wait_for(|s| *s != ConnState::Connected)
        .await
        .unwrap();
    state
        .wait_for(|s| *s == ConnState::Connected)
        .await
        .unwrap();
    s.close().await;
    let c = controls(&nvr, PTZ);
    assert!(c.iter().filter(|c| *c == "stop").count() <= 1, "{c:?}");
    assert!(!c.iter().any(|c| c == "left"));
}

#[tokio::test(start_paused = true)]
async fn info_reads_capabilities_lens_and_presets() {
    let nvr = spawn_mock();
    let s = session(&nvr);
    let info = s.ptz_info(PTZ).await.unwrap();
    assert!(info.caps.pan_tilt && info.caps.presets && info.caps.zoom_focus);
    assert_eq!(info.zoom.unwrap().max, 6000);
    assert_eq!(info.presets.len(), 3);
    assert_eq!(info.self_test, Some(0));
    let info = s.ptz_info(LENS).await.unwrap();
    assert!(!info.caps.pan_tilt && info.caps.zoom_focus);
    assert_eq!(info.focus.unwrap().max, 164);
    assert!(info.presets.is_empty() && info.self_test.is_none());
    let info = s.ptz_info(FIXED).await.unwrap();
    assert!(!info.caps.any());
    // Reads only: nothing moved.
    assert!(
        nvr.ptz_log()
            .iter()
            .all(|e| matches!(e.request, PtzRequest::Query(_)))
    );
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn commands_handed_over_without_waiting_keep_their_order() {
    use bc_session::PtzCommand;
    let nvr = spawn_mock();
    let s = session(&nvr);
    let mv = s.ptz_send(
        PTZ,
        PtzCommand::Move {
            direction: Direction::Left,
            speed: 20,
        },
        MANUAL,
    );
    // The release is handed over at once and its result never awaited:
    // it still goes out, after the move.
    drop(s.ptz_send(PTZ, PtzCommand::Stop, MANUAL));
    assert_eq!(mv.outcome().await, Ok(PtzOutcome::Moving));
    wait_for(&nvr, "stop", |n| controls(n, PTZ) == ["left", "stop"]).await;
    assert!(!moving(&nvr, PTZ));
    s.close().await;
}
