//! Accelerated soak harness (docs/ARCHITECTURE.md, goal 6).
//!
//! Two legs run side by side on the same replayed stream:
//!
//! - **Video and session** (wall clock): an in-process `mock-nvr` serves the
//!   stream at N times its recorded speed to a real `viewer_core::Core`,
//!   whose platform decoder is a fake that answers every `Decode` at once.
//!   Counts frames, drops, gating, and resets, and samples process memory
//!   over the run to judge growth.
//! - **Audio** ([`audio_sim`]): the same records through
//!   `audio_pipeline::TileAudio` in simulated time, reporting the range of
//!   its filtered A/V error.
//!
//! Timestamps in the stream are never changed by the speed; only how fast the
//! server sends them. Speeds high enough to outrun the tile queue show up as
//! queue drops, which is itself a measurement.

pub mod audio_sim;
pub mod memory;
pub mod synth;

use mock_nvr::fixture::Fixture;
use mock_nvr::replay::{Replay, ReplayConfig};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};
use viewer_core::{Action, Callbacks, Core, CoreConfig, StreamTier};

/// Settings of a run.
#[derive(Clone, Debug)]
pub struct SoakConfig {
    /// The stream every tile replays.
    pub replay: Arc<Replay>,
    /// Speed factor (60.0: one simulated minute per wall second).
    pub speed: f64,
    /// Simulated duration of the run.
    pub simulated: Duration,
    /// Tiles (each on its own channel, sub stream).
    pub tiles: u8,
    /// Time between memory samples (wall clock).
    pub sample_every: Duration,
    /// Run the audio leg (simulated time, no wall-clock cost beyond CPU).
    pub audio: bool,
}

impl SoakConfig {
    /// One tile, 10-second samples, audio on.
    pub fn new(replay: Replay, speed: f64, simulated: Duration) -> SoakConfig {
        SoakConfig {
            replay: Arc::new(replay),
            speed,
            simulated,
            tiles: 1,
            sample_every: Duration::from_secs(10),
            audio: true,
        }
    }
}

/// One memory sample.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MemSample {
    /// Simulated seconds since the run started.
    pub sim_s: f64,
    /// Process memory, bytes.
    pub bytes: u64,
    /// Pictures delivered to the fake decoder so far (all tiles).
    pub decoded: u64,
}

/// Counters of the latest `tile_stats` of every tile, summed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VideoTotals {
    /// Access units that reached the pipeline.
    pub received: u64,
    /// Pictures the decoder reported decoded.
    pub decoded: u64,
    /// Pictures discarded while waiting for a keyframe.
    pub gated: u64,
    /// Pictures dropped (queue overflow or backpressure).
    pub dropped: u64,
    /// Decode failures reported by the platform.
    pub failures: u64,
    /// Decoder resets.
    pub resets: u64,
    /// Audio records that reached the tile.
    pub audio_frames: u64,
}

/// Result of a run.
#[derive(Clone, Debug)]
pub struct Report {
    /// Settings echoed.
    pub speed: f64,
    /// Simulated duration asked for.
    pub simulated: Duration,
    /// Wall time the video leg took.
    pub wall: Duration,
    /// Tiles.
    pub tiles: u8,
    /// Latest per-tile counters, summed.
    pub video: VideoTotals,
    /// `Decode` actions the fake decoder received (all tiles).
    pub decode_actions: u64,
    /// `Configure`, `Flush`, `Reset` actions received.
    pub configures: u64,
    /// Flush actions.
    pub flushes: u64,
    /// Reset actions.
    pub resets_seen: u64,
    /// Access units the stream offered during the run, estimated from the
    /// recording (video records per pass times passes elapsed), per tile.
    pub offered_per_tile: f64,
    /// Events other than `tile_stats`, by type.
    pub events: Vec<(String, u64)>,
    /// Events dropped because the event consumer was slow.
    pub dropped_events: u64,
    /// Memory samples over the run.
    pub memory: Vec<MemSample>,
    /// Growth verdict.
    pub trend: Option<memory::Trend>,
    /// The audio leg.
    pub audio: Option<audio_sim::AudioReport>,
    /// Queue high-water marks: not exposed by viewer-core today.
    pub queue_high_water: Option<u64>,
}

#[derive(Default)]
struct Fake {
    core: OnceLock<Weak<Core>>,
    decodes: AtomicU64,
    configures: AtomicU64,
    flushes: AtomicU64,
    resets: AtomicU64,
    stats: Mutex<HashMap<u64, VideoTotals>>,
    events: Mutex<HashMap<String, u64>>,
}

impl Callbacks for Fake {
    fn on_action(&self, tile: u64, generation: u32, action: &Action<'_>) {
        match action {
            Action::Decode { pts, .. } => {
                self.decodes.fetch_add(1, Ordering::Relaxed);
                if let Some(core) = self.core.get().and_then(Weak::upgrade) {
                    let _ = core.decoded(tile, generation, *pts);
                }
            }
            Action::Configure { .. } => {
                self.configures.fetch_add(1, Ordering::Relaxed);
            }
            Action::Flush => {
                self.flushes.fetch_add(1, Ordering::Relaxed);
            }
            Action::Reset => {
                self.resets.fetch_add(1, Ordering::Relaxed);
            }
            Action::Clear | Action::Status(_) => {}
        }
    }

    fn on_event(&self, json: &str) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
            return;
        };
        let kind = v["type"].as_str().unwrap_or("?").to_owned();
        if kind == "tile_stats" {
            let n = |k: &str| v[k].as_u64().unwrap_or(0);
            let t = VideoTotals {
                received: n("received"),
                decoded: n("decoded"),
                gated: n("gated"),
                dropped: n("dropped"),
                failures: n("failures"),
                resets: n("resets"),
                audio_frames: n("audio_frames"),
            };
            if let Some(tile) = v["tile"].as_u64() {
                self.stats.lock().unwrap().insert(tile, t);
            }
        } else {
            *self.events.lock().unwrap().entry(kind).or_default() += 1;
        }
    }
}

/// Runs the soak.
///
/// # Errors
/// Setup failures (the mock cannot listen, the core cannot start).
pub fn run(cfg: &SoakConfig) -> Result<Report, String> {
    if !(cfg.speed.is_finite() && cfg.speed > 0.0) {
        return Err("speed must be positive".into());
    }
    let fixture =
        Fixture::load(&synth::fixtures_dir().join("h264-basic.h264")).map_err(|e| e.to_string())?;
    let mut mc = mock_nvr::Config::new(fixture);
    mc.channels = cfg.tiles.max(1);
    mc.max_logins = 4;
    if let Some((w, h, f)) = cfg.replay.info() {
        (mc.width, mc.height, mc.fps) = (w, h, u32::from(f));
    }
    mc.replay = Some(ReplayConfig {
        replay: Arc::clone(&cfg.replay),
        speed: cfg.speed,
        looped: true,
    });
    let nvr = mock_nvr::spawn(mc).map_err(|e| format!("mock cannot listen: {e}"))?;
    let addr = nvr.addr();

    let fake = Arc::new(Fake::default());
    let core_cfg = CoreConfig {
        stats_interval: Duration::from_millis(500),
        ..CoreConfig::default()
    };
    let core = Arc::new(Core::new(core_cfg, fake.clone()).map_err(|e| e.to_string())?);
    fake.core
        .set(Arc::downgrade(&core))
        .map_err(|_| "core already set")?;
    let device = core
        .add_device(
            &addr.ip().to_string(),
            addr.port(),
            "example-user",
            "example-password",
        )
        .map_err(|e| e.to_string())?;
    let mut tiles = Vec::new();
    for ch in 0..cfg.tiles.max(1) {
        tiles.push(
            core.open_tile(device, ch, StreamTier::Sub)
                .map_err(|e| e.to_string())?,
        );
    }

    let wall_target = Duration::from_secs_f64(cfg.simulated.as_secs_f64() / cfg.speed);
    let start = Instant::now();
    let mut memory = Vec::new();
    let mut next = Duration::ZERO;
    loop {
        let el = start.elapsed();
        if el >= next {
            if let Some(bytes) = memory::current_bytes() {
                memory.push(MemSample {
                    sim_s: el.as_secs_f64() * cfg.speed,
                    bytes,
                    decoded: fake.decodes.load(Ordering::Relaxed),
                });
            }
            next += cfg.sample_every;
        }
        if el >= wall_target {
            break;
        }
        std::thread::sleep(
            (wall_target - el)
                .min(next.saturating_sub(el))
                .min(Duration::from_millis(200))
                .max(Duration::from_millis(1)),
        );
    }
    let wall = start.elapsed();
    let decode_actions = fake.decodes.load(Ordering::Relaxed);
    // Let the last stats events arrive, then tear down in the same order an
    // app would.
    std::thread::sleep(Duration::from_millis(700));
    let video = fake
        .stats
        .lock()
        .unwrap()
        .values()
        .fold(VideoTotals::default(), |a, t| VideoTotals {
            received: a.received + t.received,
            decoded: a.decoded + t.decoded,
            gated: a.gated + t.gated,
            dropped: a.dropped + t.dropped,
            failures: a.failures + t.failures,
            resets: a.resets + t.resets,
            audio_frames: a.audio_frames + t.audio_frames,
        });
    for t in tiles {
        let _ = core.close_tile(t);
    }
    let _ = core.remove_device(device);
    let dropped_events = core.dropped_events();
    core.shutdown();
    nvr.shutdown();

    // The audio leg uses no wall clock; it runs after the video leg so it
    // cannot disturb its timing or memory samples.
    let audio = cfg
        .audio
        .then(|| audio_sim::simulate(&cfg.replay, cfg.simulated.as_micros() as u64));

    // The stream the tiles play (an `NVRREC02` recording's default tier).
    let videos_per_pass = cfg
        .replay
        .playback(false)
        .filter(|(_, step)| {
            matches!(step, mock_nvr::replay::Step::Record(bytes)
                if bytes.len() >= 24 && &bytes[2..4] == b"dc")
        })
        .count() as f64;
    let passes = wall.as_secs_f64() * cfg.speed * 1e6 / cfg.replay.pass_us() as f64;
    let mut events: Vec<_> = fake.events.lock().unwrap().clone().into_iter().collect();
    events.sort();
    let series: Vec<(f64, u64)> = memory.iter().map(|m| (m.sim_s, m.bytes)).collect();
    Ok(Report {
        speed: cfg.speed,
        simulated: cfg.simulated,
        wall,
        tiles: cfg.tiles.max(1),
        video,
        decode_actions,
        configures: fake.configures.load(Ordering::Relaxed),
        flushes: fake.flushes.load(Ordering::Relaxed),
        resets_seen: fake.resets.load(Ordering::Relaxed),
        offered_per_tile: videos_per_pass * passes,
        events,
        dropped_events,
        trend: memory::trend(&series),
        memory,
        audio,
        queue_high_water: None,
    })
}

/// Loads a recording for [`SoakConfig::new`].
pub fn load(path: &Path) -> Result<Replay, String> {
    Replay::load(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn mb(b: f64) -> f64 {
    b / (1024.0 * 1024.0)
}

impl Report {
    /// Aggregate numbers as Markdown.
    pub fn to_markdown(&self) -> String {
        let mut s = String::new();
        let sim_h = self.simulated.as_secs_f64() / 3600.0;
        let _ = writeln!(
            s,
            "## Soak: {sim_h:.2} h simulated at {}x in {:.1} s wall, {} tile(s)\n",
            self.speed,
            self.wall.as_secs_f64(),
            self.tiles
        );
        let v = &self.video;
        let wall = self.wall.as_secs_f64().max(1e-9);
        let _ = writeln!(s, "### Video and session (wall clock)\n");
        let _ = writeln!(s, "| measure | value |\n| --- | --- |");
        let offered = self.offered_per_tile * f64::from(self.tiles);
        let _ = writeln!(s, "| access units offered (estimate) | {offered:.0} |");
        let _ = writeln!(
            s,
            "| received by the pipeline (final report, 0.7 s after the run) | {} |",
            v.received
        );
        let _ = writeln!(s, "| decoded (final report) | {} |", v.decoded);
        let _ = writeln!(
            s,
            "| decode actions at the fake decoder (when the run ended) | {} |",
            self.decode_actions
        );
        let _ = writeln!(
            s,
            "| delivered per wall second | {:.0} |",
            self.decode_actions as f64 / wall
        );
        let _ = writeln!(s, "| dropped | {} |", v.dropped);
        let _ = writeln!(s, "| gated (waiting for a keyframe) | {} |", v.gated);
        let _ = writeln!(s, "| failures | {} |", v.failures);
        let _ = writeln!(
            s,
            "| resets (stats / actions) | {} / {} |",
            v.resets, self.resets_seen
        );
        let _ = writeln!(
            s,
            "| configures / flushes | {} / {} |",
            self.configures, self.flushes
        );
        let _ = writeln!(s, "| audio records at the tiles | {} |", v.audio_frames);
        let _ = writeln!(
            s,
            "| events dropped by the core | {} |",
            self.dropped_events
        );
        let _ = writeln!(s, "| queue high-water mark | not exposed by viewer-core |");
        if !self.events.is_empty() {
            let e: Vec<_> = self
                .events
                .iter()
                .map(|(k, n)| format!("{k} x{n}"))
                .collect();
            let _ = writeln!(s, "| other events | {} |", e.join(", "));
        }
        let _ = writeln!(s, "\n### Process memory\n");
        match (self.memory.first(), self.memory.last()) {
            (Some(a), Some(b)) => {
                let max = self.memory.iter().map(|m| m.bytes).max().unwrap_or(0);
                let _ = writeln!(
                    s,
                    "{} samples: first {:.1} MB, last {:.1} MB, peak {:.1} MB.",
                    self.memory.len(),
                    mb(a.bytes as f64),
                    mb(b.bytes as f64),
                    mb(max as f64)
                );
            }
            _ => {
                let _ = writeln!(s, "not available on this platform.");
            }
        }
        match &self.trend {
            Some(t) => {
                let _ = writeln!(
                    s,
                    "\nTrend after a quarter warm-up ({} samples): {:+.2} MB per simulated hour, \
                     {:+.1} MB implied over the fitted span. Verdict: **{}**.",
                    t.samples,
                    mb(t.slope_bytes_per_hour),
                    mb(t.implied_growth_bytes),
                    if t.stable { "no growth" } else { "GROWING" }
                );
            }
            None => {
                let _ = writeln!(s, "\nNo trend verdict (fewer than 8 samples).");
            }
        }
        if let Some(a) = &self.audio {
            let _ = writeln!(s, "\n### Audio (simulated time, recorded arrival jitter)\n");
            let _ = writeln!(s, "| measure | value |\n| --- | --- |");
            let _ = writeln!(s, "| simulated | {:.2} h |", a.simulated_us as f64 / 3.6e9);
            let _ = writeln!(
                s,
                "| records (decoded / concealed / rejected) | {} ({} / {} / {}) |",
                a.records, a.decoded, a.concealed, a.rejected
            );
            let _ = writeln!(
                s,
                "| filtered A/V error after each start's first minute: min / mean / max | {:.1} / {:.1} / {:.1} ms ({} samples) |",
                a.offset_min_us / 1e3,
                a.offset_mean_us / 1e3,
                a.offset_max_us / 1e3,
                a.offset_samples
            );
            let _ = writeln!(
                s,
                "| min and max occurred at stream time | {:.0} s and {:.0} s |",
                a.offset_min_at_s, a.offset_max_at_s
            );
            let _ = writeln!(
                s,
                "| largest absolute error during warm-up | {:.1} ms |",
                a.warmup_abs_max_us / 1e3
            );
            let _ = writeln!(
                s,
                "| resyncs / starts / underruns | {} / {} / {} |",
                a.resyncs, a.starts, a.underruns
            );
            let _ = writeln!(s, "| ring overflow frames | {} |", a.overflow_frames);
            let _ = writeln!(
                s,
                "| loops of the recording (treated as restarts) | {} |",
                a.restarts
            );
            let _ = writeln!(s, "| timestamp epochs | {} |", a.epoch);
            if let Some(r) = a.rate_estimate {
                let _ = writeln!(s, "| final sample-rate estimate | {r:.1} Hz |");
            }
            if !a.hourly_us.is_empty() {
                let h: Vec<_> = a
                    .hourly_us
                    .iter()
                    .map(|o| format!("{:.0}", o / 1e3))
                    .collect();
                let _ = writeln!(
                    s,
                    "| error at each simulated hour (ms) | {} |",
                    h.join(", ")
                );
            }
        }
        s
    }

    /// The memory series as CSV (`sim_seconds,bytes,decoded`).
    pub fn memory_csv(&self) -> String {
        let mut s = String::from("sim_seconds,bytes,decoded\n");
        for m in &self.memory {
            let _ = writeln!(s, "{:.1},{},{}", m.sim_s, m.bytes, m.decoded);
        }
        s
    }
}
