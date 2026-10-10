//! Rendering with an imperfect host clock, as some apps drive the renderer:
//! host times that jitter by milliseconds per call, or a timer loop that
//! passes `now + 10 ms` and sleeps 10 ms (so its calls come a little more
//! than 10 ms apart while each renders 10 ms of frames). Neither may cause
//! resyncs or silence (docs/AUDIO.md).
//!
//! On a simulated clock (`simtime`): the sleeping loop's overshoot is
//! modeled (every fourth sleep lasts 11 ms instead of 10) rather than left
//! to the machine, and the jitter comes from a seeded generator, so each
//! run renders exactly the same calls.

mod common;

use common::{PASSWORD, USER, on_sim, sim_spawn, sim_until};
use simtime::SimRuntime;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;
use viewer_core::{Action, Callbacks, Core, CoreConfig, StreamTier};

const THRESHOLD: f32 = 0.4;

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/synthetic")
        .join(name)
}

fn mock(sim: &SimRuntime) -> mock_nvr::MockNvr {
    let video = mock_nvr::fixture::Fixture::load(&fixture_path("h264-basic.h264")).unwrap();
    let mut cfg = mock_nvr::Config::new(video);
    cfg.channels = 1;
    let mut a = mock_nvr::audio::AudioConfig::new(
        mock_nvr::audio::AudioFixture::load(&fixture_path("aac-clicks.aac")).unwrap(),
    );
    a.jitter_us = 30_000;
    a.stream_index = 5;
    cfg.audio = Some(Arc::new(a));
    sim_spawn(sim, cfg)
}

#[derive(Default)]
struct Rec {
    core: OnceLock<Weak<Core>>,
    events: Mutex<Vec<serde_json::Value>>,
}

impl Callbacks for Rec {
    fn on_action(&self, tile: u64, generation: u32, action: &Action<'_>) {
        if let Action::Decode { pts, .. } = *action
            && let Some(c) = self.core.get().and_then(Weak::upgrade)
        {
            let _ = c.decoded(tile, generation, pts);
        }
    }

    fn on_event(&self, json: &str) {
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        self.events.lock().unwrap().push(v);
    }
}

impl Rec {
    fn last_audio_stats(&self, tile: u64) -> Option<serde_json::Value> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|e| e["type"] == "audio_stats" && e["tile"] == tile)
            .cloned()
    }
}

#[derive(Clone, Copy, Debug)]
enum HostClock {
    /// `now + 10 ms`, then sleep 10 ms (like the C smoke test); a sleep
    /// overshoots, here by 1 ms every fourth call.
    Sleep,
    /// An exact 10 ms schedule, passed with up to ±5 ms of error.
    Jitter,
}

/// Renders 480 frames of 48 kHz stereo per call for `seconds`; returns the
/// highest level read for `tile` in the last `level_s` seconds and the
/// longest stretch (s) the level stayed below the threshold after audio
/// first showed.
fn render(sim: &SimRuntime, core: &Core, tile: u64, how: HostClock, seconds: f64) -> (f32, f64) {
    let mut buf = vec![0.0f32; 480 * 2];
    let t0 = sim.now();
    let start = core.clock_now_ns() + 10_000_000;
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let (mut max, mut quiet_since, mut longest) = (0.0f32, None::<Duration>, 0.0f64);
    let mut heard = false;
    let mut k = 0u64;
    let elapsed = || (sim.now() - t0).as_secs_f64();
    while elapsed() < seconds {
        let host = match how {
            HostClock::Sleep => core.clock_now_ns() + 10_000_000,
            HostClock::Jitter => {
                let ideal = start + k * 10_000_000;
                let now = core.clock_now_ns();
                if now + 10_000_000 < ideal {
                    sim.advance(Duration::from_nanos(ideal - 10_000_000 - now));
                }
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let dev = (seed % 10_000_001) as i64 - 5_000_000;
                ideal.saturating_add_signed(dev)
            }
        };
        let _ = core.render_audio(&mut buf, 2, host);
        let (peak, _) = core.tile_audio_level(tile);
        if peak > THRESHOLD {
            heard = true;
            quiet_since = None;
        } else if heard {
            let now = sim.now() - t0;
            let since = *quiet_since.get_or_insert(now);
            longest = longest.max((now - since).as_secs_f64());
        }
        if elapsed() > seconds - 3.0 {
            max = max.max(peak);
        }
        if matches!(how, HostClock::Sleep) {
            let overshoot = u64::from(k % 4 == 3);
            sim.advance(Duration::from_millis(10 + overshoot));
        }
        k += 1;
    }
    (max, longest)
}

#[test]
fn a_jittery_or_sleeping_host_clock_causes_no_resyncs() {
    for how in [HostClock::Sleep, HostClock::Jitter] {
        let sim = SimRuntime::new();
        let nvr = mock(&sim);
        let rec = Arc::new(Rec::default());
        let core = Arc::new(
            Core::new(
                on_sim(
                    &sim,
                    CoreConfig {
                        stats_interval: Duration::from_millis(200),
                        ..CoreConfig::default()
                    },
                ),
                rec.clone(),
            )
            .unwrap(),
        );
        rec.core.set(Arc::downgrade(&core)).unwrap();
        let dev = core
            .add_device("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
            .unwrap();
        let tile = core.open_tile(dev, 0, StreamTier::Main).unwrap();
        core.set_audio_output(48_000, 2).unwrap();
        core.set_tile_audio(tile, true, 1.0).unwrap();
        sim_until(&sim, &core, "audio decoded", 10, || {
            core.tile_audio_level(tile).0 > 0.0
        });
        let (max, longest_quiet) = render(&sim, &core, tile, how, 15.0);
        sim.advance(Duration::from_millis(300));
        core.flush_events();
        let stats = rec.last_audio_stats(tile).expect("audio_stats");
        println!("{how:?}: level {max:.2}, longest quiet {longest_quiet:.2} s; {stats}");
        // The first start is the only one: no resync, nothing not played.
        assert_eq!(stats["resyncs"], 0, "{how:?}: {stats}");
        assert_eq!(stats["starts"], 1, "{how:?}: {stats}");
        assert!(max > THRESHOLD, "{how:?}: level {max}");
        // The clicks in the fixture come well under 2 s apart.
        assert!(longest_quiet < 2.5, "{how:?}: quiet for {longest_quiet} s");
        sim.run_free(|| core.shutdown());
        nvr.shutdown();
    }
}
