//! Second lenses found by a short `externStream` preview (docs/protocol.md,
//! "Second lenses"; docs/FFI.md, `channels`), against the mock, on a
//! simulated clock: probe delays, quiet periods, and probe windows take
//! simulated time.

use crate::common::*;
use mock_nvr::ExternMode;
use simtime::SimRuntime;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use viewer_core::{Core, CoreConfig, StreamTier};

/// ch0 lists `externStream`, ch1 hides a second lens, ch2 answers with a
/// stream-info record only, ch3 refuses (400), ch4 never answers, ch5 is a
/// multi-lens camera the capabilities reveal.
fn lens_mock(
    sim: &SimRuntime,
    bind: Option<SocketAddr>,
    ch1_uid: &str,
    ch1: ExternMode,
) -> mock_nvr::MockNvr {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/h264-basic.h264");
    let mut cfg = mock_nvr::Config::new(mock_nvr::fixture::Fixture::load(&path).unwrap());
    if let Some(b) = bind {
        cfg.bind = b;
    }
    cfg.channels = 6;
    cfg.extern_modes = vec![
        ExternMode::Listed,
        ch1,
        ExternMode::InfoOnly,
        ExternMode::Refused,
        ExternMode::Silent,
        ExternMode::Hidden,
    ];
    cfg.lens_aitypes = vec![vec![], vec![], vec![], vec![], vec![], vec![3, 17]];
    cfg.uids = (0..6).map(|c| format!("UID-EXAMPLE-{c}")).collect();
    cfg.uids[1] = ch1_uid.into();
    sim_spawn(sim, cfg)
}

fn core_in(sim: &SimRuntime, dir: &Path) -> (Arc<Core>, Arc<Recorder>) {
    start(on_sim(
        sim,
        CoreConfig {
            data_dir: Some(dir.to_owned()),
            ..fast_config()
        },
    ))
}

fn add(core: &Core, nvr: &mock_nvr::MockNvr) -> u64 {
    core.add_device("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
        .unwrap()
}

/// The latest `channels` event's entry for `ch`.
fn channel(rec: &Recorder, ch: usize) -> serde_json::Value {
    rec.events_of("channels")
        .last()
        .map(|e| e["channels"][ch].clone())
        .unwrap_or_default()
}

fn stored(dir: &Path) -> serde_json::Value {
    std::fs::read(dir.join("lenses.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Waits until every probe has run: each answered channel is in the file.
fn wait_stored(sim: &SimRuntime, core: &Core, dir: &Path, channels: &[&str]) -> serde_json::Value {
    let mut v = serde_json::Value::Null;
    sim_until(sim, core, "probe answers stored", 20, || {
        v = stored(dir);
        let devs = v["devices"].as_object().cloned().unwrap_or_default();
        devs.values()
            .any(|d| channels.iter().all(|c| d.get(*c).is_some()))
    });
    v
}

#[test]
fn probes_find_hidden_lenses_once_and_remember_them() {
    let sim = SimRuntime::new();
    let dir = tempfile::tempdir().unwrap();
    let nvr = lens_mock(&sim, None, "UID-EXAMPLE-1", ExternMode::Hidden);
    let addr = nvr.addr();
    let (core, rec) = core_in(&sim, dir.path());
    let dev = add(&core, &nvr);
    let t0 = sim.now();
    // Never two probes at once: sample the mock's open previews meanwhile.
    let mut most = 0;
    let mut v = serde_json::Value::Null;
    while sim.now() - t0 < Duration::from_secs(20) {
        most = most.max(nvr.command_count(3) - nvr.command_count(4).min(nvr.command_count(3)));
        v = stored(dir.path());
        if v["devices"].as_object().is_some_and(|d| {
            d.values()
                .any(|c| ["1", "2", "3", "4"].iter().all(|k| c.get(*k).is_some()))
        }) {
            break;
        }
        sim.advance(Duration::from_millis(5));
    }
    assert!(most <= 1, "{most} previews open at once");
    let key = core.device_key(dev).unwrap();
    let chans = &v["devices"][&key];
    assert_eq!(chans["1"]["lenses"], 2, "{v}");
    assert_eq!(chans["1"]["uid"], "UID-EXAMPLE-1");
    assert_eq!(chans["1"]["model"], "MOCK-CAM");
    for ch in ["2", "3", "4"] {
        assert_eq!(chans[ch]["lenses"], 1, "channel {ch}: {v}");
    }
    // Not probed: the channel listing `externStream`, and the multi-lens
    // camera the capabilities reveal.
    assert!(chans.get("0").is_none() && chans.get("5").is_none(), "{v}");
    sim_until(&sim, &core, "lenses in channels", 5, || {
        channel(&rec, 1)["lenses"] == 2
    });
    let c1 = channel(&rec, 1);
    assert!(c1.get("lens_detects").is_none(), "{c1}");
    assert_eq!(c1["tiers"], serde_json::json!(["main", "sub"]));
    for ch in [0, 2, 3, 4] {
        assert!(channel(&rec, ch).get("lenses").is_none(), "{ch}");
    }
    assert_eq!(channel(&rec, 5)["lenses"], 2);
    assert!(channel(&rec, 5).get("lens_detects").is_some());
    // One start per probe, each stopped, on the one login.
    assert_eq!(nvr.command_count(3), 4);
    sim_until(&sim, &core, "every probe stopped", 5, || {
        nvr.command_count(4) == nvr.command_count(3)
    });
    assert_eq!(nvr.active_logins(), 1);
    sim.run_free(|| core.shutdown());
    nvr.shutdown();

    // The next run: the answers come from the file, no probe at all.
    let nvr = lens_mock(&sim, Some(addr), "UID-EXAMPLE-1", ExternMode::Hidden);
    let (core, rec) = core_in(&sim, dir.path());
    let dev2 = add(&core, &nvr);
    assert_eq!(core.device_key(dev2).unwrap(), key);
    sim_until(&sim, &core, "cached lens", 10, || {
        channel(&rec, 1)["lenses"] == 2
    });
    sim.advance(Duration::from_secs(3));
    assert_eq!(nvr.command_count(3), 0, "a cached answer needs no probe");
    sim.run_free(|| core.shutdown());
    nvr.shutdown();

    // Another camera on channel 1 (a new uid), without a second lens: it is
    // probed again, and the lens is gone.
    let nvr = lens_mock(&sim, Some(addr), "UID-EXAMPLE-9", ExternMode::InfoOnly);
    let (core, rec) = core_in(&sim, dir.path());
    add(&core, &nvr);
    sim_until(&sim, &core, "channel 1 probed again", 15, || {
        stored(dir.path())["devices"][&key]["1"]["uid"] == "UID-EXAMPLE-9"
    });
    assert_eq!(stored(dir.path())["devices"][&key]["1"]["lenses"], 1);
    assert_eq!(nvr.command_count(3), 1);
    assert!(channel(&rec, 1).get("lenses").is_none());
    sim_until(&sim, &core, "probe stopped", 5, || {
        nvr.command_count(4) == 1
    });
    sim.run_free(|| core.shutdown());
}

#[test]
fn a_tile_starting_on_the_channel_holds_the_probe_back() {
    let sim = SimRuntime::new();
    let dir = tempfile::tempdir().unwrap();
    let nvr = lens_mock(&sim, None, "UID-EXAMPLE-1", ExternMode::Hidden);
    let (core, rec) = core_in(&sim, dir.path());
    let dev = add(&core, &nvr);
    // A tile on the hidden-lens channel, opened as soon as the slot list
    // arrives (as an app restoring its layout would).
    sim_until(&sim, &core, "slot list", 10, || {
        !rec.events_of("channels").is_empty()
    });
    let opened = sim.now();
    let t = core.open_tile(dev, 1, StreamTier::Sub).unwrap();
    sim_until(&sim, &core, "tile decodes", 10, || rec.decodes(t) > 0);
    wait_stored(&sim, &core, dir.path(), &["1"]);
    let found = sim.now() - opened;
    assert!(
        found >= Duration::from_millis(2900),
        "probed {found:?} after the tile started"
    );
    // The tile kept playing throughout.
    assert!(rec.decodes(t) > 20);
    sim.run_free(|| core.shutdown());
}

#[test]
fn a_tier_that_sends_only_a_0x0_info_record_ends_as_a_rejected_start() {
    let sim = SimRuntime::new();
    let dir = tempfile::tempdir().unwrap();
    let nvr = lens_mock(&sim, None, "UID-EXAMPLE-1", ExternMode::Hidden);
    let (core, rec) = start(on_sim(
        &sim,
        CoreConfig {
            data_dir: Some(dir.path().to_owned()),
            info_only_limit: Duration::from_secs(5),
            ..fast_config()
        },
    ));
    let dev = add(&core, &nvr);
    sim_until(&sim, &core, "slot list", 10, || {
        !rec.events_of("channels").is_empty()
    });
    // Channel 2 answers its extern preview with a 0x0 info record only;
    // channel 1's extern tier is a second lens with video.
    let info_only = core.open_tile(dev, 2, StreamTier::Extern).unwrap();
    let lens = core.open_tile(dev, 1, StreamTier::Extern).unwrap();
    let t0 = sim.now();
    let rejected = |tile: u64| {
        rec.events_of("tile_error")
            .iter()
            .any(|e| e["tile"] == tile && e["reason"] == "start_rejected" && e["status"] == 0)
    };
    sim_until(&sim, &core, "the failed start", 30, || rejected(info_only));
    let waited = sim.now() - t0;
    assert!(waited >= Duration::from_secs(5), "failed after {waited:?}");
    assert!(waited < Duration::from_secs(7), "failed after {waited:?}");
    assert_eq!(rec.decodes(info_only), 0);
    // Reported once, and never for the tier that has video.
    sim.advance(Duration::from_secs(10));
    core.flush_events();
    let errors = rec
        .events_of("tile_error")
        .iter()
        .filter(|e| e["tile"] == info_only)
        .count();
    assert_eq!(errors, 1);
    assert!(rec.decodes(lens) > 0);
    assert!(!rejected(lens));
    sim.run_free(|| core.shutdown());
}
