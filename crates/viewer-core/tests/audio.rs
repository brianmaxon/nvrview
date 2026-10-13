//! Listening end to end: the mock NVR interleaves real AAC records (the
//! clicks fixture) with video; the core decodes, timestamps, and plays them
//! into its renderer, which the test drives like an audio device on the
//! core's clock. Each click heard is compared with the moment its picture
//! was shown (pictures are shown on arrival: the time the core delivered the
//! `Decode` with the same camera time) plus the playout delay the core
//! reported. Also: taps, `audio_stats`, talk-back records, and the API's
//! argument checks.
//!
//! The core and the mock run on a simulated clock (`simtime`): the device
//! paces its records, the core stamps their arrival, and the test renders,
//! all in simulated time, so the timing checks are exact and do not depend
//! on how busy the machine is.

use crate::common::{PASSWORD, USER, on_sim, sim_spawn, sim_until, wait_until};
use audio_pipeline::adts::split_frames;
use audio_pipeline::decode::Decoder;
use simtime::SimRuntime;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;
use viewer_core::audio::TapBlock;
use viewer_core::{Action, Callbacks, Core, CoreConfig, CoreError, StreamTier};

const CLOCK_HZ: f64 = 16_000.0;
const JITTER_MS: u32 = 30;
const THRESHOLD: f32 = 0.4;

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/synthetic")
        .join(name)
}

fn mock(sim: &SimRuntime, stream_index: u8) -> mock_nvr::MockNvr {
    let video = mock_nvr::fixture::Fixture::load(&fixture_path("h264-basic.h264")).unwrap();
    let mut cfg = mock_nvr::Config::new(video);
    cfg.channels = 1;
    let mut a = mock_nvr::audio::AudioConfig::new(
        mock_nvr::audio::AudioFixture::load(&fixture_path("aac-clicks.aac")).unwrap(),
    );
    a.clock_hz = CLOCK_HZ;
    a.jitter_us = JITTER_MS * 1000;
    a.stream_index = stream_index;
    cfg.audio = Some(Arc::new(a));
    sim_spawn(sim, cfg)
}

/// Decoded-sample offsets of the clicks within one loop of the fixture.
fn click_onsets() -> (Vec<usize>, usize) {
    let bytes = std::fs::read(fixture_path("aac-clicks.aac")).unwrap();
    let frames = split_frames(&bytes);
    let mut d = Decoder::new();
    let mut pcm = Vec::new();
    for f in &frames {
        d.decode(5, f, &mut pcm).unwrap();
    }
    let mut onsets = Vec::new();
    let mut quiet = 0usize;
    for (i, s) in pcm.iter().enumerate() {
        if s.abs() > THRESHOLD && quiet > 1000 {
            onsets.push(i);
        }
        quiet = if s.abs() > THRESHOLD { 0 } else { quiet + 1 };
    }
    (onsets, frames.len() * 1024)
}

/// A tap block as seen: epoch, first sample, samples, camera µs, wall µs,
/// discontinuity.
type TapSeen = (u32, u64, usize, Option<f64>, Option<i64>, bool);

#[derive(Default)]
struct Rec {
    core: OnceLock<Weak<Core>>,
    /// (pts µs, host µs when its Decode was delivered)
    shown: Mutex<Vec<(u64, f64)>>,
    events: Mutex<Vec<serde_json::Value>>,
    taps: Mutex<Vec<TapSeen>>,
    tap_reentry: Mutex<Vec<Result<(), CoreError>>>,
    try_reentry: AtomicBool,
}

impl Callbacks for Rec {
    fn on_action(&self, tile: u64, generation: u32, action: &Action<'_>) {
        if let Action::Decode { pts, .. } = *action
            && let Some(c) = self.core.get().and_then(Weak::upgrade)
        {
            self.shown
                .lock()
                .unwrap()
                .push((pts, c.clock_now_ns() as f64 / 1e3));
            let _ = c.decoded(tile, generation, pts);
        }
    }

    fn on_event(&self, json: &str) {
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        self.events.lock().unwrap().push(v);
    }

    fn on_audio_tap(&self, tile: u64, b: &TapBlock) {
        self.taps.lock().unwrap().push((
            b.epoch,
            b.first_sample,
            b.samples.len(),
            b.camera_time_us,
            b.wall_clock_us,
            b.discontinuity,
        ));
        if self.try_reentry.swap(false, Ordering::SeqCst)
            && let Some(c) = self.core.get().and_then(Weak::upgrade)
        {
            self.tap_reentry
                .lock()
                .unwrap()
                .push(c.set_tile_audio_tap(tile, false));
        }
    }
}

fn start(sim: &SimRuntime) -> (Arc<Core>, Arc<Rec>) {
    let rec = Arc::new(Rec::default());
    let cfg = on_sim(
        sim,
        CoreConfig {
            stats_interval: Duration::from_millis(200),
            ..CoreConfig::default()
        },
    );
    let core = Arc::new(Core::new(cfg, rec.clone()).unwrap());
    rec.core.set(Arc::downgrade(&core)).unwrap();
    (core, rec)
}

/// Stops the core, letting simulated time run while it closes.
fn stop(sim: &SimRuntime, core: &Core) {
    sim.run_free(|| core.shutdown());
}

/// Lets simulated time pass until the device would ask for the callback
/// heard at `host` (each is asked for `lead_ns` before it is heard).
fn wait_for_callback(sim: &SimRuntime, core: &Core, host: u64, lead_ns: u64) {
    let now = core.clock_now_ns();
    if now + lead_ns < host {
        sim.advance(Duration::from_nanos(host - lead_ns - now));
    }
}

fn audio_stats(rec: &Rec) -> Vec<serde_json::Value> {
    rec.events
        .lock()
        .unwrap()
        .iter()
        .filter(|e| e["type"] == "audio_stats")
        .cloned()
        .collect()
}

/// Drives the renderer like a 48 kHz stereo device: 10 ms callbacks, each
/// called 10 ms before its first frame is heard. Returns the host times
/// (µs) at which clicks were heard.
fn play(sim: &SimRuntime, core: &Core, seconds: f64) -> Vec<f64> {
    const FRAMES: usize = 480;
    const LEAD_NS: u64 = 10_000_000;
    let mut buf = vec![0.0f32; FRAMES * 2];
    let start = core.clock_now_ns() + LEAD_NS;
    let mut heard = Vec::new();
    let mut quiet = 0usize;
    for k in 0..(seconds * 100.0).round() as u64 {
        let host = start + k * 10_000_000;
        wait_for_callback(sim, core, host, LEAD_NS);
        let _ = core.render_audio(&mut buf, 2, host);
        for (i, f) in buf.as_chunks::<2>().0.iter().enumerate() {
            if f[0].abs() > THRESHOLD && quiet > 3000 {
                heard.push(host as f64 / 1e3 + i as f64 * 1e6 / 48_000.0);
            }
            quiet = if f[0].abs() > THRESHOLD { 0 } else { quiet + 1 };
        }
    }
    heard
}

/// Renders like [`play`] and returns the highest peak level the core
/// reported for `tile` meanwhile.
fn play_level(sim: &SimRuntime, core: &Core, tile: u64, seconds: f64) -> f32 {
    let mut buf = vec![0.0f32; 480 * 2];
    let start = core.clock_now_ns() + 10_000_000;
    let mut max = 0.0f32;
    for k in 0..(seconds * 100.0).round() as u64 {
        let host = start + k * 10_000_000;
        wait_for_callback(sim, core, host, 10_000_000);
        let _ = core.render_audio(&mut buf, 2, host);
        let (peak, rms) = core.tile_audio_level(tile);
        assert!((0.0..=1.0).contains(&peak) && (0.0..=1.0).contains(&rms));
        assert!(rms <= peak + 1e-6);
        max = max.max(peak);
    }
    max
}

#[test]
fn clicks_are_heard_their_playout_delay_after_their_picture() {
    let sim = SimRuntime::new();
    let nvr = mock(&sim, 5);
    let (core, rec) = start(&sim);
    let dev = core
        .add_device("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
        .unwrap();
    let tile = core.open_tile(dev, 0, StreamTier::Main).unwrap();
    core.set_audio_output(48_000, 2).unwrap();
    core.set_tile_audio(tile, true, 1.0).unwrap();
    core.set_tile_audio_tap(tile, true).unwrap();
    let heard = play(&sim, &core, 25.0);
    core.flush_events();

    // Once the tap is attached, blocks arrive in order with camera times.
    // (The tap thread delivers on its own schedule: wait for it.)
    wait_until("tap blocks", 10, || rec.taps.lock().unwrap().len() > 200);
    let taps = rec.taps.lock().unwrap().clone();
    assert!(taps.len() > 200, "{} tap blocks", taps.len());
    assert!(taps[0].5, "the first block marks a discontinuity");
    for w in taps.windows(2) {
        assert_eq!(w[1].1, w[0].1 + w[0].2 as u64, "contiguous");
        assert!(!w[1].5);
    }
    assert!(taps.iter().all(|t| t.2 == 1024 && t.4.is_some()));
    assert!(taps.last().unwrap().3.is_some());

    // The reference: when pictures were shown, by camera time. Their
    // arrival jitters (the mock delays messages by up to 30 ms); like the
    // core, use the median offset from camera time.
    let shown = rec.shown.lock().unwrap().clone();
    let mut offsets: Vec<f64> = shown.iter().map(|&(p, h)| h - p as f64).collect();
    offsets.sort_by(f64::total_cmp);
    let shown_offset = offsets[offsets.len() / 2];

    // The delay the core reported over time.
    let stats = audio_stats(&rec);
    let delays: Vec<(f64, f64)> = stats
        .iter()
        .filter_map(|e| {
            Some((
                e["records"].as_f64()?,
                e["playout_delay_ms"].as_f64()? * 1e3,
            ))
        })
        .collect();
    let last = stats.last().unwrap().clone();
    println!("last audio_stats: {last}");
    assert_eq!(last["codec"], "aac");
    assert_eq!(last["audible"], true);
    assert_eq!(last["concealed"].as_u64(), Some(0));
    assert!(last["tap_blocks"].as_u64().unwrap() > 200);

    // Every click heard, against camera time (decoded sample / clock) on the
    // video's presentation line plus the delay in effect.
    let (onsets, loop_samples) = click_onsets();
    let delay_at = |sample: f64| {
        let rec_k = sample / 1024.0;
        let i = delays.partition_point(|&(r, _)| r < rec_k);
        delays.get(i).or(delays.last()).unwrap().1
    };
    let mut errors = Vec::new();
    for &h in &heard {
        // Nearest click by camera time.
        let cam_us = h - shown_offset - delays.last().unwrap().1;
        let n_rough = cam_us / 1e6 * CLOCK_HZ;
        let base = (n_rough / loop_samples as f64).floor() as i64;
        let mut best = None::<(f64, f64)>;
        for b in [base - 1, base, base + 1] {
            for &o in &onsets {
                let n = b as f64 * loop_samples as f64 + o as f64;
                if n < 0.0 {
                    continue;
                }
                let target = n / CLOCK_HZ * 1e6 + shown_offset + delay_at(n);
                let e = h - target;
                if best.is_none_or(|(be, _)| e.abs() < be.abs()) {
                    best = Some((e, n / CLOCK_HZ));
                }
            }
        }
        errors.push(best.unwrap());
    }
    // After the warm-up jump that places playout, clicks follow the target.
    let settled: Vec<f64> = errors
        .iter()
        .filter(|(_, t)| *t > 8.0)
        .map(|(e, _)| *e)
        .collect();
    let worst = settled.iter().fold(0.0f64, |m, e| m.max(e.abs()));
    let mean = settled.iter().sum::<f64>() / settled.len().max(1) as f64;
    let d_end = delays.last().unwrap().1;
    println!(
        "{} clicks heard, {} after 8 s: A/V offset (audio after picture) {:.1} ms = playout delay {:.1} ms {:+.1} ms mean error, worst {:.1} ms; delay started at {:.0} ms",
        heard.len(),
        settled.len(),
        (mean + d_end) / 1e3,
        d_end / 1e3,
        mean / 1e3,
        worst / 1e3,
        delays[0].1 / 1e3,
    );
    assert!(settled.len() >= 30, "{} clicks", settled.len());
    assert!(worst < 40_000.0, "worst {worst} µs");
    assert_eq!(core.audio_render_counters(), (0, 0));

    // Turning the tap off stops deliveries once the call returns.
    core.set_tile_audio_tap(tile, false).unwrap();
    let n = rec.taps.lock().unwrap().len();
    sim.advance(Duration::from_millis(300));
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(rec.taps.lock().unwrap().len(), n);

    // The meter sees the clicks, also while muted.
    assert!(play_level(&sim, &core, tile, 3.0) > THRESHOLD);
    core.set_tile_audio(tile, true, 0.0).unwrap();
    assert!(play_level(&sim, &core, tile, 3.0) > THRESHOLD);
    assert_eq!(core.tile_audio_level(tile + 100), (0.0, 0.0));

    // Silent: the slot is released, nothing more is heard; the meter still
    // shows the decoded audio.
    core.set_tile_audio(tile, false, 1.0).unwrap();
    sim.advance(Duration::from_millis(300));
    assert!(play(&sim, &core, 1.5).is_empty());
    assert!(max_level(&sim, &core, tile, 1.5) > THRESHOLD);
    stop(&sim, &core);
    nvr.shutdown();
}

/// The highest peak level read for `tile` over `seconds`, at display rate.
fn max_level(sim: &SimRuntime, core: &Core, tile: u64, seconds: f64) -> f32 {
    let mut max = 0.0f32;
    for _ in 0..(seconds * 1000.0 / 16.0).ceil() as u64 {
        let (peak, rms) = core.tile_audio_level(tile);
        assert!((0.0..=1.0).contains(&peak) && (0.0..=rms.max(peak)).contains(&rms));
        max = max.max(peak);
        sim.advance(Duration::from_millis(16));
    }
    max
}

#[test]
fn a_tile_never_made_audible_shows_its_level_until_its_audio_stops() {
    let sim = SimRuntime::new();
    let nvr = mock(&sim, 5);
    let (core, _rec) = start(&sim);
    let dev = core
        .add_device("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
        .unwrap();
    let tile = core.open_tile(dev, 0, StreamTier::Main).unwrap();
    // No output, never audible: the level comes from decoding (clicks
    // every half second, peak release 0.3 s).
    assert!(max_level(&sim, &core, tile, 3.0) > THRESHOLD);
    assert_eq!(core.tile_audio_level(tile + 100), (0.0, 0.0));
    // The stream stalls (the device goes away): the level falls to 0
    // instead of freezing.
    nvr.shutdown();
    sim.advance(Duration::from_millis(1_300));
    assert_eq!(core.tile_audio_level(tile), (0.0, 0.0));
    stop(&sim, &core);
}

#[test]
fn talkback_records_are_ignored() {
    let sim = SimRuntime::new();
    let nvr = mock(&sim, 1);
    let (core, rec) = start(&sim);
    let dev = core
        .add_device("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
        .unwrap();
    let tile = core.open_tile(dev, 0, StreamTier::Main).unwrap();
    core.set_audio_output(48_000, 2).unwrap();
    core.set_tile_audio(tile, true, 1.0).unwrap();
    sim_until(&sim, &core, "talk-back counted", 10, || {
        audio_stats(&rec)
            .last()
            .is_some_and(|e| e["talkback_ignored"].as_u64().unwrap_or(0) > 20)
    });
    let last = audio_stats(&rec).last().unwrap().clone();
    assert_eq!(last["records"].as_u64(), Some(0), "{last}");
    stop(&sim, &core);
    nvr.shutdown();
}

#[test]
fn audio_arguments_and_reentry() {
    let sim = SimRuntime::new();
    let nvr = mock(&sim, 5);
    let (core, rec) = start(&sim);
    assert_eq!(core.set_audio_output(0, 2), Err(CoreError::InvalidArgument));
    assert_eq!(
        core.set_audio_output(48_000, 3),
        Err(CoreError::InvalidArgument)
    );
    assert_eq!(
        core.set_tile_audio(99, true, 1.0),
        Err(CoreError::UnknownId)
    );
    assert_eq!(core.set_tile_audio_tap(99, true), Err(CoreError::UnknownId));
    // Rendering before any output is silence.
    let mut buf = vec![1.0f32; 64];
    assert_eq!(core.render_audio(&mut buf, 2, 0), Err(CoreError::Closed));
    assert!(buf.iter().all(|&s| s == 0.0));

    let dev = core
        .add_device("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
        .unwrap();
    let tile = core.open_tile(dev, 0, StreamTier::Main).unwrap();
    assert_eq!(
        core.set_tile_audio(tile, true, f32::NAN),
        Err(CoreError::InvalidArgument)
    );
    assert_eq!(
        core.set_tile_audio(tile, true, -1.0),
        Err(CoreError::InvalidArgument)
    );
    core.set_audio_output(44_100, 1).unwrap();
    assert_eq!(
        core.render_audio(&mut buf, 2, 0),
        Err(CoreError::InvalidArgument)
    );
    assert_eq!(core.render_audio(&mut buf, 1, 0), Ok(()));

    // Calls from inside on_audio_tap are refused, not deadlocked.
    rec.try_reentry.store(true, Ordering::SeqCst);
    core.set_tile_audio_tap(tile, true).unwrap();
    // Audio arrives in simulated time; the tap thread delivers it in real
    // time.
    sim.advance(Duration::from_secs(2));
    wait_until("a tap block", 10, || {
        !rec.tap_reentry.lock().unwrap().is_empty()
    });
    assert_eq!(
        rec.tap_reentry.lock().unwrap()[0],
        Err(CoreError::InvalidArgument)
    );
    // Closing the tile ends its tap; nothing for it arrives afterwards.
    core.close_tile(tile).unwrap();
    let n = rec.taps.lock().unwrap().len();
    sim.advance(Duration::from_millis(300));
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(rec.taps.lock().unwrap().len(), n);
    assert_eq!(
        core.set_tile_audio(tile, true, 1.0),
        Err(CoreError::UnknownId)
    );
    stop(&sim, &core);
    assert_eq!(core.render_audio(&mut buf, 1, 0), Err(CoreError::Closed));
    nvr.shutdown();
}
