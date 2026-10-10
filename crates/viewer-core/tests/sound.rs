//! Sound analysis end to end: the mock NVR interleaves a synthetic smoke
//! alarm (a 3.1 kHz tone in the T3 rhythm, fixtures/synthetic
//! aac-smoke-t3.aac) or a steady two-tone signal (aac-tone.aac) with its
//! video, on every channel; the core decodes it and reports alarms as
//! `detection` events with `source` `local_audio`, keeping per-second
//! loudness for each tile. On a simulated clock (`simtime`): the fixtures'
//! seconds of audio take simulated time, not real time.

mod common;

use common::{PASSWORD, Recorder, USER, on_sim, sim_spawn, sim_until, start};
use serde_json::Value;
use simtime::SimRuntime;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use viewer_core::sound::SoundSettings;
use viewer_core::{Core, CoreConfig, CoreError, StreamTier};

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/synthetic")
        .join(name)
}

fn mock(sim: &SimRuntime, audio: &str, channels: u8) -> mock_nvr::MockNvr {
    let video = mock_nvr::fixture::Fixture::load(&fixture_path("h264-basic.h264")).unwrap();
    let mut cfg = mock_nvr::Config::new(video);
    cfg.channels = channels;
    let mut a = mock_nvr::audio::AudioConfig::new(
        mock_nvr::audio::AudioFixture::load(&fixture_path(audio)).unwrap(),
    );
    a.jitter_us = 30_000;
    cfg.audio = Some(Arc::new(a));
    sim_spawn(sim, cfg)
}

fn sound_events(rec: &Recorder) -> Vec<Value> {
    rec.events_of("detection")
        .into_iter()
        .filter(|e| e["source"] == "local_audio")
        .collect()
}

fn of_channel(rec: &Recorder, channel: u64) -> Vec<Value> {
    sound_events(rec)
        .into_iter()
        .filter(|e| e["channel"] == channel)
        .collect()
}

fn setup(
    sim: &SimRuntime,
    audio: &str,
    channels: u8,
    config: CoreConfig,
) -> (mock_nvr::MockNvr, Arc<Core>, Arc<Recorder>, u64) {
    let nvr = mock(sim, audio, channels);
    let (core, rec) = start(on_sim(sim, config));
    let dev = core
        .add_device("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
        .unwrap();
    (nvr, core, rec, dev)
}

/// An alarm on two channels: one start per channel however many tiles show
/// it, audible (and muted) or silent alike; the stop comes when the last
/// tile of the channel closes; loudness is kept per tile.
#[test]
fn smoke_alarm_is_detected_once_per_channel_and_stops_with_the_tiles() {
    let sim = SimRuntime::new();
    // The core's wall clock starts at the real time and moves with the
    // simulated clock.
    let (wall0, t0) = (SystemTime::now(), sim.now());
    let (_nvr, core, rec, dev) = setup(&sim, "aac-smoke-t3.aac", 2, CoreConfig::default());
    let opened_ns = core.clock_now_ns();
    core.set_audio_output(48_000, 2).unwrap();
    let main0 = core.open_tile(dev, 0, StreamTier::Main).unwrap();
    let sub0 = core.open_tile(dev, 0, StreamTier::Sub).unwrap();
    let main1 = core.open_tile(dev, 1, StreamTier::Main).unwrap();
    // Audible but muted; the others stay silent.
    core.set_tile_audio(main0, true, 0.0).unwrap();

    sim_until(&sim, &core, "an alarm start on both channels", 30, || {
        !of_channel(&rec, 0).is_empty() && !of_channel(&rec, 1).is_empty()
    });
    let started_ns = core.clock_now_ns();
    // Two cycles are needed (8 s of the fixture), plus start-up.
    for ch in [0, 1] {
        let e = of_channel(&rec, ch);
        println!("{e:?}");
        assert_eq!(e.len(), 1, "one start for channel {ch}: {e:?}");
        let e = &e[0];
        assert_eq!(e["device"], dev);
        assert_eq!(e["kind"], "smoke_alarm");
        assert_eq!(e["state"], "start");
        assert!(e.get("lens").is_none());
        let at = e["at_ns"].as_u64().unwrap();
        assert!(
            at >= opened_ns && at <= started_ns,
            "{at} {opened_ns} {started_ns}"
        );
    }

    // Loudness: seconds with the tone at about -10 dBFS, as Unix seconds.
    let l = core.tile_loudness(sub0).unwrap();
    println!("{l:?}");
    assert!(l.peak.len() >= 5 && l.peak.len() == l.rms.len());
    assert!(
        l.peak
            .iter()
            .any(|p| p.db().is_some_and(|d| (-13..=-7).contains(&d)))
    );
    let now_s = (wall0 + (sim.now() - t0))
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let newest = l.start_s + l.peak.len() as u64 - 1;
    assert!(
        newest + 3 >= now_s && newest <= now_s + 1,
        "{newest} {now_s}"
    );
    assert_eq!(core.tile_loudness(9999), Err(CoreError::UnknownId));

    // Closing one of channel 0's tiles stops nothing; the last one does,
    // before its `tile_closed`.
    core.close_tile(main0).unwrap();
    sim.advance(Duration::from_millis(300));
    core.flush_events();
    assert_eq!(of_channel(&rec, 0).len(), 1);
    core.close_tile(sub0).unwrap();
    // Both events, not just the stop: the order is checked below.
    sim_until(
        &sim,
        &core,
        "channel 0 stops and its tile closes",
        5,
        || {
            of_channel(&rec, 0).len() == 2
                && rec
                    .events_of("tile_closed")
                    .iter()
                    .any(|e| e["tile"] == sub0)
        },
    );
    let stop = &of_channel(&rec, 0)[1];
    assert_eq!(stop["state"], "stop");
    assert_eq!(stop["kind"], "smoke_alarm");
    assert_eq!(stop["source"], "local_audio");
    let order: Vec<String> = rec
        .timeline()
        .into_iter()
        .filter_map(|t| match t {
            common::Entry::Event(e)
                if (e["type"] == "detection" && e["channel"] == 0 && e["state"] == "stop")
                    || (e["type"] == "tile_closed" && e["tile"] == sub0) =>
            {
                Some(e["type"].as_str().unwrap().to_string())
            }
            _ => None,
        })
        .collect();
    assert_eq!(order, ["detection", "tile_closed"]);

    // The device going away ends channel 1's tile, and its episode.
    core.remove_device(dev).unwrap();
    sim_until(&sim, &core, "channel 1 stops", 5, || {
        of_channel(&rec, 1).len() == 2
    });
    assert_eq!(of_channel(&rec, 1)[1]["state"], "stop");
    let _ = main1;
    sim.run_free(|| core.shutdown());
    assert_eq!(sound_events(&rec).len(), 4);
}

/// A steady signal is loud but no alarm.
#[test]
fn steady_tone_reports_loudness_but_no_alarm() {
    let sim = SimRuntime::new();
    let (_nvr, core, rec, dev) = setup(&sim, "aac-tone.aac", 1, CoreConfig::default());
    let tile = core.open_tile(dev, 0, StreamTier::Main).unwrap();
    sim.advance(Duration::from_secs(14));
    let l = core.tile_loudness(tile).unwrap();
    assert!(l.peak.len() >= 10, "{l:?}");
    assert!(l.peak.iter().filter(|p| p.is_some()).count() >= 10);
    core.close_tile(tile).unwrap();
    sim.run_free(|| core.shutdown());
    assert!(sound_events(&rec).is_empty(), "{:?}", sound_events(&rec));
}

/// With alarms switched off in `CoreConfig`, the alarm goes unreported;
/// with loudness off too, no history is kept.
#[test]
fn alarms_can_be_switched_off() {
    let config = CoreConfig {
        sound: SoundSettings {
            alarms: false,
            loudness_history_s: 0,
        },
        ..CoreConfig::default()
    };
    let sim = SimRuntime::new();
    let (_nvr, core, rec, dev) = setup(&sim, "aac-smoke-t3.aac", 1, config);
    let tile = core.open_tile(dev, 0, StreamTier::Main).unwrap();
    sim.advance(Duration::from_secs(14));
    core.flush_events();
    assert!(
        rec.events_of("audio_stats")
            .iter()
            .any(|e| e["decoded"].as_u64() > Some(100))
    );
    assert_eq!(core.tile_loudness(tile).unwrap().peak.len(), 0);
    sim.run_free(|| core.shutdown());
    assert!(sound_events(&rec).is_empty());
}
