//! Recorded playback (docs/FFI-RECORDINGS.md, section 6; slice C10)
//! against in-process mock NVRs with a recording store, on a simulated
//! clock. The mock serves plays as the devices were observed to: one seek
//! position per session (a play whose file does not hold it gets nothing),
//! media tagged with the latest seek, keyframe seconds in UTC, the media
//! clock at `playSpeed`, main streams thinned to keyframes from 2x, and no
//! end-of-play message.

mod common;

use bc_protocol::archive::DateTime;
use common::*;
use mock_nvr::archive::ArchiveConfig;
use serde_json::{Value, json};
use simtime::SimRuntime;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use viewer_core::{Core, CoreConfig, PlaybackState, SeekMode, StreamTier};

/// The devices' offset from UTC: an hour east.
const OFFSET_S: i64 = 3600;

fn d(day: u8, h: u8, m: u8, s: u8) -> DateTime {
    DateTime::new(2026, 10, day, h, m, s).unwrap()
}

fn t(day: u8, h: u8, m: u8, s: u8) -> i64 {
    d(day, h, m, s).to_naive_secs()
}

fn ms(day: u8, h: u8, m: u8, s: u8) -> i64 {
    t(day, h, m, s) * 1000
}

/// A mock NVR with `channels` channels recording all of the 8th, as
/// `tweak` changes it.
fn archive_mock(
    sim: &SimRuntime,
    channels: u8,
    tweak: impl FnOnce(&mut ArchiveConfig),
) -> mock_nvr::MockNvr {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/h264-basic.h264");
    let mut cfg = mock_nvr::Config::new(mock_nvr::fixture::Fixture::load(&path).unwrap());
    cfg.channels = channels;
    let mut a = ArchiveConfig::new(vec![d(8, 0, 0, 0)]);
    a.events_per_day = 0;
    a.utc_offset_s = OFFSET_S;
    a.play_end_message = false;
    tweak(&mut a);
    cfg.archive = Some(a);
    sim_spawn(sim, cfg)
}

struct Rig {
    sim: SimRuntime,
    core: Arc<Core>,
    rec: Arc<Recorder>,
}

impl Rig {
    fn new() -> Rig {
        let sim = SimRuntime::new();
        let (core, rec) = start(on_sim(
            &sim,
            CoreConfig {
                test_wall_clock_start_us: Some(t(10, 12, 0, 0) * 1_000_000),
                ..fast_config()
            },
        ));
        core.recordings_configure("{}").unwrap();
        Rig { sim, core, rec }
    }

    /// Adds the device, enables it, and indexes the 8th of `channels`.
    fn add(&self, nvr: &mock_nvr::MockNvr, channels: &[u8]) -> (u64, String) {
        let addr = nvr.addr();
        let id = self
            .core
            .add_device(&addr.ip().to_string(), addr.port(), USER, PASSWORD)
            .unwrap();
        let key = self.core.device_key(id).unwrap();
        let chans: Vec<Value> = channels.iter().map(|c| json!({"channel": c})).collect();
        self.core
            .recordings_enable(
                &key,
                Some(&json!({"channels": chans, "utc_offset_s": OFFSET_S}).to_string()),
            )
            .unwrap();
        let cams: Vec<Value> = channels
            .iter()
            .map(|c| json!({"device": key, "channel": c}))
            .collect();
        self.core
            .recordings_want(&json!({"cameras": cams, "days": [t(8, 0, 0, 0)]}).to_string())
            .unwrap();
        let n = nvr.command_count(14);
        // The latest report counts: an idle one from before the fill began
        // says nothing about the searches (counted when the mock gets them,
        // not when it answers).
        self.until("the index", 30, || {
            nvr.command_count(14) >= n + channels.len() as u64
                && self
                    .rec
                    .events_of("recordings_index")
                    .iter()
                    .rev()
                    .find(|e| e["device"] == key.as_str())
                    .is_some_and(|e| e["state"] == "idle" && e["pending_days"] == 0)
        });
        (id, key)
    }

    fn until(&self, what: &str, secs: u64, cond: impl FnMut() -> bool) {
        sim_until(&self.sim, &self.core, what, secs, cond);
    }

    /// Runs a blocking core call on a thread of its own and moves the
    /// clock in small steps until it returns; fails (instead of hanging)
    /// if it needs more than `secs` of simulated time. Calls that should
    /// need no time return before the first step.
    fn bounded(&self, what: &str, secs: u64, f: impl FnOnce(&Core) + Send + 'static) {
        let core = Arc::clone(&self.core);
        let (done_tx, done) = std::sync::mpsc::channel();
        let caller = std::thread::spawn(move || {
            f(&core);
            let _ = done_tx.send(());
        });
        let start = self.sim.elapsed();
        let mut finished = false;
        self.sim.advance_until(
            what,
            Duration::from_secs(secs),
            Duration::from_millis(10),
            || {
                finished = finished || done.try_recv().is_ok();
                finished
            },
        );
        caller.join().unwrap();
        let took = self.sim.elapsed() - start;
        assert!(
            took < Duration::from_secs(secs),
            "{what} took {took:?} of simulated time"
        );
    }

    fn open(&self, json: Value) -> (u64, Vec<u64>) {
        let a: Value =
            serde_json::from_str(&self.core.playback_open(&json.to_string()).unwrap()).unwrap();
        let tiles = a["tiles"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_u64().unwrap())
            .collect();
        (a["playback"].as_u64().unwrap(), tiles)
    }

    fn events(&self, kind: &str, pb: u64) -> Vec<Value> {
        self.rec
            .events_of(kind)
            .into_iter()
            .filter(|e| e["playback"] == pb)
            .collect()
    }

    fn seek_active(&self, pb: u64, generation: u64) -> bool {
        self.events("playback_seek", pb)
            .iter()
            .any(|e| e["state"] == "active" && e["generation"] == generation)
    }

    fn camera_state(&self, pb: u64, tile: u64) -> Option<Value> {
        self.events("playback_camera", pb)
            .into_iter()
            .rev()
            .find(|e| e["tile"] == tile)
    }

    fn position(&self, pb: u64) -> (i64, PlaybackState) {
        self.core.playback_position(pb).unwrap()
    }
}

fn cams(key: &str, channels: &[u8], tier: &str) -> Vec<Value> {
    channels
        .iter()
        .map(|c| json!({"camera": {"device": key, "channel": c}, "tier": tier}))
        .collect()
}

/// The decodes of one generation: (pts, when).
fn decodes_of(rec: &Recorder, tile: u64, generation: u32) -> Vec<(u64, tokio::time::Instant)> {
    rec.decode_times(tile)
        .into_iter()
        .filter(|(g, ..)| *g == generation)
        .map(|(_, p, at)| (p, at))
        .collect()
}

/// A mock picture's timestamp: its device-local time in µs.
fn pts_ms(pts: u64) -> i64 {
    (pts / 1000) as i64
}

#[test]
fn four_cameras_start_as_a_group_play_paced_and_in_sync() {
    let rig = Rig::new();
    let nvr = archive_mock(&rig.sim, 4, |_| {});
    let (_dev, key) = rig.add(&nvr, &[0, 1, 2, 3]);
    let p0 = ms(8, 10, 0, 30) + 250;
    let (pb, tiles) = rig.open(json!({
        "start_ms": ms(8, 10, 0, 0), "end_ms": ms(8, 10, 10, 0), "position_ms": p0,
        "cameras": cams(&key, &[0, 1, 2, 3], "sub"),
    }));
    assert_eq!(tiles.len(), 4);
    rig.until("the opening seek", 20, || rig.seek_active(pb, 1));

    // One seek per play, all at the same time (an exact seek's 2 s lead
    // before the position), then the four plays.
    let log = nvr.play_log();
    assert_eq!(log.len(), 4, "{log:?}");
    assert_eq!(nvr.command_count(123), 4);
    for l in &log {
        assert_eq!(l.at, d(8, 10, 0, 28));
        assert!(l.served);
        assert_eq!((l.speed, l.support_sub), (Some(1), Some(1)));
        assert_eq!(l.stream, Some(StreamTier::Sub));
    }
    let mut chans: Vec<u8> = log.iter().map(|l| l.channel).collect();
    chans.sort_unstable();
    assert_eq!(chans, vec![0, 1, 2, 3]);

    // Generation 0 was an empty placeholder: no Decode, a final Reset.
    for &tile in &tiles {
        let g0 = rig.rec.seen_of(tile, 0);
        assert_eq!(g0, vec![Seen::Reset], "tile {tile}");
        let g1 = decodes_of(&rig.rec, tile, 1);
        assert!(!g1.is_empty());
        // Paused at the position: nothing after it was decoded, and the
        // picture at it was (the anchor puts a picture within its
        // keyframe's second).
        assert!(g1.iter().all(|(p, _)| pts_ms(*p) <= p0), "{g1:?}");
        assert!(g1.iter().any(|(p, _)| pts_ms(*p) >= p0 - 1000));
        check_protocol(&rig.rec.seen_of(tile, 1));
    }
    let (pos, state) = rig.position(pb);
    assert_eq!((pos, state), (p0, PlaybackState::Paused));

    // Play for ten seconds.
    rig.core.playback_play(pb).unwrap();
    let started = rig.sim.now();
    rig.sim.advance(Duration::from_secs(10));
    rig.core.flush_events();
    let (pos, state) = rig.position(pb);
    assert_eq!(state, PlaybackState::Playing);
    assert!((pos - (p0 + 10_000)).abs() <= 50, "{pos} {}", p0 + 10_000);

    // Paced: a picture is decoded when the clock reaches it (all cameras
    // got the same recordings, so pictures pair up by timestamp).
    let per_tile: Vec<Vec<(u64, tokio::time::Instant)>> = tiles
        .iter()
        .map(|t| {
            decodes_of(&rig.rec, *t, 1)
                .into_iter()
                .filter(|(_, at)| *at > started)
                .collect()
        })
        .collect();
    for v in &per_tile {
        assert!(v.len() >= 90, "{}", v.len());
        let (p1, a1) = v[5];
        let (p2, a2) = v[v.len() - 1];
        let wall = (a2 - a1).as_millis() as i64;
        let media = pts_ms(p2) - pts_ms(p1);
        assert!((wall - media).abs() <= 30, "wall {wall} media {media}");
    }
    // In sync across the four cameras: the same picture within a tick.
    let mut worst = Duration::ZERO;
    for (p, at) in &per_tile[0] {
        for other in &per_tile[1..] {
            if let Some((_, o)) = other.iter().find(|(q, _)| q == p) {
                worst = worst.max(if *o > *at { *o - *at } else { *at - *o });
            }
        }
    }
    assert!(worst <= Duration::from_millis(20), "{worst:?}");

    // Live view is unaffected... and every play stops on close.
    rig.core.playback_close(pb).unwrap();
    rig.sim.settle();
    rig.core.flush_events();
    assert_eq!(nvr.active_plays(), 0);
    assert!(nvr.command_count(7) >= 4);
    assert_eq!(rig.rec.check_closed_once(&tiles), vec!["closed"; 4]);
    assert_eq!(rig.events("playback_closed", pb).len(), 1);
}

#[test]
fn a_seek_restarts_every_play_together_as_a_new_generation() {
    let rig = Rig::new();
    let nvr = archive_mock(&rig.sim, 2, |_| {});
    let (_dev, key) = rig.add(&nvr, &[0, 1]);
    let (pb, tiles) = rig.open(json!({
        "start_ms": ms(8, 10, 0, 0), "end_ms": ms(8, 11, 0, 0), "position_ms": ms(8, 10, 0, 10),
        "cameras": cams(&key, &[0, 1], "sub"),
    }));
    rig.until("the opening seek", 20, || rig.seek_active(pb, 1));
    rig.core.playback_play(pb).unwrap();
    rig.sim.advance(Duration::from_secs(3));
    let target = ms(8, 10, 30, 0) + 500;
    rig.core.playback_seek(pb, target, SeekMode::Exact).unwrap();
    rig.until("the seek", 20, || rig.seek_active(pb, 2));
    // Stop all, seek all to one time (2 s before the target), play all.
    let log = nvr.play_log();
    assert_eq!(log.len(), 4);
    assert!(
        log[2..]
            .iter()
            .all(|l| l.at == d(8, 10, 29, 58) && l.served)
    );
    assert_eq!(nvr.command_count(123), 4);
    assert!(nvr.command_count(7) >= 2);
    assert_eq!(nvr.active_plays(), 2);
    for &tile in &tiles {
        // The old generation got its final Reset when the new one showed;
        // the new one decoded from the keyframe to the target.
        let g = rig.rec.seen_gen(tile);
        let reset_1 = g
            .iter()
            .position(|(g, s)| *g == 1 && *s == Seen::Reset)
            .expect("generation 1 reset");
        assert!(!g[reset_1 + 1..].iter().any(|(g, _)| *g == 1));
        let g2 = decodes_of(&rig.rec, tile, 2);
        assert!(
            g2.iter()
                .any(|(p, _)| pts_ms(*p) >= target - 1000 && pts_ms(*p) <= target)
        );
        check_protocol(&rig.rec.seen_of(tile, 2));
    }
    let st = rig.events("playback_state", pb);
    let last = st.last().unwrap();
    assert_eq!(last["position_ms"].as_i64().unwrap() / 1000, target / 1000);
    // A keyframe seek is shown at the keyframe at or before the position.
    rig.core.playback_pause(pb).unwrap();
    let target = ms(8, 10, 40, 0) + 900;
    rig.core
        .playback_seek(pb, target, SeekMode::Keyframe)
        .unwrap();
    rig.until("the keyframe seek", 20, || rig.seek_active(pb, 3));
    for &tile in &tiles {
        let g3 = decodes_of(&rig.rec, tile, 3);
        assert!(
            rig.rec
                .seen_gen(tile)
                .iter()
                .any(|(g, s)| *g == 2 && *s == Seen::Reset)
        );
        // The mock's GOP is 2 s: the keyframe at 10:40:00.
        assert_eq!(pts_ms(g3[0].0), ms(8, 10, 40, 0));
    }
    rig.core.playback_close(pb).unwrap();
    rig.sim.settle();
    assert_eq!(nvr.active_plays(), 0);
}

/// The generation a camera of a playback shows or starts.
fn generation(rig: &Rig, pb: u64, i: usize) -> u32 {
    let s: Value = serde_json::from_str(&rig.core.playback_status(pb).unwrap()).unwrap();
    s["cameras"][i]["generation"].as_u64().unwrap() as u32
}

fn decode_kinds(rig: &Rig, tile: u64, generation: u32) -> Vec<bool> {
    rig.rec
        .seen_of(tile, generation)
        .iter()
        .filter_map(|s| match s {
            Seen::Decode { keyframe, .. } => Some(*keyframe),
            _ => None,
        })
        .collect()
}

#[test]
fn rates_send_play_speed_and_pace_the_clock() {
    let rig = Rig::new();
    let nvr = archive_mock(&rig.sim, 2, |_| {});
    let (_dev, key) = rig.add(&nvr, &[0, 1]);
    let p0 = ms(8, 10, 0, 10);
    let (pb, tiles) = rig.open(json!({
        "start_ms": ms(8, 10, 0, 0), "end_ms": ms(8, 12, 0, 0), "position_ms": p0,
        "cameras": [
            {"camera": {"device": key, "channel": 0}, "tier": "sub"},
            {"camera": {"device": key, "channel": 1}, "tier": "main"},
        ],
    }));
    rig.until("the opening seek", 20, || rig.seek_active(pb, 1));
    assert_eq!(
        rig.core.playback_set_rate(pb, 3.0),
        Err(viewer_core::RecError::InvalidArgument)
    );
    rig.core.playback_set_rate(pb, 4.0).unwrap();
    rig.core.playback_play(pb).unwrap();
    rig.until("plays at 4x", 20, || {
        nvr.play_log().iter().filter(|l| l.speed == Some(4)).count() == 2
    });
    // Stop all, seek all to the position (less the lead), play all at
    // playSpeed 4.
    let log = nvr.play_log();
    assert!(log[2..].iter().all(|l| l.at == d(8, 10, 0, 8)), "{log:?}");
    rig.until("playing", 20, || {
        rig.position(pb).1 == PlaybackState::Playing
    });
    let (a, _) = rig.position(pb);
    rig.sim.advance(Duration::from_secs(5));
    let (b, state) = rig.position(pb);
    assert_eq!(state, PlaybackState::Playing);
    assert!((b - a - 20_000).abs() <= 200, "{}", b - a);
    // The sub-stream keeps every frame at 4x; the main stream comes as
    // keyframes only (decoded as such).
    let (sub, main) = (tiles[0], tiles[1]);
    let g = generation(&rig, pb, 0);
    let subs = decode_kinds(&rig, sub, g);
    assert!(subs.len() > 100, "{}", subs.len());
    assert!(subs.iter().filter(|k| !**k).count() > 90);
    let mains = decode_kinds(&rig, main, g);
    assert!(mains.len() >= 8, "{}", mains.len());
    assert!(mains.iter().all(|k| *k));
    // 16x: playSpeed 16; the sub-stream is decoded as keyframes only.
    rig.core.playback_set_rate(pb, 16.0).unwrap();
    rig.until("plays at 16x", 20, || {
        nvr.play_log()
            .iter()
            .filter(|l| l.speed == Some(16))
            .count()
            == 2
    });
    rig.until("playing", 20, || {
        rig.position(pb).1 == PlaybackState::Playing
    });
    let (a, _) = rig.position(pb);
    rig.sim.advance(Duration::from_secs(2));
    let (b, _) = rig.position(pb);
    assert!((b - a - 32_000).abs() <= 400, "{}", b - a);
    let g = generation(&rig, pb, 0);
    let fast = decode_kinds(&rig, sub, g);
    assert!(fast.len() >= 8, "{}", fast.len());
    assert!(fast.iter().all(|k| *k));
    // Half speed: playSpeed 1, paced at half.
    rig.core.playback_set_rate(pb, 0.5).unwrap();
    rig.until("plays at 1x again", 20, || {
        let log = nvr.play_log();
        log.len() >= 8 && log.iter().rev().take(2).all(|l| l.speed == Some(1))
    });
    rig.until("playing", 20, || {
        rig.position(pb).1 == PlaybackState::Playing
    });
    let (a, _) = rig.position(pb);
    rig.sim.advance(Duration::from_secs(4));
    let (b, _) = rig.position(pb);
    assert!((b - a - 2_000).abs() <= 100, "{}", b - a);
    rig.core.playback_close(pb).unwrap();
    rig.sim.settle();
    assert_eq!(nvr.active_plays(), 0);
}

#[test]
fn a_camera_without_recording_is_not_started_and_resumes_with_its_recording() {
    let rig = Rig::new();
    // One-minute files; channel 1 has no file at 10:01 or 10:03.
    let nvr = archive_mock(&rig.sim, 2, |a| {
        a.file_minutes = 1;
        a.missing = vec![(1, d(8, 10, 1, 0)), (1, d(8, 10, 3, 0))];
    });
    let (_dev, key) = rig.add(&nvr, &[0, 1]);
    let (pb, tiles) = rig.open(json!({
        "start_ms": ms(8, 10, 0, 0), "end_ms": ms(8, 10, 10, 0), "position_ms": ms(8, 10, 1, 30),
        "cameras": cams(&key, &[0, 1], "sub"),
    }));
    rig.until("the opening seek", 20, || rig.seek_active(pb, 1));
    // Only the camera with a recording at the time was started.
    let log = nvr.play_log();
    assert_eq!(log.len(), 1, "{log:?}");
    assert_eq!((log[0].channel, log[0].at), (0, d(8, 10, 1, 28)));
    assert_eq!(
        rig.camera_state(pb, tiles[1]).unwrap()["state"],
        "no_recording"
    );
    // Its tile shows generation 1, cleared.
    assert_eq!(rig.rec.seen_of(tiles[1], 0), vec![Seen::Reset]);
    assert_eq!(rig.rec.seen_of(tiles[1], 1), vec![Seen::Clear]);
    // Where its recording resumes (10:02), it starts by itself: a seek and
    // play of that one camera while the other goes on.
    rig.core.playback_play(pb).unwrap();
    rig.until("channel 1 resumes", 60, || {
        rig.camera_state(pb, tiles[1]).unwrap()["state"] == "playing"
    });
    let log = nvr.play_log();
    assert_eq!(log.len(), 2, "{log:?}");
    assert_eq!((log[1].channel, log[1].at), (1, d(8, 10, 2, 0)));
    assert!(log[1].served);
    let (pos, _) = rig.position(pb);
    assert!(pos >= ms(8, 10, 1, 58) && pos <= ms(8, 10, 2, 2), "{pos}");
    let g1 = decodes_of(&rig.rec, tiles[1], 1);
    assert!(pts_ms(g1[0].0) >= ms(8, 10, 2, 0));
    check_protocol(&rig.rec.seen_of(tiles[1], 1));
    // At 10:03 it has nothing again: cleared, then back at 10:04, in the
    // same play (the device's play goes on across the gap).
    rig.until("the second gap", 120, || {
        rig.camera_state(pb, tiles[1]).unwrap()["state"] == "no_recording"
    });
    let (pos, _) = rig.position(pb);
    assert!(pos >= ms(8, 10, 3, 0) && pos <= ms(8, 10, 3, 2), "{pos}");
    assert_eq!(rig.rec.seen_of(tiles[1], 1).last(), Some(&Seen::Clear));
    rig.until("back after the gap", 120, || {
        rig.camera_state(pb, tiles[1]).unwrap()["state"] == "playing"
    });
    let (pos, _) = rig.position(pb);
    assert!(pos >= ms(8, 10, 4, 0) && pos <= ms(8, 10, 4, 2), "{pos}");
    // Channel 0 played on throughout, in its first generation.
    let g0 = decodes_of(&rig.rec, tiles[0], 1);
    let gaps = g0
        .windows(2)
        .filter(|w| pts_ms(w[1].0) - pts_ms(w[0].0) > 200)
        .count();
    assert_eq!(gaps, 0);
    rig.core.playback_close(pb).unwrap();
    rig.sim.settle();
    assert_eq!(nvr.active_plays(), 0);
}

#[test]
fn a_play_crosses_file_boundaries_and_the_playback_ends_with_the_recordings() {
    let rig = Rig::new();
    let nvr = archive_mock(&rig.sim, 2, |a| a.file_minutes = 1);
    let (_dev, key) = rig.add(&nvr, &[0, 1]);
    // The range runs past the last recording (the end of the 8th).
    let (pb, tiles) = rig.open(json!({
        "start_ms": ms(8, 23, 58, 0), "end_ms": ms(9, 0, 5, 0), "position_ms": ms(8, 23, 58, 50),
        "cameras": cams(&key, &[0, 1], "sub"),
    }));
    rig.until("the opening seek", 20, || rig.seek_active(pb, 1));
    rig.core.playback_play(pb).unwrap();
    rig.until("the end", 120, || {
        rig.position(pb).1 == PlaybackState::Ended
    });
    let (pos, _) = rig.position(pb);
    assert_eq!(pos, ms(9, 0, 0, 0));
    // One play per camera ran across the boundaries at 23:59 and midnight
    // came with no message: the engine ended at the end of the index.
    assert_eq!(nvr.play_log().len(), 2);
    for &tile in &tiles {
        let g = decodes_of(&rig.rec, tile, 1);
        let across = g
            .windows(2)
            .any(|w| pts_ms(w[0].0) < ms(8, 23, 59, 0) && pts_ms(w[1].0) >= ms(8, 23, 59, 0));
        assert!(across);
        assert!(g.windows(2).all(|w| pts_ms(w[1].0) - pts_ms(w[0].0) <= 200));
        assert!(pts_ms(g.last().unwrap().0) >= ms(8, 23, 59, 58));
        assert_eq!(rig.camera_state(pb, tile).unwrap()["state"], "ended");
    }
    rig.sim.advance(Duration::from_millis(500));
    assert_eq!(nvr.active_plays(), 0);
    let states: Vec<String> = rig
        .events("playback_state", pb)
        .iter()
        .map(|e| e["state"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(states.last().map(String::as_str), Some("ended"));
    // Play after the end starts over from the start.
    rig.core.playback_play(pb).unwrap();
    rig.until("the restart", 20, || {
        rig.seek_active(pb, 2) && rig.position(pb).1 == PlaybackState::Playing
    });
    assert!(rig.position(pb).0 < ms(8, 23, 58, 5));
    rig.core.playback_close(pb).unwrap();
    rig.sim.settle();
    assert_eq!(nvr.active_plays(), 0);
}

/// Live decodes of a tile within a window of the runtime's clock.
fn live_in(
    rec: &Recorder,
    tile: u64,
    from: tokio::time::Instant,
    to: tokio::time::Instant,
) -> usize {
    rec.decode_times(tile)
        .iter()
        .filter(|(_, _, at)| *at >= from && *at < to)
        .count()
}

#[test]
fn live_view_keeps_running_and_playback_tiles_are_tiles() {
    let rig = Rig::new();
    let nvr = archive_mock(&rig.sim, 3, |_| {});
    let (dev, key) = rig.add(&nvr, &[0, 1]);
    let live = rig.core.open_tile(dev, 2, StreamTier::Sub).unwrap();
    rig.sim.advance(Duration::from_secs(3));
    let a = rig.sim.now();
    rig.sim.advance(Duration::from_secs(5));
    let b = rig.sim.now();
    let before = live_in(&rig.rec, live, a, b);
    assert!(before >= 40, "{before}");
    let (pb, tiles) = rig.open(json!({
        "start_ms": ms(8, 10, 0, 0), "end_ms": ms(8, 11, 0, 0), "position_ms": ms(8, 10, 0, 10),
        "cameras": cams(&key, &[0, 1], "sub"),
    }));
    rig.until("the opening seek", 20, || rig.seek_active(pb, 1));
    rig.core.playback_play(pb).unwrap();
    let a = rig.sim.now();
    rig.sim.advance(Duration::from_secs(5));
    let b = rig.sim.now();
    let during = live_in(&rig.rec, live, a, b);
    // Live view kept its rate (and its one generation) beside two plays.
    assert!(
        (during as i64 - before as i64).abs() <= 3,
        "{before} {during}"
    );
    assert!(rig.rec.seen_gen(live).iter().all(|(g, _)| *g == 0));
    assert_eq!(nvr.active_plays(), 2);
    // A playback tile is not closed like a live one...
    assert_eq!(
        rig.core.close_tile(tiles[0]),
        Err(viewer_core::CoreError::InvalidArgument)
    );
    // ...and a tier switch restarts its device's plays in the new tier.
    rig.core.set_tile_tier(tiles[0], StreamTier::Main).unwrap();
    rig.until("the main-stream play", 20, || {
        nvr.play_log()
            .iter()
            .any(|l| l.channel == 0 && l.stream == Some(StreamTier::Main))
    });
    let log = nvr.play_log();
    let last_two = &log[log.len() - 2..];
    assert_eq!(last_two[0].at, last_two[1].at, "{log:?}");
    assert!(last_two.iter().all(|l| l.served));
    assert!(
        last_two
            .iter()
            .any(|l| l.channel == 0 && l.support_sub == Some(0))
    );
    rig.until("both playing", 20, || {
        tiles.iter().all(|t| {
            rig.camera_state(pb, *t)
                .is_some_and(|e| e["state"] == "playing")
        }) && rig.position(pb).1 == PlaybackState::Playing
    });
    // Removing a camera closes its tile and restarts the other's play.
    rig.core.playback_remove_camera(pb, tiles[1]).unwrap();
    rig.until("one play", 20, || nvr.active_plays() == 1);
    assert_eq!(rig.rec.check_closed_once(&tiles[1..]), vec!["closed"]);
    // Adding one joins at the position.
    let added = rig
        .core
        .playback_add_camera(
            pb,
            &json!({"camera": {"device": key, "channel": 1}, "tier": "sub"}).to_string(),
        )
        .unwrap();
    rig.until("two plays again", 20, || {
        nvr.active_plays() == 2
            && rig
                .camera_state(pb, added)
                .is_some_and(|e| e["state"] == "playing")
    });
    let log = nvr.play_log();
    let last_two = &log[log.len() - 2..];
    assert_eq!(last_two[0].at, last_two[1].at, "{log:?}");
    rig.core.close_tile(live).unwrap();
    rig.core.playback_close(pb).unwrap();
    rig.sim.settle();
    assert_eq!(nvr.active_plays(), 0);
}

#[test]
fn removing_a_device_or_shutting_down_stops_every_play() {
    let rig = Rig::new();
    let a = archive_mock(&rig.sim, 2, |_| {});
    let b = archive_mock(&rig.sim, 2, |_| {});
    // A device slow to answer file searches: a start of plays there is
    // still searching when the core shuts down.
    let slow = archive_mock(&rig.sim, 1, |a| {
        a.search_delay = Some(Duration::from_secs(8));
    });
    let (dev_a, key_a) = rig.add(&a, &[0, 1]);
    let (_dev_b, key_b) = rig.add(&b, &[0, 1]);
    let (_dev_slow, key_slow) = rig.add(&slow, &[0]);
    let mut cameras = cams(&key_a, &[0, 1], "sub");
    cameras.extend(cams(&key_b, &[0], "sub"));
    let (pb, tiles) = rig.open(json!({
        "start_ms": ms(8, 10, 0, 0), "end_ms": ms(8, 11, 0, 0), "position_ms": ms(8, 10, 0, 10),
        "cameras": cameras,
    }));
    rig.until("the opening seek", 20, || rig.seek_active(pb, 1));
    rig.core.playback_play(pb).unwrap();
    rig.sim.advance(Duration::from_secs(2));
    assert_eq!((a.active_plays(), b.active_plays()), (2, 1));
    // Device A goes: its two tiles close; the playback goes on with B.
    rig.bounded("the device removal", 5, move |core| {
        core.remove_device(dev_a).unwrap();
    });
    rig.sim.settle();
    rig.core.flush_events();
    assert_eq!(a.active_plays(), 0);
    assert!(a.command_count(7) >= 2);
    assert_eq!(
        rig.rec.check_closed_once(&tiles[..2]),
        vec!["device_removed"; 2]
    );
    assert!(rig.events("playback_closed", pb).is_empty());
    rig.sim.advance(Duration::from_secs(2));
    assert_eq!(b.active_plays(), 1);
    let (pos, state) = rig.position(pb);
    assert_eq!(state, PlaybackState::Playing);
    assert!(pos > ms(8, 10, 0, 12));
    // A second playback, of B and the slow device, starting.
    let mut cameras = cams(&key_b, &[1], "sub");
    cameras.extend(cams(&key_slow, &[0], "sub"));
    let searches = slow.command_count(14);
    let (pb2, _) = rig.open(json!({
        "start_ms": ms(8, 12, 0, 0), "end_ms": ms(8, 13, 0, 0),
        "cameras": cameras,
    }));
    rig.until("the slow device's file search", 5, || {
        slow.command_count(14) > searches
    });
    assert!(rig.events("playback_closed", pb2).is_empty());
    // The shutdown cancels the start in flight instead of waiting for it
    // to let go of its session (the search alone would take 8 s).
    rig.bounded("the shutdown", 5, Core::shutdown);
    rig.core.flush_events();
    assert_eq!(b.active_plays(), 0);
    assert_eq!(
        rig.rec.check_closed_once(&tiles[2..]),
        vec!["core_shutdown"]
    );
    let closed: Vec<Value> = rig.rec.events_of("playback_closed");
    assert_eq!(closed.len(), 2, "{closed:?}");
    assert!(closed.iter().all(|e| e["reason"] == "core_shutdown"));
}

#[test]
fn a_newer_playback_takes_the_device_and_the_older_shows_unavailable() {
    let rig = Rig::new();
    let nvr = archive_mock(&rig.sim, 4, |_| {});
    let (_dev, key) = rig.add(&nvr, &[0, 1, 2, 3]);
    let (old, old_tiles) = rig.open(json!({
        "start_ms": ms(8, 10, 0, 0), "end_ms": ms(8, 11, 0, 0), "position_ms": ms(8, 10, 0, 10),
        "cameras": cams(&key, &[0, 1], "sub"),
    }));
    rig.until("the first playback", 20, || rig.seek_active(old, 1));
    rig.core.playback_play(old).unwrap();
    rig.sim.advance(Duration::from_secs(2));
    // A second playback of the same device at another time.
    let (new, new_tiles) = rig.open(json!({
        "start_ms": ms(8, 15, 0, 0), "end_ms": ms(8, 16, 0, 0), "position_ms": ms(8, 15, 30, 0),
        "cameras": cams(&key, &[2, 3], "sub"),
    }));
    rig.until("the second playback", 20, || rig.seek_active(new, 1));
    // The older one's plays stopped first; its cameras are unavailable.
    for t in &old_tiles {
        let e = rig.camera_state(old, *t).unwrap();
        assert_eq!(
            (e["state"].as_str(), e["reason"].as_str()),
            (Some("failed"), Some("device_in_use"))
        );
        assert_eq!(rig.rec.seen_of(*t, 1).last(), Some(&Seen::Clear));
    }
    assert_eq!(nvr.active_plays(), 2);
    let log = nvr.play_log();
    assert!(
        log[2..]
            .iter()
            .all(|l| l.at == d(8, 15, 29, 58) && l.served),
        "{log:?}"
    );
    // The newer one plays; the older one's clock does not wait for its
    // unavailable cameras.
    rig.core.playback_play(new).unwrap();
    rig.sim.advance(Duration::from_secs(3));
    assert_eq!(rig.position(new).1, PlaybackState::Playing);
    assert_eq!(rig.position(old).1, PlaybackState::Failed);
    // Seeking the older one takes the device back.
    rig.core
        .playback_seek(old, ms(8, 10, 20, 0), SeekMode::Exact)
        .unwrap();
    rig.until("the older one again", 20, || rig.seek_active(old, 2));
    for t in &new_tiles {
        let e = rig.camera_state(new, *t).unwrap();
        assert_eq!(e["reason"], "device_in_use");
    }
    assert_eq!(nvr.active_plays(), 2);
    rig.core.playback_close(old).unwrap();
    rig.core.playback_close(new).unwrap();
    rig.sim.settle();
    assert_eq!(nvr.active_plays(), 0);
}

#[test]
fn paused_plays_stop_when_their_buffer_is_full_and_continue_from_it() {
    let rig = Rig::new();
    let nvr = archive_mock(&rig.sim, 1, |_| {});
    let (_dev, key) = rig.add(&nvr, &[0]);
    let p0 = ms(8, 10, 0, 10);
    let (pb, tiles) = rig.open(json!({
        "start_ms": ms(8, 10, 0, 0), "end_ms": ms(8, 11, 0, 0), "position_ms": p0,
        "cameras": cams(&key, &[0], "sub"),
    }));
    rig.until("the opening seek", 20, || rig.seek_active(pb, 1));
    // Paused, the device keeps sending at real time: once 20 s are held
    // the play is stopped.
    rig.until("the play stopped", 60, || nvr.active_plays() == 0);
    assert_eq!(nvr.command_count(7), 1);
    assert_eq!(rig.position(pb), (p0, PlaybackState::Paused));
    // Playing drains it; at half the play starts again from the newest
    // second held, in the same generation, with no gap.
    rig.core.playback_play(pb).unwrap();
    rig.until("the play again", 60, || nvr.play_log().len() == 2);
    let log = nvr.play_log();
    let resumed = log[1].at.to_naive_secs() * 1000;
    assert!(resumed >= p0 + 15_000 && resumed <= p0 + 25_000, "{log:?}");
    rig.sim.advance(Duration::from_secs(20));
    let g = decodes_of(&rig.rec, tiles[0], 1);
    assert!(pts_ms(g.last().unwrap().0) > resumed + 5_000);
    let late = g.iter().filter(|(p, _)| pts_ms(*p) > p0);
    let pts: Vec<i64> = late.map(|(p, _)| pts_ms(*p)).collect();
    assert!(
        pts.windows(2).all(|w| w[1] > w[0] && w[1] - w[0] <= 200),
        "a gap or a repeat"
    );
    assert_eq!(rig.position(pb).1, PlaybackState::Playing);
    rig.core.playback_close(pb).unwrap();
    rig.sim.settle();
    assert_eq!(nvr.active_plays(), 0);
}

#[test]
fn the_clock_waits_for_a_camera_up_to_its_limit_unless_sync_is_free() {
    for sync in ["wait", "free"] {
        let rig = Rig::new();
        // Channel 1's plays are accepted but never deliver.
        let nvr = archive_mock(&rig.sim, 2, |a| a.silent_plays = vec![1]);
        let (_dev, key) = rig.add(&nvr, &[0, 1]);
        let p0 = ms(8, 10, 0, 10);
        let (pb, tiles) = rig.open(json!({
            "start_ms": ms(8, 10, 0, 0), "end_ms": ms(8, 11, 0, 0), "position_ms": p0,
            "sync": sync, "cameras": cams(&key, &[0, 1], "sub"),
        }));
        rig.core.playback_play(pb).unwrap();
        rig.sim.advance(Duration::from_secs(3));
        let (pos, state) = rig.position(pb);
        if sync == "wait" {
            // Held for the silent camera...
            assert_eq!((pos, state), (p0, PlaybackState::Buffering));
            // ...until the first-media limit makes it unavailable.
            rig.until("channel 1 unavailable", 20, || {
                rig.camera_state(pb, tiles[1])
                    .is_some_and(|e| e["state"] == "no_recording")
            });
            rig.sim.advance(Duration::from_secs(2));
            let (pos, state) = rig.position(pb);
            assert_eq!(state, PlaybackState::Playing);
            assert!(pos > p0 + 1500, "{pos}");
        } else {
            assert_eq!(state, PlaybackState::Playing);
            assert!(pos > p0 + 1500, "{pos}");
        }
        // The seek completes with the silent camera cleared.
        rig.until("the seek", 20, || rig.seek_active(pb, 1));
        assert_eq!(rig.rec.seen_of(tiles[1], 1), vec![Seen::Clear]);
        // Its play was stopped; no other was started for it.
        rig.sim.advance(Duration::from_secs(5));
        assert_eq!(nvr.active_plays(), 1);
        assert_eq!(nvr.play_log().len(), 2);
        rig.core.playback_close(pb).unwrap();
        rig.sim.settle();
        assert_eq!(nvr.active_plays(), 0);
    }
}

/// The jitter of each released picture against its own timestamp spacing
/// (ms), for the decodes released after `from`.
fn release_jitter(decodes: &[(u64, tokio::time::Instant)], from: tokio::time::Instant) -> Vec<i64> {
    let v: Vec<_> = decodes.iter().filter(|(_, at)| *at >= from).collect();
    v.windows(2)
        .map(|w| {
            let wall = (w[1].1 - w[0].1).as_millis() as i64;
            let media = pts_ms(w[1].0) - pts_ms(w[0].0);
            (wall - media).abs()
        })
        .collect()
}

#[test]
fn pictures_are_released_evenly_when_the_given_utc_offset_is_not_the_devices() {
    // The app passes the computer's UTC offset; this device's clock runs
    // three hours east of that. Its keyframes' UTC seconds would put every
    // picture hours before the seek's target, and the engine would release
    // each group of pictures as fast as the decoder answers as it arrives
    // (bunched: a renderer showing the newest picture per refresh skips most
    // of them). The first keyframe shows the device's own offset; with it,
    // the target is reached and every picture is released at its own time,
    // also after the incident is selected again (an exact seek while
    // playing), and the device's opening burst is short.
    let rig = Rig::new();
    let nvr = archive_mock(&rig.sim, 1, |a| {
        a.utc_offset_s = OFFSET_S + 3 * 3600;
        a.play_burst_secs = 1;
    });
    let (_dev, key) = rig.add(&nvr, &[0]);
    let p0 = ms(8, 10, 0, 31);
    let (pb, tiles) = rig.open(json!({
        "start_ms": ms(8, 10, 0, 0), "end_ms": ms(8, 11, 0, 0), "position_ms": p0,
        "cameras": cams(&key, &[0], "sub"),
    }));
    // Played at once, as the player does when an incident is selected.
    rig.core.playback_play(pb).unwrap();
    rig.sim.advance(Duration::from_secs(1));
    rig.core
        .playback_seek(pb, p0 + 7_300, SeekMode::Exact)
        .unwrap();
    rig.core.playback_play(pb).unwrap();
    rig.until("the seek", 20, || rig.seek_active(pb, 2));
    rig.sim.advance(Duration::from_secs(2));
    let from = rig.sim.now();
    rig.sim.advance(Duration::from_secs(10));
    rig.core.flush_events();
    let (pos, state) = rig.position(pb);
    assert_eq!(state, PlaybackState::Playing);
    let released = decodes_of(&rig.rec, tiles[0], 2);
    // Paced against the clock: nothing released is far ahead of it.
    let newest = released.iter().map(|(p, _)| pts_ms(*p)).max().unwrap();
    assert!(
        newest <= pos + 100,
        "released up to {newest}, position {pos}"
    );
    let jitter = release_jitter(&released, from);
    assert!(jitter.len() >= 90, "{}", jitter.len());
    let worst = jitter.iter().copied().max().unwrap_or(0);
    assert!(
        worst <= 20,
        "a picture released {worst} ms off its spacing: {jitter:?}"
    );
    assert_eq!(rig.camera_state(pb, tiles[0]).unwrap()["state"], "playing");
    rig.core.playback_close(pb).unwrap();
    rig.sim.settle();
}

#[test]
fn audio_plays_at_normal_speed_only() {
    let rig = Rig::new();
    let aac =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/aac-tone.aac");
    let aac = Arc::new(mock_nvr::audio::AudioFixture::load(&aac).unwrap());
    let nvr = archive_mock(&rig.sim, 1, |a| a.audio = Some(aac));
    let (_dev, key) = rig.add(&nvr, &[0]);
    let (pb, tiles) = rig.open(json!({
        "start_ms": ms(8, 10, 0, 0), "end_ms": ms(8, 11, 0, 0), "position_ms": ms(8, 10, 0, 10),
        "cameras": cams(&key, &[0], "sub"),
    }));
    rig.until("the opening seek", 20, || rig.seek_active(pb, 1));
    let audio = |rig: &Rig| {
        rig.rec
            .events_of("tile_stats")
            .iter()
            .filter(|e| e["tile"] == tiles[0])
            .filter_map(|e| e["audio_frames"].as_u64())
            .max()
            .unwrap_or(0)
    };
    // Paused: no audio.
    rig.sim.advance(Duration::from_secs(2));
    rig.core.flush_events();
    assert_eq!(audio(&rig), 0);
    rig.core.playback_play(pb).unwrap();
    rig.sim.advance(Duration::from_secs(4));
    rig.core.flush_events();
    let at_1x = audio(&rig);
    // About 15 frames a second (64 ms each).
    assert!(at_1x >= 40, "{at_1x}");
    // At 4x the sub-stream still carries audio, but it is not played.
    rig.core.playback_set_rate(pb, 4.0).unwrap();
    rig.until("4x", 20, || {
        nvr.play_log().iter().any(|l| l.speed == Some(4))
    });
    rig.sim.advance(Duration::from_secs(1));
    rig.core.flush_events();
    let settled = audio(&rig);
    rig.sim.advance(Duration::from_secs(4));
    rig.core.flush_events();
    assert_eq!(audio(&rig), settled);
    rig.core.playback_close(pb).unwrap();
    rig.sim.settle();
    assert_eq!(nvr.active_plays(), 0);
}

#[test]
fn an_incident_opens_with_its_cameras_and_range_and_bad_requests_are_refused() {
    let rig = Rig::new();
    let nvr = archive_mock(&rig.sim, 3, |a| {
        let ev = |channel: u8, kind: &str, start: DateTime, end: DateTime| {
            mock_nvr::archive::MockEvent {
                channel,
                kind: kind.into(),
                start,
                end,
            }
        };
        a.events = vec![
            ev(0, "people", d(8, 10, 0, 5), d(8, 10, 0, 20)),
            ev(1, "vehicle", d(8, 10, 0, 8), d(8, 10, 0, 25)),
        ];
    });
    let (_dev, key) = rig.add(&nvr, &[0, 1, 2]);
    let inc: Value = serde_json::from_str(
        &rig.core
            .incidents_get(&json!({"start": t(8, 0, 0, 0), "end": t(8, 23, 59, 59)}).to_string())
            .unwrap(),
    )
    .unwrap();
    let k = inc["incidents"][0]["key"].as_str().unwrap().to_owned();
    let (pb, tiles) = rig.open(json!({"incident": k, "tier": "sub"}));
    assert_eq!(tiles.len(), 2);
    let s: Value = serde_json::from_str(&rig.core.playback_status(pb).unwrap()).unwrap();
    // The incident's range with its 5 s margins.
    assert_eq!(s["start_ms"], ms(8, 10, 0, 0));
    assert_eq!(s["end_ms"], ms(8, 10, 0, 31));
    assert_eq!(s["position_ms"], ms(8, 10, 0, 0));
    rig.until("the opening seek", 20, || rig.seek_active(pb, 1));
    // A seek beyond the range is clamped to it.
    rig.core
        .playback_seek(pb, ms(8, 12, 0, 0), SeekMode::Exact)
        .unwrap();
    rig.until("the clamped seek", 20, || rig.seek_active(pb, 2));
    assert_eq!(rig.position(pb).0, ms(8, 10, 0, 31));
    rig.core.playback_close(pb).unwrap();

    // Refusals.
    use viewer_core::RecError;
    let open = |v: Value| rig.core.playback_open(&v.to_string());
    let one = cams(&key, &[0], "sub");
    assert_eq!(
        open(json!({"start_ms": 10, "end_ms": 5, "cameras": one})),
        Err(RecError::InvalidArgument)
    );
    assert_eq!(
        open(json!({"start_ms": 0, "end_ms": 90_000_000, "cameras": one})),
        Err(RecError::Limit)
    );
    let mut five = cams(&key, &[0, 1, 2], "sub");
    five.extend(cams("d-other", &[0, 1], "sub"));
    assert_eq!(
        open(json!({"start_ms": 0, "end_ms": 5, "cameras": five})),
        Err(RecError::Limit)
    );
    let twice = cams(&key, &[0, 0], "sub");
    assert_eq!(
        open(json!({"start_ms": 0, "end_ms": 5, "cameras": twice})),
        Err(RecError::InvalidArgument)
    );
    assert_eq!(
        open(json!({"start_ms": 0, "end_ms": 5, "cameras": cams("d-other", &[0], "sub")})),
        Err(RecError::InvalidArgument)
    );
    assert_eq!(
        open(json!({"start_ms": 0, "end_ms": 5, "rate": 3, "cameras": one})),
        Err(RecError::InvalidArgument)
    );
    assert_eq!(rig.core.playback_play(999), Err(RecError::UnknownId));
    assert_eq!(rig.core.playback_close(pb), Err(RecError::UnknownId));
    rig.sim.settle();
    assert_eq!(nvr.active_plays(), 0);
}

/// An incident groups every camera its devices' indexes hold events of,
/// across devices, also one searched before and no longer in the enabled
/// list: it still plays (only its device must be enabled), as do range
/// requests of it.
#[test]
fn an_incident_of_two_devices_and_a_camera_no_longer_searched_still_opens() {
    let rig = Rig::new();
    let ev = |channel: u8, start: DateTime, end: DateTime| mock_nvr::archive::MockEvent {
        channel,
        kind: "people".into(),
        start,
        end,
    };
    let nvr = archive_mock(&rig.sim, 2, |a| {
        a.events = vec![
            ev(0, d(8, 10, 0, 5), d(8, 10, 0, 20)),
            ev(1, d(8, 10, 0, 8), d(8, 10, 0, 25)),
        ];
    });
    let other = archive_mock(&rig.sim, 1, |a| {
        a.events = vec![ev(0, d(8, 10, 0, 10), d(8, 10, 0, 15))];
    });
    let (_dev, key) = rig.add(&nvr, &[0, 1]);
    rig.add(&other, &[0]);
    // The user stops searching channel 1; its indexed events stay.
    rig.core
        .recordings_enable(
            &key,
            Some(&json!({"channels": [{"channel": 0}], "utc_offset_s": OFFSET_S}).to_string()),
        )
        .unwrap();
    let inc: Value = serde_json::from_str(
        &rig.core
            .incidents_get(&json!({"start": t(8, 0, 0, 0), "end": t(8, 23, 59, 59)}).to_string())
            .unwrap(),
    )
    .unwrap();
    let inc = &inc["incidents"][0];
    assert_eq!(inc["cameras"].as_array().unwrap().len(), 3);
    let k = inc["key"].as_str().unwrap().to_owned();
    let (pb, tiles) = rig.open(json!({"incident": k, "tier": "sub"}));
    assert_eq!(tiles.len(), 3);
    rig.until("the opening seek", 20, || rig.seek_active(pb, 1));
    rig.core.playback_play(pb).unwrap();
    rig.until("both cameras play", 20, || {
        tiles.iter().all(|t| {
            rig.camera_state(pb, *t)
                .is_some_and(|e| e["state"] == "playing")
        })
    });
    rig.core.playback_close(pb).unwrap();
    let (pb, tiles) = rig.open(json!({
        "start_ms": ms(8, 0, 0, 0), "end_ms": ms(9, 0, 0, 0), "position_ms": ms(8, 10, 0, 0),
        "cameras": cams(&key, &[0, 1], "sub"),
    }));
    assert_eq!(tiles.len(), 2);
    rig.core.playback_close(pb).unwrap();
    rig.sim.settle();
    assert_eq!(nvr.active_plays() + other.active_plays(), 0);
}

/// What [`watch_sync`] saw over one stage.
#[derive(Debug, Default)]
struct SyncWatch {
    /// Largest spread of the cameras' newest shown pictures (media ms),
    /// over samples where every camera had shown one and none was late.
    spread_ms: i64,
    /// The same over samples while a camera was late.
    spread_late_ms: i64,
    /// Samples, and those taken while a camera was late.
    samples: usize,
    late_samples: usize,
    /// Each camera's first picture of the stage: the position less its
    /// media time when it was seen (negative: shown ahead of the clock).
    first_lag_ms: Vec<i64>,
}

/// Samples, every 20 ms for `secs` of simulated time, each camera's newest
/// decoded picture of generation `generation` or later at or after
/// `from_ms` (pictures decoded up to a seek's target are never shown), and
/// how far apart the cameras' pictures are.
fn watch_sync(
    rig: &Rig,
    pb: u64,
    tiles: &[u64],
    generation: u32,
    from_ms: i64,
    secs: u64,
) -> SyncWatch {
    let mut w = SyncWatch {
        first_lag_ms: vec![i64::MIN; tiles.len()],
        ..SyncWatch::default()
    };
    for _ in 0..secs * 50 {
        rig.sim.advance(Duration::from_millis(20));
        rig.core.flush_events();
        let pos = rig.position(pb).0;
        let newest: Vec<Option<i64>> = tiles
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let n = rig
                    .rec
                    .decode_times(*t)
                    .into_iter()
                    .filter(|(g, p, _)| *g >= generation && pts_ms(*p) >= from_ms)
                    .map(|(_, p, _)| pts_ms(p))
                    .next_back();
                if let Some(p) = n
                    && w.first_lag_ms[i] == i64::MIN
                {
                    w.first_lag_ms[i] = pos - p;
                }
                n
            })
            .collect();
        if newest.iter().any(Option::is_none) {
            continue;
        }
        let v: Vec<i64> = newest.into_iter().flatten().collect();
        let spread = v.iter().max().unwrap() - v.iter().min().unwrap();
        let late = tiles.iter().any(|t| {
            rig.camera_state(pb, *t)
                .is_some_and(|e| e["state"] == "late")
        });
        w.samples += 1;
        if late {
            w.late_samples += 1;
            w.spread_late_ms = w.spread_late_ms.max(spread);
        } else {
            w.spread_ms = w.spread_ms.max(spread);
        }
    }
    w
}

/// The bug a hardware run showed: after a seek or a rate change, a camera
/// whose play started late, began at a keyframe after the target, or
/// stalled mid-play was not held with the others, and the cameras' media
/// times drifted seconds apart. Here, at 1x (after a seek), 4x, and 16x
/// (after rate changes), with one camera whose first media comes late, one
/// whose play begins at a keyframe after the target, and one that stalls
/// mid-play, the cameras stay together: within 250 ms at 1x, 500 ms at 4x,
/// and one keyframe interval of the decoded stream (the mock's 2 s GOP)
/// plus a few ticks at 16x, where only keyframes are shown. The stalled
/// camera is shown `late` for a bounded time while the others go on, and
/// then rejoins them through a restart of its play alone.
#[test]
fn cameras_stay_together_after_seeks_and_rate_changes_despite_late_and_stalled_plays() {
    use mock_nvr::archive::PlayFaultKind::{FirstKeyframeAfter, FirstMediaAfter, Stall};
    let rig = Rig::new();
    let faults = mock_nvr::archive::PlayFaults::default();
    let f2 = faults.clone();
    let nvr = archive_mock(&rig.sim, 4, move |a| a.play_faults = f2);
    let (_dev, key) = rig.add(&nvr, &[0, 1, 2, 3]);
    let (pb, tiles) = rig.open(json!({
        "start_ms": ms(8, 9, 0, 0), "end_ms": ms(8, 13, 0, 0), "position_ms": ms(8, 10, 0, 10),
        "cameras": cams(&key, &[0, 1, 2, 3], "sub"),
    }));
    rig.until("the opening seek", 20, || rig.seek_active(pb, 1));
    rig.core.playback_play(pb).unwrap();
    rig.sim.advance(Duration::from_secs(3));

    for (rate, bound_ms) in [(1.0, 250), (4.0, 500), (16.0, 2_000 + 16 * 50)] {
        let plays = nvr.play_log().len();
        // Channel 1 starts 3 s late; channel 2 begins at the first keyframe
        // 4 s after the device's seek time (2 s after the target); channel 3
        // stalls for 10 s after 6 s worth (at the rate) of media.
        faults.add(1, FirstMediaAfter(Duration::from_secs(3)));
        faults.add(2, FirstKeyframeAfter(4));
        faults.add(
            3,
            Stall {
                after_secs: (6.0 * rate) as u32,
                pause: Duration::from_secs(10),
            },
        );
        let target = if rate == 1.0 {
            let target = rig.position(pb).0 + 60_000;
            rig.core.playback_seek(pb, target, SeekMode::Exact).unwrap();
            target
        } else {
            rig.core.playback_set_rate(pb, rate).unwrap();
            rig.position(pb).0
        };
        let generation = generation(&rig, pb, 0);
        let w = watch_sync(&rig, pb, &tiles, generation, target, 16);
        let log = nvr.play_log();
        // The four started as a group, then channel 2 (its play began after
        // the target) and channel 3 (stalled) once more each, alone.
        let mut again: Vec<u8> = log[plays + 4..].iter().map(|l| l.channel).collect();
        again.sort_unstable();
        assert_eq!(again, vec![2, 3], "{rate}x: {log:?}");
        assert!(w.samples > 250, "{rate}x: {w:?}");
        assert!(w.spread_ms <= bound_ms, "{rate}x: {w:?}");
        // After the clock waited 2 s for it, late for at most 0.8 s, and
        // never further behind than 0.75 s of wall time at the rate.
        assert!(w.late_samples <= 40, "{rate}x: {w:?}");
        assert!(
            w.spread_late_ms <= bound_ms.max((750.0 * rate) as i64),
            "{rate}x: {w:?}"
        );
        // No camera's first picture was shown ahead of the clock.
        for lag in &w.first_lag_ms {
            assert!(*lag >= -(bound_ms / 2), "{rate}x: {w:?}");
        }
        let states: Vec<String> = tiles
            .iter()
            .map(|t| rig.camera_state(pb, *t).unwrap()["state"].to_string())
            .collect();
        assert!(
            states.iter().all(|s| s == "\"playing\""),
            "{rate}x: {states:?}"
        );
    }
    rig.core.playback_close(pb).unwrap();
    rig.sim.settle();
    assert_eq!(nvr.active_plays(), 0);
}

/// Hardware showed two cases still drifting: the first 1x play after a
/// paused exact-seek open, and the return to 1x after 16x (where only
/// keyframes are shown). With cameras whose keyframes are spaced
/// differently, on the main stream (thinned by the device at 16x), both
/// stages stay within the bounds of a seek at 1x.
#[test]
fn cameras_stay_together_on_the_first_play_and_back_from_16x() {
    let rig = Rig::new();
    use mock_nvr::archive::PlayFaultKind::{FirstKeyframeAfter, FirstMediaAfter};
    let faults = mock_nvr::archive::PlayFaults::default();
    let f2 = faults.clone();
    let nvr = archive_mock(&rig.sim, 4, move |a| {
        a.gop_by_channel = vec![(0, 1), (1, 2), (2, 3), (3, 3)];
        a.play_faults = f2;
    });
    // One camera's play begins at a keyframe 4 s after the seek time, 2 s
    // after the target: later than the seek lead covers.
    faults.add(2, FirstKeyframeAfter(4));

    let (_dev, key) = rig.add(&nvr, &[0, 1, 2, 3]);
    let (pb, tiles) = rig.open(json!({
        "start_ms": ms(8, 9, 0, 0), "end_ms": ms(8, 13, 0, 0), "position_ms": ms(8, 10, 0, 10),
        "cameras": cams(&key, &[0, 1, 2, 3], "sub"),
    }));
    rig.until("the opening seek", 20, || rig.seek_active(pb, 1));
    // The late camera's play was asked again, earlier, once.
    let opened: Vec<u8> = nvr.play_log().iter().map(|l| l.channel).collect();
    assert_eq!(opened.iter().filter(|c| **c == 2).count(), 2, "{opened:?}");
    // Paused for a while, as when the user looks at the incident first.
    rig.sim.advance(Duration::from_secs(5));
    let target = rig.position(pb).0;
    rig.core.playback_play(pb).unwrap();
    let g = generation(&rig, pb, 0);
    let w = watch_sync(&rig, pb, &tiles, g, target, 10);
    assert!(w.spread_ms <= 250, "first play: {w:?}");
    assert!(w.samples > 150, "first play: {w:?}");
    assert!(w.spread_ms <= 250, "first play: {w:?}");

    rig.core.playback_set_rate(pb, 16.0).unwrap();
    let p = rig.position(pb).0;
    let g = generation(&rig, pb, 0);
    let w = watch_sync(&rig, pb, &tiles, g, p, 6);
    assert!(w.spread_ms <= 2_000 + 16 * 50, "16x: {w:?}");
    faults.add(2, FirstKeyframeAfter(4));
    faults.add(1, FirstMediaAfter(Duration::from_secs(1)));
    rig.core.playback_set_rate(pb, 1.0).unwrap();
    let p = rig.position(pb).0;
    let g = generation(&rig, pb, 0);
    let w = watch_sync(&rig, pb, &tiles, g, p, 10);
    assert!(w.samples > 150, "back to 1x: {w:?}");
    assert!(w.spread_ms <= 250, "back to 1x: {w:?}");
    assert!(w.late_samples <= 40, "back to 1x: {w:?}");
    assert!(w.spread_late_ms <= 750, "back to 1x: {w:?}");
    for lag in &w.first_lag_ms {
        assert!(*lag >= -125, "back to 1x: {w:?}");
    }
    rig.core.playback_close(pb).unwrap();
    rig.sim.settle();
    assert_eq!(nvr.active_plays(), 0);
}
